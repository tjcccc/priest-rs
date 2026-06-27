use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Conversation-compaction state (spec 2.5.0) persisted inside session
/// `metadata` under this reserved key, so the SQLite schema and cross-SDK
/// interop are unchanged. The stored object uses EXACT camelCase field names —
/// a cross-SDK contract; see spec/behavior/session-lifecycle.md.
pub const COMPACTION_METADATA_KEY: &str = "__compaction";

/// Decoded view of `session.metadata["__compaction"]` (spec 2.5.0).
#[derive(Debug, Clone, Default)]
pub struct CompactionState {
    pub summary: Option<String>,
    pub summarized_through: usize,
    pub last_input_tokens: Option<u32>,
    pub updated_at: Option<String>,
}

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

    // ---- Conversation compaction (spec 2.5.0) ----

    /// Read compaction state from metadata. Empty state when unset.
    pub fn get_compaction(&self) -> CompactionState {
        match self.metadata.get(COMPACTION_METADATA_KEY) {
            Some(Value::Object(map)) => CompactionState {
                summary: map.get("summary").and_then(Value::as_str).map(String::from),
                summarized_through: map
                    .get("summarizedThrough")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize,
                last_input_tokens: map.get("lastInputTokens").and_then(Value::as_u64).map(|v| v as u32),
                updated_at: map.get("updatedAt").and_then(Value::as_str).map(String::from),
            },
            _ => CompactionState::default(),
        }
    }

    /// Serialize compaction state into metadata using the camelCase wire keys.
    fn set_compaction(&mut self, state: &CompactionState) {
        let mut map = serde_json::Map::new();
        map.insert("summarizedThrough".into(), json!(state.summarized_through));
        if let Some(summary) = &state.summary {
            map.insert("summary".into(), json!(summary));
        }
        if let Some(tokens) = state.last_input_tokens {
            map.insert("lastInputTokens".into(), json!(tokens));
        }
        if let Some(updated) = &state.updated_at {
            map.insert("updatedAt".into(), json!(updated));
        }
        self.metadata
            .insert(COMPACTION_METADATA_KEY.into(), Value::Object(map));
        self.updated_at = Utc::now();
    }

    /// Record the most recent turn's input size (the compaction trigger signal).
    pub fn record_input_tokens(&mut self, tokens: Option<u32>) {
        let Some(tokens) = tokens else { return };
        let mut state = self.get_compaction();
        state.last_input_tokens = Some(tokens);
        self.set_compaction(&state);
    }

    /// Fold turns[0 .. summarized_through) into `summary`; raw turns stay intact.
    pub fn apply_compaction(&mut self, summary: String, summarized_through: usize) {
        let mut state = self.get_compaction();
        state.summary = Some(summary);
        state.summarized_through = summarized_through;
        state.updated_at = Some(Self::format_timestamp(&Utc::now()));
        self.set_compaction(&state);
    }
}
