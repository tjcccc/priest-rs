mod mock_adapter;

use std::sync::Arc;
use priest::engine::PriestEngine;
use priest::errors::PriestError;
use priest::profile::default_profile::built_in_default;
use priest::profile::loader::ProfileLoader;
use priest::schema::config::PriestConfig;
use priest::schema::request::{PriestRequest, SessionRef};
use priest::session::in_memory::InMemorySessionStore;
use mock_adapter::MockAdapter;

struct FixedProfileLoader;
impl ProfileLoader for FixedProfileLoader {
    fn load(&self, name: &str) -> Result<priest::profile::model::Profile, PriestError> {
        if name == "default" {
            Ok(built_in_default())
        } else {
            Err(PriestError::ProfileNotFound { profile: name.to_string() })
        }
    }
}

fn engine_with_mock(text: &str) -> PriestEngine {
    PriestEngine::new(Arc::new(FixedProfileLoader))
        .register("mock", Box::new(MockAdapter::ok(text)))
}

fn config() -> PriestConfig { PriestConfig::new("mock", "m") }

fn request(prompt: &str) -> PriestRequest { PriestRequest::new(config(), prompt) }

// ── Basic run ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_returns_response_text() {
    let engine = engine_with_mock("hello world");
    let resp = engine.run(request("hi")).await.unwrap();
    assert!(resp.ok());
    assert_eq!(resp.text.unwrap(), "hello world");
}

#[tokio::test]
async fn run_returns_execution_info() {
    let engine = engine_with_mock("ok");
    let resp = engine.run(request("hi")).await.unwrap();
    assert_eq!(resp.execution.provider, "mock");
    assert_eq!(resp.execution.model, "m");
    assert_eq!(resp.execution.profile, "default");
    assert_eq!(resp.execution.finished_reason.as_deref(), Some("stop"));
}

#[tokio::test]
async fn run_returns_usage_info() {
    let engine = engine_with_mock("ok");
    let resp = engine.run(request("hi")).await.unwrap();
    let usage = resp.usage.unwrap();
    assert_eq!(usage.input_tokens, Some(10));
    assert_eq!(usage.output_tokens, Some(5));
    assert_eq!(usage.total_tokens, Some(15));
}

// ── Provider not registered ───────────────────────────────────────────────────

#[tokio::test]
async fn run_errors_when_provider_not_registered() {
    let engine = PriestEngine::new(Arc::new(FixedProfileLoader));
    let result = engine.run(request("hi")).await;
    assert!(matches!(result, Err(PriestError::ProviderNotRegistered { .. })));
}

// ── Provider error captured in response ──────────────────────────────────────

#[tokio::test]
async fn provider_error_captured_in_response() {
    let engine = PriestEngine::new(Arc::new(FixedProfileLoader))
        .register("mock", Box::new(MockAdapter::failing(
            PriestError::ProviderError { provider: "mock".into(), message: "boom".into() }
        )));
    let resp = engine.run(request("hi")).await.unwrap();
    assert!(!resp.ok());
    assert!(resp.error.is_some());
    assert_eq!(resp.execution.finished_reason.as_deref(), Some("error"));
}

// ── Metadata passthrough ──────────────────────────────────────────────────────

#[tokio::test]
async fn metadata_echoed_in_response() {
    let engine = engine_with_mock("ok");
    let mut req = request("hi");
    req.metadata.insert("key".into(), serde_json::json!("value"));
    let resp = engine.run(req).await.unwrap();
    assert_eq!(resp.metadata["key"], serde_json::json!("value"));
}

// ── Session handling ──────────────────────────────────────────────────────────

#[tokio::test]
async fn run_creates_session_on_first_call() {
    let store = Arc::new(InMemorySessionStore::new());
    let engine = PriestEngine::new(Arc::new(FixedProfileLoader))
        .register("mock", Box::new(MockAdapter::ok("resp")))
        .with_session_store(store.clone());

    let mut req = request("hi");
    req.session = Some(SessionRef::new("s1"));
    let resp = engine.run(req).await.unwrap();

    let sess_info = resp.session.unwrap();
    assert!(sess_info.is_new);
    assert_eq!(sess_info.id, "s1");
    assert_eq!(sess_info.turn_count, 2); // user + assistant
}

#[tokio::test]
async fn run_continues_session_on_second_call() {
    let store = Arc::new(InMemorySessionStore::new());
    let engine = PriestEngine::new(Arc::new(FixedProfileLoader))
        .register("mock", Box::new(MockAdapter::ok("resp")))
        .with_session_store(store.clone());

    let mut req = request("first");
    req.session = Some(SessionRef::new("s2"));
    let _ = engine.run(req).await.unwrap();

    // Register fresh mock for second call
    let engine2 = PriestEngine::new(Arc::new(FixedProfileLoader))
        .register("mock", Box::new(MockAdapter::ok("resp2")))
        .with_session_store(store.clone());

    let mut req2 = request("second");
    req2.session = Some(SessionRef::new("s2"));
    let resp2 = engine2.run(req2).await.unwrap();
    let sess_info = resp2.session.unwrap();
    assert!(!sess_info.is_new);
    assert_eq!(sess_info.turn_count, 4); // 2 from first + 2 from second
}

#[tokio::test]
async fn session_not_found_errors_when_create_if_missing_false() {
    let store = Arc::new(InMemorySessionStore::new());
    let engine = PriestEngine::new(Arc::new(FixedProfileLoader))
        .register("mock", Box::new(MockAdapter::ok("resp")))
        .with_session_store(store.clone());

    let mut req = request("hi");
    req.session = Some(SessionRef { id: "ghost".into(), continue_existing: true, create_if_missing: false });
    let result = engine.run(req).await;
    assert!(matches!(result, Err(PriestError::SessionNotFound { .. })));
}

#[tokio::test]
async fn no_session_info_in_response_when_no_session_store() {
    let engine = engine_with_mock("ok");
    let mut req = request("hi");
    req.session = Some(SessionRef::new("s1"));
    let resp = engine.run(req).await.unwrap();
    assert!(resp.session.is_none());
}
