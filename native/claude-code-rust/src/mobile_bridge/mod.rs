use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use tokio::sync::RwLock;

use crate::agent_runtime::{AgentCancellation, AgentEvent, AgentEventHandler};
use crate::api::ChatMessage;
use crate::config::Settings;
use crate::query_engine::{BudgetState, QueryEngine, QuerySubmitRequest, SessionUsageTotals};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeRunRequest {
    pub run_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub trigger_label: String,
    pub history: Vec<BridgeMessage>,
    pub settings: BridgeRequestSettings,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub max_iterations: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeRequestSettings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub system_prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BridgeRunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BridgeEventType {
    Started,
    RequestBuilt,
    ProviderSelected,
    ModelSwitched,
    BudgetWarning,
    BudgetExhausted,
    ReasoningSummary,
    ToolCallRequested,
    ToolCallCompleted,
    ToolCallFailed,
    AnswerReceived,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeRunEvent {
    pub order_index: usize,
    pub event_type: BridgeEventType,
    pub title: String,
    pub details: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeRunSnapshot {
    pub run_id: String,
    pub session_id: String,
    pub status: BridgeRunStatus,
    pub active_model: String,
    pub reasoning_content: String,
    pub answer_content: String,
    pub total_tokens: usize,
    pub total_cost_usd: f64,
    pub budget_state: BudgetState,
    pub model_usage: SessionUsageTotals,
    pub error_summary: Option<String>,
    pub events: Vec<BridgeRunEvent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    ok: bool,
    workspace_root: String,
}

#[derive(Clone)]
pub struct MobileBridgeServer {
    runs: Arc<DashMap<String, BridgeRunHandle>>,
    default_workspace_root: Arc<PathBuf>,
    query_engine: Arc<QueryEngine>,
}

#[derive(Clone)]
struct BridgeRunHandle {
    state: Arc<RwLock<BridgeRunState>>,
    cancel_requested: Arc<AtomicBool>,
}

impl MobileBridgeServer {
    pub fn new() -> Self {
        let default_workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_workspace_root(default_workspace_root)
    }

    pub fn with_workspace_root(default_workspace_root: impl Into<PathBuf>) -> Self {
        let default_workspace_root = default_workspace_root.into();
        let query_engine_root = default_workspace_root
            .join(".claude-code")
            .join("query-engine")
            .join("sessions");

        Self {
            runs: Arc::new(DashMap::new()),
            default_workspace_root: Arc::new(default_workspace_root),
            query_engine: Arc::new(QueryEngine::with_root(query_engine_root)),
        }
    }

    pub async fn start_run(&self, request: BridgeRunRequest) -> Result<()> {
        if self.runs.contains_key(&request.run_id) {
            return Err(anyhow!("Run already exists: {}", request.run_id));
        }

        let session_id = request
            .session_id
            .clone()
            .unwrap_or_else(|| request.run_id.clone());

        let handle = BridgeRunHandle {
            state: Arc::new(RwLock::new(BridgeRunState::new(&request.run_id, &session_id))),
            cancel_requested: Arc::new(AtomicBool::new(false)),
        };
        self.runs.insert(request.run_id.clone(), handle);

        let server = self.clone();
        tokio::spawn(async move {
            server.process_run(request).await;
        });

        Ok(())
    }

    pub async fn snapshot(&self, run_id: &str) -> Option<BridgeRunSnapshot> {
        let handle = self.runs.get(run_id).map(|entry| entry.clone())?;
        let snapshot = {
            let state = handle.state.read().await;
            state.snapshot()
        };
        Some(snapshot)
    }

    pub async fn cancel_run(&self, run_id: &str) -> Result<()> {
        let handle = self
            .runs
            .get(run_id)
            .ok_or_else(|| anyhow!("Run not found: {}", run_id))?;

        handle.cancel_requested.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn process_run(&self, request: BridgeRunRequest) {
        let run_id = request.run_id.clone();
        let workspace_root = self.resolve_workspace_root(&request);
        let session_id = request
            .session_id
            .clone()
            .unwrap_or_else(|| request.run_id.clone());

        self.append_event(
            &run_id,
            BridgeEventType::Started,
            "Started",
            request.trigger_label.clone(),
        )
        .await;
        self.append_event(
            &run_id,
            BridgeEventType::RequestBuilt,
            "Prompt Built",
            format!(
                "Built prompt context from {} transcript messages.",
                request.history.len()
            ),
        )
        .await;
        self.append_event(
            &run_id,
            BridgeEventType::ProviderSelected,
            "Provider Selected",
            format!("Embedded Rust Agent | {}", request.settings.model),
        )
        .await;

        match self
            .execute_agent_run(&request, &run_id, workspace_root)
            .await
        {
            Ok(session_snapshot) => {
                self.apply_session_snapshot(&run_id, &session_snapshot).await;
                match session_snapshot.last_run_status {
                    crate::query_engine::QueryRunStatus::Cancelled => {
                        self.mark_cancelled(&run_id, "Cancelled from the UI.").await;
                    }
                    crate::query_engine::QueryRunStatus::Failed => {
                        self.mark_failed(
                            &run_id,
                            session_snapshot
                                .last_error
                                .as_deref()
                                .unwrap_or("Query session failed."),
                        )
                        .await;
                    }
                    _ => {
                        self.mark_completed(
                            &run_id,
                            &format!(
                                "Completed in session {} using {}.",
                                session_id, session_snapshot.active_model
                            ),
                        )
                        .await;
                    }
                }
            }
            Err(error) => {
                self.mark_failed(&run_id, &error.to_string()).await;
            }
        }
    }

    async fn execute_agent_run(
        &self,
        request: &BridgeRunRequest,
        run_id: &str,
        workspace_root: PathBuf,
    ) -> Result<crate::query_engine::QuerySessionSnapshot> {
        if request.settings.base_url.trim().is_empty() || request.settings.api_key.trim().is_empty()
        {
            return Err(anyhow!("Embedded runtime needs both base URL and API key."));
        }

        let settings = build_settings(&request.settings, workspace_root.clone());
        let event_sink = BridgeEventSink {
            server: self.clone(),
            run_id: run_id.to_string(),
        };
        let cancellation = BridgeCancellationToken {
            cancel_requested: self
                .runs
                .get(run_id)
                .map(|handle| handle.cancel_requested.clone())
                .ok_or_else(|| anyhow!("Run not found: {}", run_id))?,
        };

        self.query_engine
            .submit_message(
                QuerySubmitRequest {
                    run_id: run_id.to_string(),
                    session_id: request
                        .session_id
                        .clone()
                        .unwrap_or_else(|| request.run_id.clone()),
                    system_prompt: request.settings.system_prompt.clone(),
                    history: request
                        .history
                        .iter()
                        .map(|message| ChatMessage {
                            role: message.role.clone(),
                            content: Some(message.content.clone()),
                            reasoning_content: None,
                            tool_calls: None,
                            tool_call_id: None,
                        })
                        .collect(),
                    settings,
                    workspace_root,
                    max_iterations: request.max_iterations.unwrap_or(0),
                },
                &event_sink,
                &cancellation,
            )
            .await
    }

    fn resolve_workspace_root(&self, request: &BridgeRunRequest) -> PathBuf {
        request
            .workspace_root
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| (*self.default_workspace_root).clone())
    }

    async fn append_event(
        &self,
        run_id: &str,
        event_type: BridgeEventType,
        title: impl Into<String>,
        details: impl Into<String>,
    ) {
        if let Some(handle) = self.runs.get(run_id).map(|entry| entry.clone()) {
            let mut guard = handle.state.write().await;
            let order_index = guard.events.len();
            guard.events.push(BridgeRunEvent {
                order_index,
                event_type,
                title: title.into(),
                details: details.into(),
                created_at: now_millis(),
            });
        }
    }

    async fn update_reasoning(&self, run_id: &str, reasoning_content: String) {
        if let Some(handle) = self.runs.get(run_id).map(|entry| entry.clone()) {
            handle.state.write().await.reasoning_content = reasoning_content;
        }
    }

    async fn update_answer(&self, run_id: &str, answer_content: String) {
        if let Some(handle) = self.runs.get(run_id).map(|entry| entry.clone()) {
            handle.state.write().await.answer_content = answer_content;
        }
    }

    async fn apply_session_snapshot(
        &self,
        run_id: &str,
        session_snapshot: &crate::query_engine::QuerySessionSnapshot,
    ) {
        if let Some(handle) = self.runs.get(run_id).map(|entry| entry.clone()) {
            let mut model_switch_details = None;
            let mut budget_warning_details = None;
            let mut budget_exhausted_details = None;

            {
                let mut guard = handle.state.write().await;
                if !guard.active_model.is_empty() && guard.active_model != session_snapshot.active_model {
                    model_switch_details = Some(format!(
                        "Session switched models from {} to {}.",
                        guard.active_model, session_snapshot.active_model
                    ));
                }

                if !guard.budget_state.warning_emitted && session_snapshot.budget_state.warning_emitted {
                    budget_warning_details = Some(format_budget_details(
                        "Soft budget warning",
                        session_snapshot.budget_state.total_cost_usd,
                        session_snapshot.budget_state.soft_budget_usd,
                    ));
                }

                if !guard.budget_state.hard_limit_reached
                    && session_snapshot.budget_state.hard_limit_reached
                {
                    budget_exhausted_details = Some(format_budget_details(
                        "Budget exhausted",
                        session_snapshot.budget_state.total_cost_usd,
                        session_snapshot.budget_state.hard_budget_usd,
                    ));
                }

                guard.session_id = session_snapshot.session_id.clone();
                guard.active_model = session_snapshot.active_model.clone();
                guard.total_tokens = session_snapshot.total_tokens;
                guard.total_cost_usd = session_snapshot.total_cost_usd;
                guard.budget_state = session_snapshot.budget_state.clone();
                guard.model_usage = session_snapshot.model_usage.clone();
            }

            if let Some(details) = model_switch_details {
                self.append_event(
                    run_id,
                    BridgeEventType::ModelSwitched,
                    "Model Switched",
                    details,
                )
                .await;
            }

            if let Some(details) = budget_warning_details {
                self.append_event(
                    run_id,
                    BridgeEventType::BudgetWarning,
                    "Budget Warning",
                    details,
                )
                .await;
            }

            if let Some(details) = budget_exhausted_details {
                self.append_event(
                    run_id,
                    BridgeEventType::BudgetExhausted,
                    "Budget Exhausted",
                    details,
                )
                .await;
            }
        }
    }

    async fn mark_completed(&self, run_id: &str, details: &str) {
        if let Some(handle) = self.runs.get(run_id).map(|entry| entry.clone()) {
            handle.state.write().await.status = BridgeRunStatus::Completed;
            self.append_event(
                run_id,
                BridgeEventType::Completed,
                "Completed",
                details.to_string(),
            )
            .await;
        }
    }

    async fn mark_cancelled(&self, run_id: &str, details: &str) {
        if let Some(handle) = self.runs.get(run_id).map(|entry| entry.clone()) {
            {
                let mut guard = handle.state.write().await;
                guard.status = BridgeRunStatus::Cancelled;
                guard.error_summary = Some(details.to_string());
                if guard.answer_content.trim().is_empty() {
                    guard.answer_content = "Agent run cancelled.".to_string();
                }
            }
            self.append_event(
                run_id,
                BridgeEventType::Cancelled,
                "Cancelled",
                details.to_string(),
            )
            .await;
        }
    }

    async fn mark_failed(&self, run_id: &str, error_summary: &str) {
        if let Some(handle) = self.runs.get(run_id).map(|entry| entry.clone()) {
            {
                let mut guard = handle.state.write().await;
                guard.status = BridgeRunStatus::Failed;
                guard.error_summary = Some(error_summary.to_string());
                if guard.answer_content.trim().is_empty() {
                    guard.answer_content = format!("Agent run failed: {}", error_summary);
                }
            }
            self.append_event(
                run_id,
                BridgeEventType::Failed,
                "Failed",
                error_summary.to_string(),
            )
            .await;
        }
    }

    pub fn router(self) -> Router {
        Router::new()
            .route("/api/health", get(health_handler))
            .route("/api/runs", post(start_run_handler))
            .route("/api/runs/:run_id", get(snapshot_handler))
            .route("/api/runs/:run_id/cancel", post(cancel_run_handler))
            .with_state(self)
    }
}

impl Default for MobileBridgeServer {
    fn default() -> Self {
        Self::new()
    }
}

struct BridgeRunState {
    run_id: String,
    session_id: String,
    status: BridgeRunStatus,
    active_model: String,
    reasoning_content: String,
    answer_content: String,
    total_tokens: usize,
    total_cost_usd: f64,
    budget_state: BudgetState,
    model_usage: SessionUsageTotals,
    error_summary: Option<String>,
    events: Vec<BridgeRunEvent>,
}

impl BridgeRunState {
    fn new(run_id: &str, session_id: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            session_id: session_id.to_string(),
            status: BridgeRunStatus::Running,
            active_model: String::new(),
            reasoning_content: String::new(),
            answer_content: String::new(),
            total_tokens: 0,
            total_cost_usd: 0.0,
            budget_state: BudgetState::default(),
            model_usage: SessionUsageTotals::default(),
            error_summary: None,
            events: Vec::new(),
        }
    }

    fn snapshot(&self) -> BridgeRunSnapshot {
        BridgeRunSnapshot {
            run_id: self.run_id.clone(),
            session_id: self.session_id.clone(),
            status: self.status.clone(),
            active_model: self.active_model.clone(),
            reasoning_content: self.reasoning_content.clone(),
            answer_content: self.answer_content.clone(),
            total_tokens: self.total_tokens,
            total_cost_usd: self.total_cost_usd,
            budget_state: self.budget_state.clone(),
            model_usage: self.model_usage.clone(),
            error_summary: self.error_summary.clone(),
            events: self.events.clone(),
        }
    }
}

struct BridgeEventSink {
    server: MobileBridgeServer,
    run_id: String,
}

#[async_trait]
impl AgentEventHandler for BridgeEventSink {
    async fn on_event(&self, event: AgentEvent) {
        match event {
            AgentEvent::ReasoningDelta { full_text, .. } => {
                self.server.update_reasoning(&self.run_id, full_text).await;
            }
            AgentEvent::Reasoning { full_text, summary } => {
                self.server.update_reasoning(&self.run_id, full_text).await;
                self.server
                    .append_event(
                        &self.run_id,
                        BridgeEventType::ReasoningSummary,
                        "Reasoning Summary",
                        summary,
                    )
                    .await;
            }
            AgentEvent::ToolCallRequested {
                tool_name,
                input_preview,
                ..
            } => {
                self.server
                    .append_event(
                        &self.run_id,
                        BridgeEventType::ToolCallRequested,
                        format!("Tool Requested: {}", tool_name),
                        input_preview,
                    )
                    .await;
            }
            AgentEvent::ToolCallCompleted {
                tool_name,
                output_preview,
                ..
            } => {
                self.server
                    .append_event(
                        &self.run_id,
                        BridgeEventType::ToolCallCompleted,
                        format!("Tool Completed: {}", tool_name),
                        output_preview,
                    )
                    .await;
            }
            AgentEvent::ToolCallFailed {
                tool_name,
                error_summary,
                ..
            } => {
                self.server
                    .append_event(
                        &self.run_id,
                        BridgeEventType::ToolCallFailed,
                        format!("Tool Failed: {}", tool_name),
                        error_summary,
                    )
                    .await;
            }
            AgentEvent::MemorySurfaced { .. } => {}
            AgentEvent::AnswerDelta { full_text, .. } => {
                self.server.update_answer(&self.run_id, full_text).await;
            }
            AgentEvent::FinalAnswer { answer } => {
                self.server
                    .update_answer(&self.run_id, answer.clone())
                    .await;
                self.server
                    .append_event(
                        &self.run_id,
                        BridgeEventType::AnswerReceived,
                        "Answer Received",
                        trim_for_event(&answer, 400),
                    )
                    .await;
            }
        }
    }
}

struct BridgeCancellationToken {
    cancel_requested: Arc<AtomicBool>,
}

impl AgentCancellation for BridgeCancellationToken {
    fn is_cancelled(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartRunResponse {
    accepted: bool,
    run_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CancelRunResponse {
    accepted: bool,
    run_id: String,
}

async fn health_handler(State(server): State<MobileBridgeServer>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        workspace_root: server.default_workspace_root.display().to_string(),
    })
}

async fn start_run_handler(
    State(server): State<MobileBridgeServer>,
    Json(request): Json<BridgeRunRequest>,
) -> std::result::Result<Json<StartRunResponse>, (StatusCode, String)> {
    server
        .start_run(request.clone())
        .await
        .map_err(internal_error)?;

    Ok(Json(StartRunResponse {
        accepted: true,
        run_id: request.run_id,
    }))
}

async fn snapshot_handler(
    Path(run_id): Path<String>,
    State(server): State<MobileBridgeServer>,
) -> std::result::Result<Json<BridgeRunSnapshot>, StatusCode> {
    server
        .snapshot(&run_id)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

async fn cancel_run_handler(
    Path(run_id): Path<String>,
    State(server): State<MobileBridgeServer>,
) -> std::result::Result<Json<CancelRunResponse>, (StatusCode, String)> {
    server.cancel_run(&run_id).await.map_err(internal_error)?;

    Ok(Json(CancelRunResponse {
        accepted: true,
        run_id,
    }))
}

fn internal_error(error: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

fn build_settings(request_settings: &BridgeRequestSettings, workspace_root: PathBuf) -> Settings {
    let mut settings = Settings::default();
    settings.api.api_key = Some(request_settings.api_key.clone());
    settings.api.base_url = request_settings.base_url.clone();
    settings.model = request_settings.model.clone();
    settings.working_dir = workspace_root;
    settings
}

fn trim_for_event(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_chars).collect()
    }
}

fn format_budget_details(label: &str, total_cost_usd: f64, threshold_usd: Option<f64>) -> String {
    match threshold_usd {
        Some(threshold) => format!(
            "{} at ${:.6} total cost (threshold ${:.6}).",
            label, total_cost_usd, threshold
        ),
        None => format!("{} at ${:.6} total cost.", label, total_cost_usd),
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(feature = "mobile-bridge")]
static ANDROID_SERVER: OnceLock<AndroidServerHandle> = OnceLock::new();

#[cfg(feature = "mobile-bridge")]
struct AndroidServerHandle {
    #[allow(dead_code)]
    runtime: Runtime,
    port: u16,
}

#[cfg(feature = "mobile-bridge")]
pub fn ensure_mobile_bridge_server_started(default_workspace_root: PathBuf) -> Result<u16> {
    if let Some(handle) = ANDROID_SERVER.get() {
        return Ok(handle.port);
    }

    let runtime = Runtime::new()?;
    let listener =
        runtime.block_on(async { tokio::net::TcpListener::bind("127.0.0.1:0").await })?;
    let port = listener.local_addr()?.port();
    let app = MobileBridgeServer::with_workspace_root(default_workspace_root).router();

    runtime.spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let new_handle = AndroidServerHandle { runtime, port };

    if ANDROID_SERVER.set(new_handle).is_err() {
        return ANDROID_SERVER
            .get()
            .map(|handle| handle.port)
            .ok_or_else(|| anyhow!("Android bridge server failed to initialize"));
    }

    ANDROID_SERVER
        .get()
        .map(|handle| handle.port)
        .ok_or_else(|| anyhow!("Android bridge server failed to initialize"))
}

#[cfg(feature = "mobile-bridge")]
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_yuhangdo_rustagent_runtime_RustEmbeddedRuntimeBridge_nativeEnsureServerStarted(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
    app_storage_dir: jni::objects::JString,
) -> jni::sys::jint {
    let workspace_root = env
        .get_string(&app_storage_dir)
        .map(|value| value.to_string_lossy().into_owned())
        .map(PathBuf::from);

    match workspace_root
        .map_err(|error| anyhow!("Failed to read Android storage dir: {}", error))
        .and_then(ensure_mobile_bridge_server_started)
    {
        Ok(port) => port as jni::sys::jint,
        Err(error) => {
            let _ = env.throw_new("java/lang/IllegalStateException", error.to_string());
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_snapshot_uses_agent_prefix() {
        let mut state = BridgeRunState::new("run-1", "session-1");
        state.status = BridgeRunStatus::Failed;
        state.error_summary = Some("Runtime offline".to_string());
        state.answer_content = "Agent run failed: Runtime offline".to_string();

        let snapshot = state.snapshot();
        assert_eq!(snapshot.session_id, "session-1");
        assert_eq!(snapshot.status, BridgeRunStatus::Failed);
        assert_eq!(snapshot.error_summary.as_deref(), Some("Runtime offline"));
        assert_eq!(snapshot.answer_content, "Agent run failed: Runtime offline");
    }

    #[test]
    fn trim_for_event_caps_large_payloads() {
        let trimmed = trim_for_event("abcdef", 3);
        assert_eq!(trimmed, "abc");
    }
}
