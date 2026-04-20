use async_trait::async_trait;
use crate::errors::PriestError;
use super::model::Session;

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn get(&self, id: &str) -> Result<Option<Session>, PriestError>;
    async fn create(&self, profile_name: &str, id: Option<&str>) -> Result<Session, PriestError>;
    async fn save(&self, session: &Session) -> Result<(), PriestError>;
}
