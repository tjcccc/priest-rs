use super::model::Profile;
use crate::errors::PriestError;

pub trait ProfileLoader: Send + Sync {
    fn load(&self, name: &str) -> Result<Profile, PriestError>;
}
