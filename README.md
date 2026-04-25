# priest-rs

Rust crate for the [priest](https://github.com/tjcccc/priest) AI orchestration protocol.

Rust 2021 · async/await (tokio) · Zero system dependencies

---

## Overview

`priest` is a Rust crate that implements the priest protocol spec v2.0.0 natively — no Python server, no FFI. It is designed for Rust services, CLI tools, and any async Rust host that needs to talk to a local or remote AI provider.

The core API is two methods on `PriestEngine`:

| Method | Returns | Use when |
|--------|---------|----------|
| `run(request)` | `Result<PriestResponse, PriestError>` | You need structured metadata (usage, latency, session info) |
| `stream(request)` | `Result<BoxStream<Result<String, PriestError>>, PriestError>` | You want to yield text as it arrives |

---

## Installation

```toml
[dependencies]
priest = "2.0.0"
tokio  = { version = "1", features = ["full"] }
```

Then import:

```rust
use priest::{PriestEngine, OllamaProvider, FilesystemProfileLoader};
```

---

## Quick Start

### Single run with Ollama

```rust
use std::sync::Arc;
use priest::{PriestEngine, OllamaProvider, FilesystemProfileLoader, PriestRequest, PriestConfig};

#[tokio::main]
async fn main() {
    let loader = Arc::new(FilesystemProfileLoader::new("./profiles"));

    let engine = PriestEngine::new(loader)
        .register("ollama", Box::new(OllamaProvider::default()));

    let request = PriestRequest::new(
        PriestConfig::new("ollama", "llama3.2"),
        "What is the capital of France?",
    );

    let response = engine.run(request).await.unwrap();
    if response.ok() {
        println!("{}", response.text.unwrap());
    }
}
```

### Streaming

```rust
use futures::StreamExt;

let stream = engine.stream(request).await.unwrap();
stream.for_each(|chunk| async move {
    if let Ok(text) = chunk {
        print!("{text}");
    }
}).await;
```

### Anthropic or OpenAI-compatible providers

```rust
use priest::{AnthropicProvider, OpenAICompatProvider};

let engine = PriestEngine::new(loader)
    .register("anthropic", Box::new(AnthropicProvider::new("sk-ant-...")))
    .register("openai",    Box::new(OpenAICompatProvider::new("https://api.openai.com", "sk-...")));

let request = PriestRequest {
    config: PriestConfig::new("anthropic", "claude-opus-4-6"),
    prompt: "Summarize the priest protocol in one sentence.".into(),
    ..PriestRequest::new(PriestConfig::new("anthropic", "claude-opus-4-6"), "")
};
```

---

## Session Continuity

Pass a `SessionRef` to persist conversation history across calls.

```rust
use std::sync::Arc;
use priest::{SqliteSessionStore, SessionRef};

let store = Arc::new(SqliteSessionStore::open("./sessions.db").unwrap());

let engine = PriestEngine::new(loader)
    .register("ollama", Box::new(OllamaProvider::default()))
    .with_session_store(store);

let session_id = "user-123-chat";

// First turn — session is created automatically
let mut req1 = PriestRequest::new(config.clone(), "My name is Alex.");
req1.session = Some(SessionRef::new(session_id));
let _ = engine.run(req1).await.unwrap();

// Second turn — session is continued
let mut req2 = PriestRequest::new(config.clone(), "What is my name?");
req2.session = Some(SessionRef::new(session_id));
let r = engine.run(req2).await.unwrap();
// r.text → "Your name is Alex."
```

`SessionRef` behavior:

| `continue_existing` | `create_if_missing` | Result |
|---------------------|---------------------|--------|
| `true` (default) | `true` (default) | Load existing session or create it |
| `true` | `false` | Load existing or return `Err(PriestError::SessionNotFound)` |
| `false` | — | Always create a new session |

`SqliteSessionStore` is interoperable with the Python `priest` `SqliteSessionStore` and all other priest SDK SQLite implementations — the schema and timestamp format are identical, so sessions written by Python can be read by Rust and vice versa.

---

## Profiles

A profile is a directory that supplies `identity`, `rules`, and optional `custom` and `memories` that shape the system prompt.

```
profiles/
├── default/
│   ├── PROFILE.md       ← required: identity and behavior text
│   ├── RULES.md         ← optional: strict constraints
│   ├── CUSTOM.md        ← optional: user customization layer
│   └── memories/
│       ├── 01-facts.md  ← memory files loaded in lexicographic order
│       └── 02-prefs.md
└── coder/
    └── PROFILE.md
```

```rust
let loader = Arc::new(FilesystemProfileLoader::new("./profiles"));
```

If the named directory or `PROFILE.md` is not found, `FilesystemProfileLoader` falls back to the built-in default profile when `name == "default"`, and returns `Err(PriestError::ProfileNotFound)` for any other name.

The loader caches loaded profiles per instance. Cache key: `(max_mtime, file_count)` across all profile files. Invalidates automatically when any file changes, is added, or is removed.

---

## Memory and Context

```rust
let mut req = PriestRequest::new(config, "What should I work on today?");

// Raw system context — injected first, never trimmed
req.context = vec!["Today is Monday. App: ProjectManager".into()];

// Dynamic memory — deduped against profile memories and each other
req.memory = vec!["User prefers bullet points.".into(), "Active sprint: v3.0".into()];

// Per-turn user context — appended to the user message
req.user_context = vec!["Recent tasks: [fix login bug, update README]".into()];

// Optional: trim memory to fit a system prompt budget
req.config.max_system_chars = Some(4096);
```

When `max_system_chars` is set, the engine trims `memory` entries tail-first, then `profile.memories` tail-first. `context`, rules, identity, custom, and format instructions are never trimmed.

---

## Output Format Hints

```rust
use priest::schema::request::OutputSpec;

let mut req = PriestRequest::new(config, "List three planets as JSON.");
req.output = OutputSpec {
    provider_format: Some("json".into()),  // Ollama format field / OpenAI response_format
    prompt_format:   Some("json".into()),  // Injects instruction into system prompt
    ..Default::default()
};
```

For strict schema compliance, use `json_schema` instead:

```rust
use priest::schema::request::OutputSpec;
use serde_json::json;

let mut req = PriestRequest::new(config, "Give me a person object.");
req.output = OutputSpec {
    json_schema: Some(json!({
        "type": "object",
        "properties": { "name": { "type": "string" }, "age": { "type": "integer" } },
        "required": ["name", "age"],
    })),
    json_schema_name:   "person".into(),  // optional, defaults to "response"
    json_schema_strict: false,            // true requires additionalProperties:false on all objects
    ..Default::default()
};
```

`json_schema` maps to `response_format:{type:"json_schema"}` for OpenAI-compat, `format:<schema>` for Ollama (v0.5+), and system message injection for Anthropic. It takes precedence over `provider_format` when both are set.

`response.text` is always the raw string. `priest` never parses the output.

---

## Error Handling

Two errors are always returned as `Err(...)` from `run()`/`stream()` and are never captured into `response.error`:

- `PriestError::ProviderNotRegistered` — no adapter found for the requested provider key.
- `PriestError::SessionNotFound` — session lookup failed and `create_if_missing` is `false`.

All other provider errors (network failures, rate limits, timeouts) are caught and placed into `response.error`. Check `response.ok()` before reading `response.text`.

```rust
match engine.run(request).await {
    Ok(response) => {
        if response.ok() {
            println!("{}", response.text.unwrap());
        } else {
            let err = response.error.unwrap();
            eprintln!("Provider error [{}]: {}", err.code, err.message);
        }
    }
    Err(PriestError::ProviderNotRegistered { provider }) => {
        eprintln!("No adapter registered for '{provider}'");
    }
    Err(PriestError::SessionNotFound { session_id }) => {
        eprintln!("Session '{session_id}' not found");
    }
    Err(e) => eprintln!("Unexpected: {e}"),
}
```

---

## Providers

| Key | Type | Notes |
|-----|------|-------|
| any | `OllamaProvider` | NDJSON streaming; default base URL `http://localhost:11434` |
| any | `AnthropicProvider` | SSE streaming; requires API key |
| any | `OpenAICompatProvider` | SSE streaming; works with any OpenAI-compatible endpoint |

Provider keys are arbitrary strings — the key you pass to `.register()` must match the `provider` field in `PriestConfig`.

---

## Custom Providers

Implement `ProviderAdapter` to add your own backend:

```rust
use async_trait::async_trait;
use futures::stream::BoxStream;
use priest::providers::adapter::{AdapterResult, ProviderAdapter};
use priest::context_builder::Message;
use priest::schema::{config::PriestConfig, request::OutputSpec};
use priest::errors::PriestError;

struct MyProvider;

#[async_trait]
impl ProviderAdapter for MyProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
    ) -> Result<AdapterResult, PriestError> {
        Ok(AdapterResult {
            text: "my response".into(),
            finish_reason: Some("stop".into()),
            input_tokens: None,
            output_tokens: None,
        })
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _config: &PriestConfig,
        _output_spec: &OutputSpec,
    ) -> Result<BoxStream<'static, Result<String, PriestError>>, PriestError> {
        Ok(Box::pin(futures::stream::once(async { Ok("my response".into()) })))
    }
}

let engine = PriestEngine::new(loader)
    .register("my", Box::new(MyProvider));
```

---

## Spec

`priest` targets priest protocol spec **v2.0.0**. The spec lives in the [`priest`](https://github.com/tjcccc/priest) repository under `spec/`. It defines the canonical context assembly algorithm, session schema, timestamp format, and error codes that all priest SDKs must implement identically.

```rust
priest::SPEC_VERSION  // "2.0.0"
```

---

## Requirements

- Rust 2021 edition
- An async runtime — `tokio` with `full` features is the standard choice
- No system library dependencies — `rusqlite` is bundled (builds sqlite3 from source), TLS uses `rustls` (no OpenSSL required)
