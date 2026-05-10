use std::fs;
use std::path::Path;
use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use claude_code_rs::services::{AutoDreamConfig, AutoDreamService};
use claude_code_rs::{AppState, Settings};
use filetime::{set_file_mtime, FileTime};
use tempfile::tempdir;
use tokio::sync::RwLock;

fn service_for(
    memory_dir: &Path,
    sessions_dir: &Path,
    config: AutoDreamConfig,
) -> AutoDreamService {
    let mut settings = Settings::default();
    settings.memory.enabled = true;
    settings.memory.auto_memory_directory = Some(memory_dir.to_path_buf());

    AutoDreamService::new(
        Arc::new(RwLock::new(AppState::new(settings))),
        Some(AutoDreamConfig {
            memory_dir: Some(memory_dir.to_path_buf()),
            sessions_dir: Some(sessions_dir.to_path_buf()),
            state_path: Some(memory_dir.join(".autodream_state.json")),
            session_scan_interval_ms: 0,
            ..config
        }),
    )
}

fn write_session(root: &Path, session_id: &str, body: &str) {
    let session_dir = root.join(session_id);
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(session_dir.join("transcript.jsonl"), body).unwrap();
}

fn set_mtime(path: &Path, timestamp: i64) {
    set_file_mtime(path, FileTime::from_unix_time(timestamp, 0)).unwrap();
}

#[tokio::test]
async fn manual_dream_organizes_memdir_and_prunes_index() {
    let temp = tempdir().unwrap();
    let memory_dir = temp.path().join("memory");
    let sessions_dir = temp.path().join("sessions");
    fs::create_dir_all(memory_dir.join("project")).unwrap();
    fs::create_dir_all(memory_dir.join("logs/2026/05")).unwrap();

    let long_index = (0..260)
        .map(|line| format!("- existing pointer {} {}", line, "x".repeat(180)))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(memory_dir.join("MEMORY.md"), long_index).unwrap();
    fs::write(
        memory_dir.join("project/roadmap.md"),
        "# Roadmap\nsummary: yesterday we decided Auto Dream must be deterministic.\n",
    )
    .unwrap();
    fs::write(
        memory_dir.join("logs/2026/05/2026-05-10.md"),
        "yesterday user asked for full Auto Dream implementation\n",
    )
    .unwrap();
    write_session(
        &sessions_dir,
        "session-a",
        r#"{"event":{"type":"UserMessage","content":"remember project preference"}}"#,
    );

    let service = service_for(
        &memory_dir,
        &sessions_dir,
        AutoDreamConfig {
            min_hours: 24,
            min_sessions: 5,
            enabled: true,
            ..AutoDreamConfig::default()
        },
    );
    let now = Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();

    let report = service.force_consolidation_at(now).await.unwrap();

    assert!(report.manual);
    assert_eq!(report.memory_dir, memory_dir);
    assert!(report.prompt_path.ends_with(".last-dream-prompt.md"));
    assert!(report.index_lines <= 200);
    assert!(report.index_bytes <= 25 * 1024);

    let index = fs::read_to_string(memory_dir.join("MEMORY.md")).unwrap();
    assert!(index.contains("Auto Dream updated 2026-05-10"));
    assert!(index.contains("project/roadmap.md"));
    assert!(index.contains("logs/2026/05/2026-05-10.md"));
    assert!(!index.contains("yesterday"));
    assert!(index.lines().count() <= 200);
    assert!(index.len() <= 25 * 1024);
    assert!(index.lines().all(|line| line.len() <= 150));

    let prompt = fs::read_to_string(memory_dir.join(".last-dream-prompt.md")).unwrap();
    assert!(prompt.contains("Phase 1: Orient"));
    assert!(prompt.contains("Phase 2: Gather"));
    assert!(prompt.contains("Phase 3: Consolidate"));
    assert!(prompt.contains("Phase 4: Prune and Index"));
    assert!(prompt.contains("Do not extract new memories"));
}

#[tokio::test]
async fn check_and_run_applies_time_session_and_pid_lock_gates() {
    let temp = tempdir().unwrap();
    let memory_dir = temp.path().join("memory");
    let sessions_dir = temp.path().join("sessions");
    fs::create_dir_all(&memory_dir).unwrap();
    let service = service_for(
        &memory_dir,
        &sessions_dir,
        AutoDreamConfig {
            min_hours: 24,
            min_sessions: 2,
            enabled: true,
            ..AutoDreamConfig::default()
        },
    );
    let now = Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();

    assert!(!service.check_and_run_at(now).await.unwrap());
    let status = service.get_status().await;
    assert_eq!(status.sessions_accumulated, 0);
    assert_eq!(status.last_skip_reason.as_deref(), Some("sessions"));

    write_session(&sessions_dir, "session-a", "{}\n");
    write_session(&sessions_dir, "session-b", "{}\n");
    let lock_path = memory_dir.join(".consolidate-lock");
    fs::write(&lock_path, "pid:999999").unwrap();
    set_mtime(&lock_path, now.timestamp());

    assert!(!service.check_and_run_at(now).await.unwrap());
    let status = service.get_status().await;
    assert_eq!(status.sessions_accumulated, 2);
    assert_eq!(status.last_skip_reason.as_deref(), Some("locked"));

    set_mtime(&lock_path, (now - Duration::hours(2)).timestamp());

    assert!(service.check_and_run_at(now).await.unwrap());
    let status = service.get_status().await;
    assert_eq!(status.last_skip_reason, None);
    assert_eq!(status.last_consolidation, now);
}

#[tokio::test]
async fn failed_dream_rolls_back_existing_lock_metadata() {
    let temp = tempdir().unwrap();
    let memory_dir = temp.path().join("memory");
    let sessions_dir = temp.path().join("sessions");
    fs::create_dir_all(&memory_dir).unwrap();
    fs::create_dir(memory_dir.join("MEMORY.md")).unwrap();

    let lock_path = memory_dir.join(".consolidate-lock");
    let previous = Utc.with_ymd_and_hms(2026, 5, 8, 8, 0, 0).unwrap();
    fs::write(&lock_path, "last:old-holder").unwrap();
    set_mtime(&lock_path, previous.timestamp());

    let service = service_for(
        &memory_dir,
        &sessions_dir,
        AutoDreamConfig {
            min_hours: 0,
            min_sessions: 0,
            enabled: true,
            ..AutoDreamConfig::default()
        },
    );
    let now = Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();

    let error = service.force_consolidation_at(now).await.unwrap_err();

    assert!(error.to_string().contains("MEMORY.md"));
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), "last:old-holder");
    let restored = fs::metadata(&lock_path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert_eq!(restored, previous.timestamp());
}

#[tokio::test]
async fn disabled_auto_dream_does_not_create_memory_directory() {
    let temp = tempdir().unwrap();
    let memory_dir = temp.path().join("memory");
    let sessions_dir = temp.path().join("sessions");
    let service = service_for(
        &memory_dir,
        &sessions_dir,
        AutoDreamConfig {
            min_hours: 0,
            min_sessions: 0,
            enabled: false,
            ..AutoDreamConfig::default()
        },
    );
    let now = Utc.with_ymd_and_hms(2026, 5, 10, 12, 0, 0).unwrap();

    assert!(!service.check_and_run_at(now).await.unwrap());
    assert!(!memory_dir.exists());
    let status = service.get_status().await;
    assert_eq!(status.last_skip_reason.as_deref(), Some("disabled"));
}
