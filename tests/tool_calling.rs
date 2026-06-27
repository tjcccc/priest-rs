//! Tool calling and tool loop tests (spec 2.4.0).

mod mock_adapter;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::BoxStream;
use priest::context_builder::{build_messages, Message};
use priest::providers::adapter::{AdapterCallOptions, AdapterResult, ProviderAdapter};
use priest::schema::config::PriestConfig;
use priest::schema::request::{OutputSpec, PriestRequest, SessionRef};
use priest::schema::tools::{ToolCall, ToolDefinition, ToolExchangeTurn};
use priest::tool_loop::{run_with_tools, ApprovalDecision, ToolExecutionResult, ToolExecutor};
use priest::{InMemorySessionStore, PriestEngine, PriestError, Profile};

fn config() -> PriestConfig {
    PriestConfig::new("mock", "test-model")
}

fn read_file_call() -> ToolCall {
    let mut arguments = serde_json::Map::new();
    arguments.insert("path".into(), serde_json::json!("a.txt"));
    ToolCall { id: "call_0".into(), name: "read_file".into(), arguments }
}

fn read_file_tool() -> ToolDefinition {
    ToolDefinition { name: "read_file".into(), description: "Read a file".into(), parameters: None }
}

fn request() -> PriestRequest {
    let mut req = PriestRequest::new(config(), "Read a.txt");
    req.tools = vec![read_file_tool()];
    req
}

struct StaticLoader;
impl priest::ProfileLoader for StaticLoader {
    fn load(&self, _name: &str) -> Result<Profile, PriestError> {
        Ok(priest::built_in_default())
    }
}

/// Scripted adapter: one AdapterResult per complete() call, recording calls.
struct ScriptedAdapter {
    results: Vec<AdapterResult>,
    cursor: Mutex<usize>,
    calls: Arc<Mutex<Vec<(Vec<Message>, Option<AdapterCallOptions>)>>>,
}

impl ScriptedAdapter {
    fn new(results: Vec<AdapterResult>) -> (Self, Arc<Mutex<Vec<(Vec<Message>, Option<AdapterCallOptions>)>>>) {
        let calls = Arc::new(Mutex::new(vec![]));
        (Self { results, cursor: Mutex::new(0), calls: calls.clone() }, calls)
    }
}

fn scripted(text: &str, finish: &str, tool_calls: Option<Vec<ToolCall>>) -> AdapterResult {
    AdapterResult {
        text: text.into(),
        finish_reason: Some(finish.into()),
        input_tokens: None,
        output_tokens: None,
        cached_input_tokens: None,
        tool_calls,
    }
}

#[async_trait]
impl ProviderAdapter for ScriptedAdapter {
    async fn complete(
        &self,
        messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
        options: Option<&AdapterCallOptions>,
    ) -> Result<AdapterResult, PriestError> {
        self.calls.lock().unwrap().push((messages.to_vec(), options.cloned()));
        let mut cursor = self.cursor.lock().unwrap();
        let result = self.results[(*cursor).min(self.results.len() - 1)].clone();
        *cursor += 1;
        Ok(result)
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
        _options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

fn engine_with(adapter: ScriptedAdapter, store: Option<Arc<InMemorySessionStore>>) -> PriestEngine {
    let mut engine = PriestEngine::new(Arc::new(StaticLoader)).register("mock", Box::new(adapter));
    if let Some(store) = store {
        engine = engine.with_session_store(store);
    }
    engine
}

#[tokio::test]
async fn tool_calls_surface_with_finished_reason() {
    let (adapter, calls) = ScriptedAdapter::new(vec![
        scripted("", "tool_calls", Some(vec![read_file_call()])),
    ]);
    let response = engine_with(adapter, None).run(request()).await.unwrap();

    assert!(response.ok());
    assert_eq!(response.tool_calls.as_ref().unwrap()[0].name, "read_file");
    assert_eq!(response.execution.finished_reason.as_deref(), Some("tool_calls"));
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded[0].1.as_ref().unwrap().tools[0].name, "read_file");
}

#[test]
fn tool_exchange_replayed_after_user_message() {
    let mut req = request();
    req.tool_exchange = vec![
        ToolExchangeTurn::Assistant { text: Some(String::new()), tool_calls: vec![read_file_call()] },
        ToolExchangeTurn::ToolResult {
            tool_call_id: "call_0".into(),
            name: "read_file".into(),
            content: "file body".into(),
            is_error: None,
        },
    ];
    let messages = build_messages(&req, &priest::built_in_default(), None);

    let n = messages.len();
    assert_eq!(messages[n - 3].role, "user");
    assert_eq!(messages[n - 2].role, "assistant");
    assert!(messages[n - 2].tool_calls.is_some());
    assert_eq!(messages[n - 1].role, "tool");
    assert_eq!(messages[n - 1].tool_call_id.as_deref(), Some("call_0"));
    assert_eq!(messages[n - 1].content, "file body");
}

#[tokio::test]
async fn session_not_persisted_while_tool_calls_pending() {
    let store = Arc::new(InMemorySessionStore::new());
    let (adapter, _) = ScriptedAdapter::new(vec![
        scripted("", "tool_calls", Some(vec![read_file_call()])),
        scripted("The file says hello.", "stop", None),
    ]);
    let engine = engine_with(adapter, Some(store.clone()));

    let mut req = request();
    req.session = Some(SessionRef { id: "s1".into(), continue_existing: true, create_if_missing: true });
    let first = engine.run(req.clone()).await.unwrap();
    assert!(first.tool_calls.is_some());
    assert!(priest::SessionStore::get(store.as_ref(), "s1").await.unwrap().unwrap().turns.is_empty());

    req.tool_exchange = vec![
        ToolExchangeTurn::Assistant { text: None, tool_calls: first.tool_calls.unwrap() },
        ToolExchangeTurn::ToolResult {
            tool_call_id: "call_0".into(),
            name: "read_file".into(),
            content: "hello".into(),
            is_error: None,
        },
    ];
    let second = engine.run(req).await.unwrap();
    assert_eq!(second.text.as_deref(), Some("The file says hello."));

    let session = priest::SessionStore::get(store.as_ref(), "s1").await.unwrap().unwrap();
    assert_eq!(session.turns.len(), 2);
    assert_eq!(session.turns[0].role, "user");
    assert_eq!(session.turns[0].content, "Read a.txt");
}

struct RecordingExecutor {
    executed: Mutex<Vec<ToolCall>>,
    deny: bool,
}

#[async_trait]
impl ToolExecutor for RecordingExecutor {
    async fn execute(&self, call: &ToolCall) -> ToolExecutionResult {
        self.executed.lock().unwrap().push(call.clone());
        ToolExecutionResult { content: "hello".into(), is_error: false }
    }

    async fn approve(&self, _call: &ToolCall) -> ApprovalDecision {
        if self.deny {
            ApprovalDecision { approved: false, reason: Some("not allowed".into()) }
        } else {
            ApprovalDecision { approved: true, reason: None }
        }
    }
}

#[tokio::test]
async fn run_with_tools_executes_and_returns_final_response() {
    let (adapter, calls) = ScriptedAdapter::new(vec![
        scripted("", "tool_calls", Some(vec![read_file_call()])),
        scripted("The file says hello.", "stop", None),
    ]);
    let engine = engine_with(adapter, None);
    let executor = RecordingExecutor { executed: Mutex::new(vec![]), deny: false };

    let result = run_with_tools(&engine, request(), &executor, None).await.unwrap();

    assert_eq!(executor.executed.lock().unwrap().len(), 1);
    assert_eq!(result.response.text.as_deref(), Some("The file says hello."));
    assert!(!result.iteration_limit_reached);
    assert!(matches!(result.exchange[0], ToolExchangeTurn::Assistant { .. }));
    assert!(matches!(result.exchange[1], ToolExchangeTurn::ToolResult { .. }));
    // second engine call replayed the exchange
    let recorded = calls.lock().unwrap();
    assert!(recorded[1].0.iter().any(|m| m.role == "tool"));
}

#[tokio::test]
async fn run_with_tools_denial_injects_error_result() {
    let (adapter, _) = ScriptedAdapter::new(vec![
        scripted("", "tool_calls", Some(vec![read_file_call()])),
        scripted("Understood.", "stop", None),
    ]);
    let engine = engine_with(adapter, None);
    let executor = RecordingExecutor { executed: Mutex::new(vec![]), deny: true };

    let result = run_with_tools(&engine, request(), &executor, None).await.unwrap();

    assert!(executor.executed.lock().unwrap().is_empty());
    let ToolExchangeTurn::ToolResult { content, is_error, .. } = &result.exchange[1] else {
        panic!("expected tool result turn");
    };
    assert_eq!(*is_error, Some(true));
    assert!(content.contains("not allowed"));
}

#[tokio::test]
async fn run_with_tools_iteration_cap() {
    let (adapter, calls) = ScriptedAdapter::new(vec![
        scripted("", "tool_calls", Some(vec![read_file_call()])),
    ]);
    let engine = engine_with(adapter, None);
    let executor = RecordingExecutor { executed: Mutex::new(vec![]), deny: false };

    let result = run_with_tools(&engine, request(), &executor, Some(3)).await.unwrap();

    assert!(result.iteration_limit_reached);
    assert_eq!(calls.lock().unwrap().len(), 3);
}
