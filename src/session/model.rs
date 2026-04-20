use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub profile_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub turns: Vec<Turn>,
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

impl Session {
    pub fn new(id: impl Into<String>, profile_name: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: id.into(),
            profile_name: profile_name.into(),
            created_at: now,
            updated_at: now,
            turns: vec![],
            metadata: HashMap::new(),
        }
    }

    pub fn append_turn(&mut self, role: impl Into<String>, content: impl Into<String>) {
        self.turns.push(Turn {
            role: role.into(),
            content: content.into(),
            timestamp: Utc::now(),
        });
        self.updated_at = Utc::now();
    }

    pub fn format_timestamp(dt: &DateTime<Utc>) -> String {
        dt.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}
