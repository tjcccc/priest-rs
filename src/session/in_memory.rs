use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use crate::errors::PriestError;
use super::model::Session;
use super::store::SessionStore;

#[derive(Clone, Default)]
pub struct InMemorySessionStore {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

impl InMemorySessionStore {
    pub fn new() -> Self { Self::default() }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn get(&self, id: &str) -> Result<Option<Session>, PriestError> {
        Ok(self.sessions.lock().unwrap().get(id).cloned())
    }

    async fn create(&self, profile_name: &str, id: Option<&str>) -> Result<Session, PriestError> {
        let session_id = id.map(|s| s.to_string()).unwrap_or_else(|| Uuid::new_v4().to_string());
        let session = Session::new(session_id.clone(), profile_name);
        self.sessions.lock().unwrap().insert(session_id, session.clone());
        Ok(session)
    }

    async fn save(&self, session: &Session) -> Result<(), PriestError> {
        self.sessions.lock().unwrap().insert(session.id.clone(), session.clone());
        Ok(())
    }
}
