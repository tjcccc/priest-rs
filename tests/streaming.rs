mod mock_adapter;

use std::sync::Arc;
use futures::StreamExt;
use priest::engine::PriestEngine;
use priest::profile::default_profile::built_in_default;
use priest::profile::loader::ProfileLoader;
use priest::errors::PriestError;
use priest::schema::config::PriestConfig;
use priest::schema::request::{PriestRequest, SessionRef};
use priest::session::in_memory::InMemorySessionStore;
use priest::session::store::SessionStore;
use mock_adapter::MockAdapter;

struct FixedProfileLoader;
impl ProfileLoader for FixedProfileLoader {
    fn load(&self, name: &str) -> Result<priest::profile::model::Profile, PriestError> {
        if name == "default" { Ok(built_in_default()) }
        else { Err(PriestError::ProfileNotFound { profile: name.to_string() }) }
    }
}

fn config() -> PriestConfig { PriestConfig::new("mock", "m") }
fn request(prompt: &str) -> PriestRequest { PriestRequest::new(config(), prompt) }

#[tokio::test]
async fn stream_yields_all_chunks() {
    let engine = PriestEngine::new(Arc::new(FixedProfileLoader))
        .register("mock", Box::new(MockAdapter::ok("hello world foo")));
    let stream = engine.stream(request("hi")).await.unwrap();
    let chunks: Vec<String> = stream.map(|r| r.unwrap()).collect().await;
    assert_eq!(chunks.join(""), "hello world foo");
}

#[tokio::test]
async fn stream_saves_session_after_completion() {
    let store = Arc::new(InMemorySessionStore::new());
    let engine = PriestEngine::new(Arc::new(FixedProfileLoader))
        .register("mock", Box::new(MockAdapter::ok("response text")))
        .with_session_store(store.clone());

    let mut req = request("my prompt");
    req.session = Some(SessionRef::new("sess1"));

    let stream = engine.stream(req).await.unwrap();
    let _chunks: Vec<_> = stream.collect().await;

    // Session should be saved with 2 turns (user + assistant)
    let sess = store.get("sess1").await.unwrap().expect("session should be saved");
    assert_eq!(sess.turns.len(), 2);
    assert_eq!(sess.turns[0].role, "user");
    assert_eq!(sess.turns[0].content, "my prompt");
    assert_eq!(sess.turns[1].role, "assistant");
    assert_eq!(sess.turns[1].content, "response text");
}
