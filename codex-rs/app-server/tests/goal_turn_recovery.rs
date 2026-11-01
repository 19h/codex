//! Verifies goal continuation after recoverable errors through the public API.

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_sequence;
use codex_app_server_protocol::ThreadGoalGetResponse;
use codex_app_server_protocol::ThreadGoalSetResponse;
use codex_app_server_protocol::ThreadGoalStatus;
use codex_app_server_protocol::ThreadGoalUpdatedNotification;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_features::Feature;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn overloaded_goal_turns_start_fresh_turns_until_completion() -> Result<()> {
    const FAILURES: usize = 5;
    let mut scripts: Vec<_> = (0..FAILURES)
        .map(|turn| {
            responses::sse(vec![json!({
                "type": "response.failed",
                "response": {
                    "id": format!("overloaded-{turn}"),
                    "error": { "code": "server_is_overloaded", "message": "test overload" },
                }
            })])
        })
        .collect();
    scripts.push(responses::sse(vec![
        responses::ev_response_created("recovered"),
        responses::ev_function_call("complete-goal", "update_goal", r#"{"status":"complete"}"#),
        responses::ev_completed("recovered"),
    ]));
    scripts.push(responses::sse(vec![
        responses::ev_response_created("done"),
        responses::ev_assistant_message("done-message", "Finished"),
        responses::ev_completed("done"),
    ]));
    let server = create_mock_responses_server_sequence(scripts).await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.4")
        .enable_feature(Feature::Goals)
        .write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_plugin_startup_tasks()
        .without_managed_config()
        .build_initialized()
        .await?;
    let request = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } = mcp.read_response(request).await?;
    let request = mcp
        .send_raw_request(
            "thread/goal/set",
            Some(json!({
                "threadId": thread.id, "objective": "Finish after the overloaded turn"
            })),
        )
        .await?;
    let _: ThreadGoalSetResponse = mcp.read_response(request).await?;

    let mut failed_turns = std::collections::HashSet::new();
    // Only goal/set starts work. Each overload terminates a distinct turn.
    for _ in 0..FAILURES {
        let completed: TurnCompletedNotification = timeout(
            Duration::from_secs(30),
            mcp.read_notification("turn/completed"),
        )
        .await??;
        assert_eq!(completed.turn.status, TurnStatus::Failed);
        assert!(failed_turns.insert(completed.turn.id));
        assert_eq!(
            completed
                .turn
                .error
                .expect("overload remains visible")
                .codex_error_info,
            Some(codex_app_server_protocol::CodexErrorInfo::ServerOverloaded),
        );
    }
    let completed: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    timeout(Duration::from_secs(30), async {
        loop {
            let notification: ThreadGoalUpdatedNotification =
                mcp.read_notification("thread/goal/updated").await?;
            assert_ne!(notification.goal.status, ThreadGoalStatus::Blocked);
            if notification.goal.status == ThreadGoalStatus::Complete {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await??;
    server.verify().await;
    Ok(())
}

#[test_case::test_case(true, false; "overload_without_goal")]
#[test_case::test_case(false, false; "overload_goals_disabled")]
#[test_case::test_case(true, true; "overload_after_goal_completion")]
#[tokio::test]
async fn overloaded_turn_continues_unfinished_work_without_active_goal(
    goals_enabled: bool,
    complete_goal: bool,
) -> Result<()> {
    let output_schema = json!({
        "type": "object",
        "properties": { "result": { "type": "string" } },
        "required": ["result"], "additionalProperties": false,
    });
    let failures = 5;
    let mut scripts = Vec::new();
    if complete_goal {
        scripts.push(responses::sse(vec![
            responses::ev_response_created("complete-goal"),
            responses::ev_function_call("complete", "update_goal", r#"{"status":"complete"}"#),
            responses::ev_completed("complete-goal"),
        ]));
    }
    for turn in 0..failures {
        scripts.push(responses::sse(vec![json!({
            "type": "response.failed",
            "response": {
                "id": format!("failed-{turn}"),
                "error": { "code": "server_is_overloaded", "message": "test overload" }
            }
        })]));
    }
    scripts.push(responses::sse(vec![
        responses::ev_response_created("finished"),
        responses::ev_assistant_message(
            "finished-message",
            r#"{"result":"Independent permitted work finished"}"#,
        ),
        responses::ev_completed("finished"),
    ]));
    let server = create_mock_responses_server_sequence(scripts).await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.4")
        .with_features(&std::collections::BTreeMap::from([(
            Feature::Goals,
            goals_enabled,
        )]))
        .write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_plugin_startup_tasks()
        .without_managed_config()
        .build_initialized()
        .await?;
    let request = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } = mcp.read_response(request).await?;
    if complete_goal {
        let request = mcp
            .send_raw_request(
                "thread/goal/set",
                Some(json!({
                    "threadId": thread.id, "objective": "Complete the initial work"
                })),
            )
            .await?;
        let _: ThreadGoalSetResponse = mcp.read_response(request).await?;
    } else {
        let request = mcp
            .send_raw_request(
                "turn/start",
                Some(json!({
                    "threadId": thread.id,
                    "input": [{ "type": "text", "text": "Finish the permitted work" }],
                    "outputSchema": output_schema,
                })),
            )
            .await?;
        let _: TurnStartResponse = mcp.read_response(request).await?;
    }
    let mut root = None;
    for _ in 0..failures {
        let completed: TurnCompletedNotification = timeout(
            Duration::from_secs(30),
            mcp.read_notification("turn/completed"),
        )
        .await??;
        assert_eq!(completed.turn.status, TurnStatus::Failed);
        assert!(
            completed.turn.error.is_some(),
            "original error must remain visible"
        );
        let failed_root = completed
            .turn
            .root_turn_id
            .expect("root lineage is recorded");
        if let Some(root) = root.as_ref() {
            assert_eq!(&failed_root, root);
        } else {
            root = Some(failed_root);
        }
    }
    let completed: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    assert_eq!(completed.turn.root_turn_id, root);
    if complete_goal {
        let request = mcp
            .send_raw_request("thread/goal/get", Some(json!({ "threadId": thread.id })))
            .await?;
        let response: ThreadGoalGetResponse = mcp.read_response(request).await?;
        assert_eq!(response.goal.unwrap().status, ThreadGoalStatus::Complete);
    }
    let requests: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| {
            request.method.as_str() == "POST" && request.url.path().ends_with("/responses")
        })
        .collect();
    for request in &requests {
        let body: serde_json::Value = request.body_json()?;
        assert_eq!(body["model"], "gpt-5.4");
        if !complete_goal {
            assert_eq!(body["text"]["format"]["schema"], output_schema);
        }
    }
    let final_body: serde_json::Value = requests.last().unwrap().body_json()?;
    let final_input = final_body["input"].to_string();
    assert!(final_input.contains("the server was overloaded"));
    server.verify().await;
    Ok(())
}

#[test_case::test_case(None, "cyber_policy"; "cyber_without_goal")]
#[test_case::test_case(None, "bio_policy"; "bio_without_goal")]
#[test_case::test_case(None, "invalid_prompt"; "terminal_error_without_goal")]
#[test_case::test_case(Some("paused"), "cyber_policy"; "paused_goal")]
#[test_case::test_case(Some("blocked"), "server_is_overloaded"; "blocked_goal")]
#[test_case::test_case(Some("paused"), "misalignment_policy_violation"; "paused_misalignment")]
#[test_case::test_case(Some("blocked"), "misalignment_policy_violation"; "blocked_misalignment")]
#[test_case::test_case(None, "misalignment_policy_violation"; "misalignment_without_goal")]
#[tokio::test]
async fn stopped_work_is_not_recovered(goal_status: Option<&str>, code: &str) -> Result<()> {
    let mut scripts = Vec::new();
    if let Some(status) = goal_status {
        scripts.push(responses::sse(vec![
            responses::ev_response_created("stop-goal"),
            responses::ev_function_call(
                "stop",
                "update_goal",
                &json!({"status": status}).to_string(),
            ),
            responses::ev_completed("stop-goal"),
        ]));
    }
    scripts.push(responses::sse(vec![json!({
        "type": "response.failed",
        "response": { "id": "failed", "error": { "code": code } }
    })]));
    let server = create_mock_responses_server_sequence(scripts).await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.4")
        .enable_feature(Feature::Goals)
        .write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_plugin_startup_tasks()
        .without_managed_config()
        .build_initialized()
        .await?;
    let request = mcp
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let ThreadStartResponse { thread, .. } = mcp.read_response(request).await?;
    if goal_status.is_some() {
        let request = mcp
            .send_raw_request(
                "thread/goal/set",
                Some(json!({
                    "threadId": thread.id, "objective": "Respect stopped work"
                })),
            )
            .await?;
        let _: ThreadGoalSetResponse = mcp.read_response(request).await?;
    } else {
        let request = mcp
            .send_raw_request(
                "turn/start",
                Some(json!({
                    "threadId": thread.id,
                    "input": [{ "type": "text", "text": "Finish permitted work" }]
                })),
            )
            .await?;
        let _: TurnStartResponse = mcp.read_response(request).await?;
    }
    let completed: TurnCompletedNotification = timeout(
        Duration::from_secs(30),
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(completed.turn.status, TurnStatus::Failed);
    tokio::time::sleep(Duration::from_millis(100)).await;
    server.verify().await;
    Ok(())
}
