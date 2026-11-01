use super::*;

#[tokio::test]
async fn active_goal_policy_error_shows_continuation_instead_of_block_notice() {
    for (status, replay, continuing) in [
        (AppThreadGoalStatus::Active, false, true),
        (AppThreadGoalStatus::Blocked, false, false),
        (AppThreadGoalStatus::Paused, false, false),
        (AppThreadGoalStatus::BudgetLimited, false, false),
        (AppThreadGoalStatus::Active, true, false),
    ] {
        for (message, info) in [
            (
                "This content was flagged for possible biological risk.",
                None,
            ),
            ("rejected", Some(AppServerCodexErrorInfo::CyberPolicy)),
        ] {
            let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;
            let thread_id = ThreadId::new();
            chat.current_goal_status = Some(super::super::goal_status::GoalStatusState::new(
                test_goal(thread_id, status, None),
                Instant::now(),
            ));
            chat.thread_usage.replaying_turn_completion = replay;
            chat.handle_non_retry_error(message.into(), info);
            let rendered = rendered_goal_summary(&mut rx);
            assert_eq!(
                rendered.contains("starting a new turn for the active goal"),
                continuing
            );
            assert_eq!(rendered.contains("This content"), !continuing);
        }
    }
}

#[tokio::test]
async fn active_goal_policy_error_with_recourse_waits_for_fresh_turn() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    handle_turn_started(&mut chat, "rejected-turn");
    chat.current_goal_status = Some(super::super::goal_status::GoalStatusState::new(
        test_goal(thread_id, AppThreadGoalStatus::Active, None),
        Instant::now(),
    ));
    safety_recourse::on_metadata(
        &mut chat,
        codex_app_server_protocol::TurnModerationMetadataNotification {
            thread_id: thread_id.to_string(),
            turn_id: "rejected-turn".into(),
            metadata: json!({"safety_recourse": {
                "schema_version": 1, "domain": "cyber", "source": "user_request",
                "source_message": {"id": "1", "kind": "ordinary_message", "role": "user"},
                "topic": "unauthorized_account_or_system_access",
                "suggestions": [
                    {"label": "Overview", "prompt_text": "Explain access"},
                    {"label": "Recovery", "prompt_text": "Explain account recovery"},
                    {"label": "Prevention", "prompt_text": "Explain account protection"},
                ],
            }}),
        },
    );
    assert!(chat.safety_recourse.is_some());
    chat.handle_server_notification(
        ServerNotification::TurnCompleted(TurnCompletedNotification {
            thread_id: thread_id.to_string(),
            turn: app_server_turn(
                "rejected-turn",
                AppServerTurnStatus::Failed,
                None,
                Some(AppServerTurnError {
                    misalignment: None,
                    message: "response rejected".into(),
                    codex_error_info: Some(AppServerCodexErrorInfo::CyberPolicy),
                    additional_details: None,
                }),
            ),
        }),
        None,
    );
    assert!(rendered_goal_summary(&mut rx).contains("starting a new turn for the active goal"));
    assert!(!chat.bottom_pane.has_active_view());
    assert!(chat.bottom_pane.composer_input_enabled());
}

#[tokio::test]
async fn active_goal_misalignment_error_waits_for_fresh_turn_without_locking_input() {
    for status in [
        AppThreadGoalStatus::Active,
        AppThreadGoalStatus::Blocked,
        AppThreadGoalStatus::Paused,
        AppThreadGoalStatus::BudgetLimited,
        AppThreadGoalStatus::Complete,
    ] {
        let (mut chat, mut rx, _ops) = make_chatwidget_manual(None).await;
        let thread_id = ThreadId::new();
        chat.thread_id = Some(thread_id);
        handle_turn_started(&mut chat, "rejected-turn");
        chat.current_goal_status = Some(super::super::goal_status::GoalStatusState::new(
            test_goal(thread_id, status, None),
            Instant::now(),
        ));
        handle_error(
            &mut chat,
            "response rejected",
            Some(CodexErrorInfo::MisalignmentPolicyViolation),
        );
        let continuing = status == AppThreadGoalStatus::Active;
        assert_eq!(chat.has_misalignment_policy_violation(), !continuing);
        assert_eq!(chat.bottom_pane.composer_input_enabled(), continuing);
        assert_eq!(
            rendered_goal_summary(&mut rx).contains("starting a new turn for the active goal"),
            continuing,
        );

        chat.handle_server_notification(
            ServerNotification::TurnCompleted(TurnCompletedNotification {
                thread_id: thread_id.to_string(),
                turn: app_server_turn(
                    "rejected-turn",
                    AppServerTurnStatus::Failed,
                    None,
                    Some(AppServerTurnError {
                        misalignment: None,
                        message: "response rejected".to_string(),
                        codex_error_info: Some(
                            AppServerCodexErrorInfo::MisalignmentPolicyViolation,
                        ),
                        additional_details: None,
                    }),
                ),
            }),
            None,
        );
        assert_eq!(chat.has_misalignment_policy_violation(), !continuing);
        assert!(!rendered_goal_summary(&mut rx).contains("starting a new turn"));
    }
}

#[tokio::test]
async fn goal_menu_active_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();

    chat.show_goal_summary(test_goal(
        thread_id,
        AppThreadGoalStatus::Active,
        /*token_budget*/ Some(80_000),
    ));

    assert_chatwidget_snapshot!("goal_menu_active", rendered_goal_summary(&mut rx));
}

#[tokio::test]
async fn goal_menu_paused_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();

    chat.show_goal_summary(test_goal(
        thread_id,
        AppThreadGoalStatus::Paused,
        /*token_budget*/ None,
    ));

    assert_chatwidget_snapshot!("goal_menu_paused", rendered_goal_summary(&mut rx));
}

#[tokio::test]
async fn goal_menu_budget_limited_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();

    chat.show_goal_summary(test_goal(
        thread_id,
        AppThreadGoalStatus::BudgetLimited,
        /*token_budget*/ Some(80_000),
    ));

    assert_chatwidget_snapshot!("goal_menu_budget_limited", rendered_goal_summary(&mut rx));
}

#[tokio::test]
async fn resume_paused_goal_prompt_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();

    chat.show_resume_paused_goal_prompt(
        thread_id,
        "Keep improving the bare goal command until it feels calm and useful.".to_string(),
    );

    assert_chatwidget_snapshot!(
        "resume_paused_goal_prompt",
        render_bottom_popup(&chat, /*width*/ 100)
    );
}

#[tokio::test]
async fn goal_edit_prompt_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();

    chat.show_goal_edit_prompt(
        thread_id,
        test_goal(
            thread_id,
            AppThreadGoalStatus::Active,
            /*token_budget*/ Some(80_000),
        ),
    );

    assert_chatwidget_snapshot!(
        "goal_edit_prompt",
        render_bottom_popup(&chat, /*width*/ 100)
    );
}

#[tokio::test]
async fn goal_edit_prompt_submits_preserved_status_and_budget() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();

    chat.show_goal_edit_prompt(
        thread_id,
        test_goal(
            thread_id,
            AppThreadGoalStatus::Paused,
            /*token_budget*/ Some(80_000),
        ),
    );
    chat.handle_paste(" with clearer wording".to_string());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    match rx.try_recv() {
        Ok(AppEvent::SetThreadGoalDraft {
            thread_id: event_thread_id,
            draft,
            mode:
                crate::app_event::ThreadGoalSetMode::UpdateExisting {
                    status,
                    token_budget,
                },
        }) => {
            assert_eq!(event_thread_id, thread_id);
            assert_eq!(
                draft.objective,
                "Keep improving the bare goal command until it feels calm and useful. with clearer wording"
            );
            assert_eq!(status, AppThreadGoalStatus::Paused);
            assert_eq!(token_budget, Some(80_000));
        }
        other => panic!("expected SetThreadGoalDraft event, got {other:?}"),
    }
    assert!(chat.no_modal_or_popup_active());
}

#[tokio::test]
async fn goal_edit_prompt_preserves_resumable_stopped_statuses() {
    for stopped_status in [
        AppThreadGoalStatus::Blocked,
        AppThreadGoalStatus::UsageLimited,
    ] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
        let thread_id = ThreadId::new();

        chat.show_goal_edit_prompt(
            thread_id,
            test_goal(
                thread_id,
                stopped_status,
                /*token_budget*/ Some(80_000),
            ),
        );
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

        match rx.try_recv() {
            Ok(AppEvent::SetThreadGoalDraft {
                mode:
                    crate::app_event::ThreadGoalSetMode::UpdateExisting {
                        status,
                        token_budget,
                    },
                ..
            }) => {
                assert_eq!(status, stopped_status);
                assert_eq!(token_budget, Some(80_000));
            }
            other => panic!("expected SetThreadGoalDraft event, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn goal_edit_prompt_resets_terminal_status_to_active() {
    let cases = [
        AppThreadGoalStatus::BudgetLimited,
        AppThreadGoalStatus::Complete,
    ];

    for terminal_status in cases {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
        let thread_id = ThreadId::new();

        chat.show_goal_edit_prompt(
            thread_id,
            test_goal(
                thread_id,
                terminal_status,
                /*token_budget*/ Some(80_000),
            ),
        );
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

        match rx.try_recv() {
            Ok(AppEvent::SetThreadGoalDraft {
                mode:
                    crate::app_event::ThreadGoalSetMode::UpdateExisting {
                        status,
                        token_budget,
                    },
                ..
            }) => {
                assert_eq!(status, AppThreadGoalStatus::Active);
                assert_eq!(token_budget, Some(80_000));
            }
            other => panic!("expected SetThreadGoalDraft event, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn resume_paused_goal_prompt_default_resumes_goal() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();

    chat.show_resume_paused_goal_prompt(thread_id, "Finish the paused goal.".to_string());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    match rx.try_recv() {
        Ok(AppEvent::SetThreadGoalStatus {
            thread_id: event_thread_id,
            status,
        }) => {
            assert_eq!(event_thread_id, thread_id);
            assert_eq!(status, AppThreadGoalStatus::Active);
        }
        other => panic!("expected SetThreadGoalStatus event, got {other:?}"),
    }
    assert!(chat.no_modal_or_popup_active());
}

#[tokio::test]
async fn resume_paused_goal_prompt_can_leave_goal_paused() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();

    chat.show_resume_paused_goal_prompt(thread_id, "Finish the paused goal.".to_string());
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    assert!(chat.no_modal_or_popup_active());
}

fn test_goal(
    thread_id: ThreadId,
    status: AppThreadGoalStatus,
    token_budget: Option<i64>,
) -> AppThreadGoal {
    AppThreadGoal {
        thread_id: thread_id.to_string(),
        objective: "Keep improving the bare goal command until it feels calm and useful."
            .to_string(),
        status,
        token_budget,
        tokens_used: 12_500,
        time_used_seconds: 90,
        created_at: 1_776_272_400,
        updated_at: 1_776_272_460,
    }
}

fn rendered_goal_summary(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::app_event::AppEvent>,
) -> String {
    drain_insert_history(rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n")
}
