use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::api::ChatMessage;
use crate::query_engine::budget::BudgetState;
use crate::query_engine::cost::SessionUsageTotals;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryRunStatus {
    Idle,
    Running,
    Completed,
    Cancelled,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySessionMetadata {
    pub session_id: String,
    pub workspace_root: PathBuf,
    pub active_model: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub total_tokens: usize,
    pub total_cost_usd: f64,
    pub model_usage: SessionUsageTotals,
    pub budget_state: BudgetState,
    pub last_run_status: QueryRunStatus,
    pub last_error: Option<String>,
}

impl QuerySessionMetadata {
    pub fn new(
        session_id: impl Into<String>,
        workspace_root: PathBuf,
        active_model: impl Into<String>,
        budget_state: BudgetState,
    ) -> Self {
        let now = Utc::now();
        Self {
            session_id: session_id.into(),
            workspace_root,
            active_model: active_model.into(),
            created_at: now,
            updated_at: now,
            total_tokens: 0,
            total_cost_usd: 0.0,
            model_usage: SessionUsageTotals::default(),
            budget_state,
            last_run_status: QueryRunStatus::Idle,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySessionSnapshot {
    pub session_id: String,
    pub workspace_root: PathBuf,
    pub active_model: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub total_tokens: usize,
    pub total_cost_usd: f64,
    pub model_usage: SessionUsageTotals,
    pub budget_state: BudgetState,
    pub last_run_status: QueryRunStatus,
    pub last_error: Option<String>,
    pub messages: Vec<ChatMessage>,
}

#[cfg(test)]
mod tests {
    use crate::query_engine::{QueryEngine, QueryRunStatus, TranscriptEvent};

    #[tokio::test]
    async fn session_resume_marks_interrupted_run_when_no_terminal_event_exists() {
        let temp = tempfile::tempdir().unwrap();
        let engine = QueryEngine::for_tests(temp.path().to_path_buf());

        let session = engine.create_session("workspace", "sonnet").await.unwrap();
        engine
            .append_transcript_event(
                &session.session_id,
                TranscriptEvent::user_message("u1", "hello"),
            )
            .await
            .unwrap();

        let resumed = engine.resume_session(&session.session_id).await.unwrap();

        assert_eq!(resumed.last_run_status, QueryRunStatus::Interrupted);
    }
}
