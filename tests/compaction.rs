//! Conversation compaction + session turn window (spec 2.5.0 / 2.6.0).

use async_trait::async_trait;
use futures::stream::BoxStream;
use std::sync::{Arc, Mutex};

use priest::compactor::{build_summary_messages, plan_compaction, should_compact};
use priest::context_builder::{build_messages, Message};
use priest::engine::PriestEngine;
use priest::errors::PriestError;
use priest::profile::default_profile::built_in_default;
use priest::profile::loader::ProfileLoader;
use priest::profile::model::Profile;
use priest::providers::adapter::{AdapterCallOptions, AdapterResult, ProviderAdapter};
use priest::schema::config::PriestConfig;
use priest::schema::request::{OutputSpec, PriestRequest, SessionRef};
use priest::session::in_memory::InMemorySessionStore;
use priest::session::model::{Session, Turn};
use priest::session::store::SessionStore;
use priest::session::sqlite::SqliteSessionStore;
use tempfile::TempDir;

const SUMMARY_MARKER: &str = "compress prior conversation";

struct FixedProfileLoader;
impl ProfileLoader for FixedProfileLoader {
    fn load(&self, _name: &str) -> Result<Profile, PriestError> {
        Ok(built_in_default())
    }
}

type Calls = Arc<Mutex<Vec<Vec<Message>>>>;

struct ProgrammableAdapter {
    input_tokens: u32,
    calls: Calls,
}

fn is_summary(messages: &[Message]) -> bool {
    messages
        .first()
        .map(|m| m.content.contains(SUMMARY_MARKER))
        .unwrap_or(false)
}

#[async_trait]
impl ProviderAdapter for ProgrammableAdapter {
    async fn complete(
        &self,
        messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
        _options: Option<&AdapterCallOptions>,
    ) -> Result<AdapterResult, PriestError> {
        self.calls.lock().unwrap().push(messages.to_vec());
        let summary = is_summary(messages);
        Ok(AdapterResult {
            text: if summary { "SUMMARY".into() } else { "assistant reply".into() },
            finish_reason: Some("stop".into()),
            input_tokens: Some(if summary { 5 } else { self.input_tokens }),
            output_tokens: Some(5),
            cached_input_tokens: None,
            tool_calls: None,
        })
    }

    async fn stream(
        &self,
        messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
        _options: Option<&AdapterCallOptions>,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        self.calls.lock().unwrap().push(messages.to_vec());
        Ok(Box::pin(futures::stream::once(async move {
            Ok("assistant reply".to_string())
        })))
    }
}

fn budget_config() -> PriestConfig {
    let mut c = PriestConfig::new("mock", "test-model");
    c.max_context_tokens = Some(100);
    c.compaction_keep_turns = Some(2);
    c
}

fn turn(role: &str, content: &str) -> Turn {
    Turn {
        role: role.into(),
        content: content.into(),
        timestamp: chrono::Utc::now(),
    }
}

fn engine_with(store: Arc<dyn SessionStore>, input_tokens: u32) -> (PriestEngine, Calls) {
    let calls: Calls = Arc::new(Mutex::new(vec![]));
    let adapter = ProgrammableAdapter { input_tokens, calls: calls.clone() };
    let engine = PriestEngine::new(Arc::new(FixedProfileLoader))
        .with_session_store(store)
        .register("mock", Box::new(adapter));
    (engine, calls)
}

fn req(config: &PriestConfig, prompt: &str) -> PriestRequest {
    let mut r = PriestRequest::new(config.clone(), prompt);
    r.session = Some(SessionRef::new("s"));
    r
}

// ── Compactor (pure) ──────────────────────────────────────────────────────────

#[test]
fn should_compact_off_without_budget_or_measured_turn() {
    assert!(!should_compact(Some(10_000), None));
    assert!(!should_compact(Some(10_000), Some(0)));
    assert!(!should_compact(None, Some(1000)));
}

#[test]
fn should_compact_fires_only_above_80_percent() {
    assert!(!should_compact(Some(799), Some(1000)));
    assert!(should_compact(Some(801), Some(1000)));
}

#[test]
fn plan_compaction_none_while_history_fits() {
    let turns = vec![turn("user", "a"), turn("assistant", "b")];
    assert!(plan_compaction(&turns, 0, 2).is_none());
}

#[test]
fn plan_compaction_folds_before_tail_and_advances() {
    let turns = vec![turn("user", "u1"), turn("assistant", "a1"), turn("user", "u2"), turn("assistant", "a2")];
    let plan = plan_compaction(&turns, 0, 2).unwrap();
    assert_eq!(plan.summarized_through, 2);
    assert_eq!(plan.to_summarize.iter().map(|t| t.content.clone()).collect::<Vec<_>>(), vec!["u1", "a1"]);
}

#[test]
fn plan_compaction_recursive_only_folds_after_summarized() {
    let turns = vec![
        turn("user", "u1"), turn("assistant", "a1"),
        turn("user", "u2"), turn("assistant", "a2"),
        turn("user", "u3"), turn("assistant", "a3"),
    ];
    let plan = plan_compaction(&turns, 2, 2).unwrap();
    assert_eq!(plan.summarized_through, 4);
    assert_eq!(plan.to_summarize.iter().map(|t| t.content.clone()).collect::<Vec<_>>(), vec!["u2", "a2"]);
}

#[test]
fn build_summary_messages_merges_existing_and_includes_new_turns() {
    let messages = build_summary_messages(Some("prior synopsis"), &[turn("user", "hello"), turn("assistant", "hi there")]);
    assert!(messages[0].content.contains(SUMMARY_MARKER));
    assert!(messages[1].content.contains("prior synopsis"));
    assert!(messages[1].content.contains("hello"));
    assert!(messages[1].content.contains("hi there"));
}

// ── Engine compaction ─────────────────────────────────────────────────────────

#[tokio::test]
async fn compacts_over_budget_chat_and_replays_summary_plus_tail() {
    let store = Arc::new(InMemorySessionStore::new());
    let (engine, calls) = engine_with(store.clone(), 200);
    let config = budget_config();

    for prompt in ["msg1", "msg2", "msg3"] {
        engine.run(req(&config, prompt)).await.unwrap();
    }

    let session = store.get("s").await.unwrap().unwrap();
    assert_eq!(session.get_compaction().summary.as_deref(), Some("SUMMARY"));

    let recorded = calls.lock().unwrap();
    assert!(recorded.iter().any(|m| is_summary(m)));
    let last_chat = recorded.iter().rev().find(|m| !is_summary(m)).unwrap();
    assert!(last_chat[0].content.contains("## Conversation so far (summary)"));
    assert!(last_chat[0].content.contains("SUMMARY"));
    assert!(!last_chat.iter().any(|m| m.content == "msg1"));
}

#[tokio::test]
async fn compaction_state_survives_sqlite_round_trip() {
    // Cross-SDK interop: state written as camelCase JSON must read back from a
    // fresh store, and the persisted bytes must use camelCase keys.
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("sessions.db");
    let store: Arc<dyn SessionStore> = Arc::new(SqliteSessionStore::open(&db).unwrap());
    let (engine, _calls) = engine_with(store, 200);
    let config = budget_config();

    for prompt in ["msg1", "msg2", "msg3"] {
        engine.run(req(&config, prompt)).await.unwrap();
    }

    // Reopen a fresh store on the same DB — forces a JSON deserialize from disk.
    let fresh = SqliteSessionStore::open(&db).unwrap();
    let session = fresh.get("s").await.unwrap().unwrap();
    let comp = session.get_compaction();
    assert_eq!(comp.summary.as_deref(), Some("SUMMARY"));
    assert_eq!(comp.summarized_through, 2);

    let serialized = serde_json::to_string(&session.metadata).unwrap();
    assert!(serialized.contains("summarizedThrough"));
    assert!(!serialized.contains("summarized_through"));
}

#[tokio::test]
async fn does_not_record_trigger_when_tool_exchange_replayed() {
    use priest::schema::tools::ToolExchangeTurn;
    let store = Arc::new(InMemorySessionStore::new());
    let (engine, _calls) = engine_with(store.clone(), 200);
    let config = budget_config();

    let mut request = req(&config, "msg1");
    request.tool_exchange = vec![ToolExchangeTurn::ToolResult {
        tool_call_id: "c1".into(),
        name: "web_search".into(),
        content: "big results".into(),
        is_error: None,
    }];
    engine.run(request).await.unwrap();

    let session = store.get("s").await.unwrap().unwrap();
    assert!(session.get_compaction().last_input_tokens.is_none());
}

#[tokio::test]
async fn never_compacts_without_budget() {
    let store = Arc::new(InMemorySessionStore::new());
    let (engine, calls) = engine_with(store.clone(), 200);
    let config = PriestConfig::new("mock", "test-model"); // no budget

    for prompt in ["msg1", "msg2", "msg3", "msg4"] {
        engine.run(req(&config, prompt)).await.unwrap();
    }

    let session = store.get("s").await.unwrap().unwrap();
    assert!(session.get_compaction().summary.is_none());
    assert!(!calls.lock().unwrap().iter().any(|m| is_summary(m)));
}

#[tokio::test]
async fn compact_session_folds_on_demand_and_reports_coverage() {
    let store = Arc::new(InMemorySessionStore::new());
    let (engine, _calls) = engine_with(store.clone(), 10); // small input — no auto-compaction
    let no_budget = PriestConfig::new("mock", "test-model");

    for prompt in ["msg1", "msg2", "msg3"] {
        engine.run(req(&no_budget, prompt)).await.unwrap();
    }
    assert!(store.get("s").await.unwrap().unwrap().get_compaction().summary.is_none());

    let mut config = PriestConfig::new("mock", "test-model");
    config.compaction_keep_turns = Some(2);
    let (compacted, summarized_through) = engine.compact_session("s", &config).await.unwrap();
    assert!(compacted);
    assert_eq!(summarized_through, 4); // 6 turns − keep 2
    assert_eq!(store.get("s").await.unwrap().unwrap().get_compaction().summary.as_deref(), Some("SUMMARY"));
}

#[tokio::test]
async fn compact_session_errors_for_unknown_session() {
    let store = Arc::new(InMemorySessionStore::new());
    let (engine, _calls) = engine_with(store, 10);
    let err = engine.compact_session("nope", &PriestConfig::new("mock", "m")).await.unwrap_err();
    assert!(matches!(err, PriestError::SessionNotFound { .. }));
}

// ── Session turn window (spec 2.6.0) ──────────────────────────────────────────

fn session_with(n: usize) -> Session {
    let mut s = Session::new("s", "default");
    for i in 0..n {
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        s.append_turn(role, format!("turn-{i}"));
    }
    s
}

fn window_request(n: Option<usize>) -> PriestRequest {
    let mut c = PriestConfig::new("mock", "m");
    c.session_context_turns = n;
    PriestRequest::new(c, "Hi")
}

fn replayed(msgs: &[Message]) -> Vec<String> {
    let body: Vec<&Message> = msgs.iter().filter(|m| m.role != "system").collect();
    body[..body.len() - 1].iter().map(|m| m.content.clone()).collect()
}

#[test]
fn replays_all_turns_when_window_unset() {
    let msgs = build_messages(&window_request(None), &built_in_default(), Some(&session_with(6)));
    assert_eq!(replayed(&msgs), vec!["turn-0", "turn-1", "turn-2", "turn-3", "turn-4", "turn-5"]);
}

#[test]
fn replays_only_last_n_turns() {
    let msgs = build_messages(&window_request(Some(2)), &built_in_default(), Some(&session_with(6)));
    assert_eq!(replayed(&msgs), vec!["turn-4", "turn-5"]);
}

#[test]
fn replays_no_turns_when_window_is_zero() {
    let msgs = build_messages(&window_request(Some(0)), &built_in_default(), Some(&session_with(6)));
    assert!(replayed(&msgs).is_empty());
}

#[test]
fn snaps_odd_window_down_to_user_turn() {
    let msgs = build_messages(&window_request(Some(5)), &built_in_default(), Some(&session_with(8)));
    let first_replayed = msgs.iter().find(|m| m.role != "system").unwrap();
    assert_eq!(first_replayed.role, "user");
    assert_eq!(replayed(&msgs), vec!["turn-2", "turn-3", "turn-4", "turn-5", "turn-6", "turn-7"]);
}

#[test]
fn window_never_unhides_summarized_turns() {
    let mut session = session_with(6);
    session.apply_compaction("earlier conversation summary".into(), 4);
    let msgs = build_messages(&window_request(Some(5)), &built_in_default(), Some(&session));
    assert_eq!(replayed(&msgs), vec!["turn-4", "turn-5"]);
    assert!(msgs[0].content.contains("earlier conversation summary"));
}
