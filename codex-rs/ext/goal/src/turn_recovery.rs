//! Continues unfinished root turns independently of goal lifecycle.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::Weak;

use codex_core::ThreadManager;
use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_core::TurnStartOptions;
use codex_core::context::ContextualUserFragment;
use codex_core::context::InternalContextSource;
use codex_core::context::InternalModelContextFragment;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ThreadIdleCause;
use codex_extension_api::ThreadIdleInput;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadStartInput;
use codex_extension_api::ThreadStopInput;
use codex_extension_api::TurnAbortInput;
use codex_extension_api::TurnErrorInput;
use codex_extension_api::TurnLifecycleContributor;
use codex_extension_api::TurnStartInput;
use codex_extension_api::TurnStopInput;
use codex_protocol::ThreadId;
use codex_protocol::protocol::CodexErrorInfo;
use codex_state::ThreadGoalStatus;

use crate::GoalRuntimeHandle;

const RECOVERY_TRIGGER: &str = "turn_recovery";
const RECOVERY_PROMPT: &str = "The previous turn ended because the server was overloaded. Continue only \
unfinished work under the existing instructions and permissions. Do not repeat completed \
actions or reopen a completed goal. If no unfinished work remains, report completion and stop.";

pub(crate) struct TurnRecovery {
    state_dbs: Arc<codex_state::StateRuntime>,
    thread_manager: Weak<ThreadManager>,
}

#[derive(Default)]
struct RecoveryState {
    turn_id: Option<String>,
    pending: Option<PendingRecovery>,
}

struct PendingRecovery {
    turn_id: String,
    goal: RecoveryGoal,
    start: TurnStartOptions,
}

#[derive(PartialEq, Eq)]
enum RecoveryGoal {
    Absent,
    Complete(String),
}

type ThreadRecoveryState = Mutex<RecoveryState>;

impl TurnRecovery {
    pub(crate) fn new(
        state_dbs: Arc<codex_state::StateRuntime>,
        thread_manager: Weak<ThreadManager>,
    ) -> Self {
        Self {
            state_dbs,
            thread_manager,
        }
    }

    async fn eligible_goal(&self, thread_id: ThreadId) -> Result<Option<RecoveryGoal>, String> {
        let goal = self
            .state_dbs
            .thread_goals()
            .get_thread_goal(thread_id)
            .await
            .map_err(|err| err.to_string())?;
        Ok(match goal {
            None => Some(RecoveryGoal::Absent),
            Some(goal) if goal.status == ThreadGoalStatus::Complete => {
                Some(RecoveryGoal::Complete(goal.goal_id))
            }
            Some(_) => None,
        })
    }
}

impl<C: Send + Sync + 'static> ThreadLifecycleContributor<C> for TurnRecovery {
    fn on_thread_start<'a>(&'a self, input: ThreadStartInput<'a, C>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            // Internal and delegated agents have their own scheduling/lifecycle owners.
            if !input.session_source.is_non_root_agent() {
                input
                    .thread_store
                    .get_or_init::<ThreadRecoveryState>(Default::default);
            }
        })
    }

    fn on_thread_stop<'a>(&'a self, input: ThreadStopInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            input.thread_store.remove::<ThreadRecoveryState>();
        })
    }

    fn on_thread_idle<'a>(&'a self, input: ThreadIdleInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Some(state) = input.thread_store.get::<ThreadRecoveryState>() else {
                return;
            };
            let pending = {
                let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                state.pending.take()
            };
            if input.cause != ThreadIdleCause::Failed {
                return;
            }
            let Some(pending) = pending else {
                return;
            };
            let Some(manager) = self.thread_manager.upgrade() else {
                return;
            };
            let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) else {
                return;
            };
            let Ok(thread) = manager.get_thread(thread_id).await else {
                return;
            };
            let runtime = input.thread_store.get::<GoalRuntimeHandle>();
            let _goal_permit = match runtime.as_ref() {
                Some(runtime) => match runtime.goal_state_permit().await {
                    Ok(permit) => Some(permit),
                    Err(err) => {
                        tracing::warn!("failed to lock goal state for turn recovery: {err}");
                        return;
                    }
                },
                None => None,
            };
            match self.eligible_goal(thread_id).await {
                Ok(Some(goal)) if goal == pending.goal => {}
                Ok(_) => return,
                Err(err) => {
                    tracing::warn!("failed to check goal state for turn recovery: {err}");
                    return;
                }
            }
            let item = ContextualUserFragment::into(InternalModelContextFragment::new(
                InternalContextSource::from_static(RECOVERY_TRIGGER),
                RECOVERY_PROMPT.to_string(),
            ));
            let request =
                TurnInputRequest::new(TurnInput::ResponseItem(item)).on_start(pending.start);
            match thread.continue_turn_if_idle(request, pending.turn_id).await {
                Ok(TurnInputSubmission::Started { .. }) => {}
                Ok(result) => tracing::debug!(?result, "turn recovery was not admitted"),
                Err(err) => tracing::warn!("failed to continue unfinished turn: {err}"),
            }
        })
    }
}

impl TurnLifecycleContributor for TurnRecovery {
    fn on_turn_start<'a>(&'a self, input: TurnStartInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Some(state) = input.thread_store.get::<ThreadRecoveryState>() else {
                return;
            };
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            *state = RecoveryState {
                turn_id: Some(input.turn_id.to_string()),
                pending: None,
            };
        })
    }

    fn on_turn_abort<'a>(&'a self, input: TurnAbortInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if let Some(state) = input.thread_store.get::<ThreadRecoveryState>() {
                let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                if state.turn_id.as_deref() == Some(input.turn_store.level_id())
                    || state
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.turn_id == input.turn_store.level_id())
                {
                    *state = RecoveryState::default();
                }
            }
        })
    }

    fn on_turn_stop<'a>(&'a self, input: TurnStopInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if let Some(state) = input.thread_store.get::<ThreadRecoveryState>() {
                let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                if state.turn_id.as_deref() == Some(input.turn_store.level_id()) {
                    state.turn_id = None;
                    if state.pending.is_none() {
                        *state = RecoveryState::default();
                    }
                }
            }
        })
    }

    fn on_turn_error<'a>(&'a self, input: TurnErrorInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Some(state) = input.thread_store.get::<ThreadRecoveryState>() else {
                return;
            };
            if !matches!(input.error, CodexErrorInfo::ServerOverloaded) {
                let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
                if state.turn_id.as_deref() == Some(input.turn_id) {
                    // A later duplicate/recoverable notification cannot undo a
                    // terminal failure such as usage exhaustion in this turn.
                    *state = RecoveryState::default();
                }
                return;
            }
            let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) else {
                return;
            };
            let goal = match self.eligible_goal(thread_id).await {
                Ok(Some(goal)) => goal,
                Ok(None) => return,
                Err(err) => {
                    tracing::warn!("failed to check goal state after turn error: {err}");
                    return;
                }
            };
            let Some(manager) = self.thread_manager.upgrade() else {
                return;
            };
            let Ok(thread) = manager.get_thread(thread_id).await else {
                return;
            };
            let Some(mut start) = thread.active_turn_start_options(input.turn_id).await else {
                return;
            };
            start.turn_trigger = Some(RECOVERY_TRIGGER.to_string());
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.turn_id.as_deref() != Some(input.turn_id) {
                return;
            }
            state.pending = Some(PendingRecovery {
                turn_id: input.turn_id.to_string(),
                goal,
                start,
            });
        })
    }
}
