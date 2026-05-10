use std::time::Duration;

use claude_code_rs::plan_mode::{PlanMode, PlanModeSession};
use claude_code_rs::ultraplan::{
    find_ultraplan_trigger_positions, highlight_ultraplan_keyword, process_ultraplan_input,
    replace_ultraplan_keyword, ExitPlanModeScanner, UltraplanCommandHandler, UltraplanInputAction,
    UltraplanLaunchMode, UltraplanRoute,
};

#[test]
fn keyword_detection_filters_quotes_paths_and_other_slash_commands() {
    let positions = find_ultraplan_trigger_positions("请 ultraplan 重构认证模块");
    assert_eq!(positions.len(), 1);
    assert_eq!(
        &"请 ultraplan 重构认证模块"[positions[0].start..positions[0].end],
        "ultraplan"
    );

    assert!(find_ultraplan_trigger_positions("打印 \"ultraplan\" 这个词").is_empty());
    assert!(find_ultraplan_trigger_positions("打开 /tmp/ultraplan/spec.md").is_empty());
    assert!(find_ultraplan_trigger_positions("/help ultraplan").is_empty());
    assert_eq!(
        process_ultraplan_input("/ultraplan --remote 重构 CLI").action,
        UltraplanInputAction::Route(UltraplanRoute {
            launch_mode: UltraplanLaunchMode::Remote,
            original_input: "/ultraplan --remote 重构 CLI".to_string(),
            cleaned_prompt: "重构 CLI".to_string(),
            explicit_command: true,
        })
    );
}

#[test]
fn keyword_replacement_cleans_trigger_without_touching_non_triggers() {
    assert_eq!(
        replace_ultraplan_keyword("帮我 ultraplan 重构这个模块"),
        "帮我 重构这个模块"
    );
    assert_eq!(
        replace_ultraplan_keyword("路径 /tmp/ultraplan/spec.md 不应改变"),
        "路径 /tmp/ultraplan/spec.md 不应改变"
    );
}

#[test]
fn input_routing_respects_feature_flag_and_supports_command() {
    std::env::remove_var("FEATURE_ULTRAPLAN");
    assert_eq!(
        process_ultraplan_input("ultraplan 重构").action,
        UltraplanInputAction::Normal
    );

    std::env::set_var("FEATURE_ULTRAPLAN", "1");
    assert_eq!(
        process_ultraplan_input("帮我 ultraplan 重构模块").action,
        UltraplanInputAction::Route(UltraplanRoute {
            launch_mode: UltraplanLaunchMode::Local,
            original_input: "帮我 ultraplan 重构模块".to_string(),
            cleaned_prompt: "帮我 重构模块".to_string(),
            explicit_command: false,
        })
    );
    assert_eq!(
        process_ultraplan_input("/ultraplan 重构模块").action,
        UltraplanInputAction::Route(UltraplanRoute {
            launch_mode: UltraplanLaunchMode::Local,
            original_input: "/ultraplan 重构模块".to_string(),
            cleaned_prompt: "重构模块".to_string(),
            explicit_command: true,
        })
    );
    std::env::remove_var("FEATURE_ULTRAPLAN");
}

#[tokio::test]
async fn command_handler_enters_enhanced_plan_mode_locally() {
    let temp = tempfile::tempdir().unwrap();
    let session = PlanModeSession::new(temp.path().to_path_buf());
    let handler = UltraplanCommandHandler::new(session.clone());

    let result = handler
        .execute(UltraplanRoute {
            launch_mode: UltraplanLaunchMode::Local,
            original_input: "ultraplan 重构".to_string(),
            cleaned_prompt: "重构".to_string(),
            explicit_command: false,
        })
        .await
        .unwrap();

    assert_eq!(result.launch_mode, UltraplanLaunchMode::Local);
    assert_eq!(result.status.mode, PlanMode::Plan);
    assert!(result.status.ultraplan.as_ref().unwrap().active);
    assert!(result.system_prompt.contains("ULTRAPLAN"));
    assert!(result.system_prompt.contains("read-only"));
    assert!(result.system_prompt.contains("deep implementation plan"));
}

#[tokio::test]
async fn ccr_scanner_polls_until_exit_plan_is_approved() {
    let temp = tempfile::tempdir().unwrap();
    let session = PlanModeSession::new(temp.path().to_path_buf());
    let handler = UltraplanCommandHandler::new(session.clone());
    handler
        .execute(UltraplanRoute {
            launch_mode: UltraplanLaunchMode::Remote,
            original_input: "/ultraplan --remote refactor".to_string(),
            cleaned_prompt: "refactor".to_string(),
            explicit_command: true,
        })
        .await
        .unwrap();

    let session_for_exit = session.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        session_for_exit
            .exit_with_plan("1. Inspect\n2. Implement\n3. Test", Vec::new())
            .await
            .unwrap();
    });

    let scanner = ExitPlanModeScanner::new(session)
        .with_poll_interval(Duration::from_millis(5))
        .with_timeout(Duration::from_secs(1));
    let result = scanner.poll_for_approved_exit_plan_mode().await.unwrap();

    assert!(result.approved);
    assert!(result.plan_file_path.unwrap().exists());
    assert!(result.poll_attempts >= 1);
}

#[test]
fn rainbow_highlight_marks_only_real_trigger_spans() {
    let spans = highlight_ultraplan_keyword("ultraplan /tmp/ultraplan");

    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].text, "ultraplan");
    assert!(spans[0].rainbow);
    assert_eq!(spans[1].text, " /tmp/ultraplan");
    assert!(!spans[1].rainbow);
}
