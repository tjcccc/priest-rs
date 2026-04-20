use crate::errors::PriestError;
use super::model::Profile;

pub trait ProfileLoader: Send + Sync {
    fn load(&self, name: &str) -> Result<Profile, PriestError>;
}
