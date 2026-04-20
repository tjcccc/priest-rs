use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub identity: String,
    pub rules: String,
    pub custom: String,
    pub memories: Vec<String>,
    #[serde(default)]
    pub meta: HashMap<String, Value>,
}

impl Profile {
    pub fn new(
        name: impl Into<String>,
        identity: impl Into<String>,
        rules: impl Into<String>,
        custom: impl Into<String>,
        memories: Vec<String>,
        meta: HashMap<String, Value>,
    ) -> Self {
        Self {
            name: name.into(),
            identity: identity.into(),
            rules: rules.into(),
            custom: custom.into(),
            memories,
            meta,
        }
    }
}
