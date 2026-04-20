# DEVLOG

## 2026-04-20 — v2.0.0 — Initial implementation

First implementation of `priest-rs`, the Rust crate for the priest protocol.

Implements priest protocol spec v2.0.0 from the start (no v1.0.0 step). Reference implementation: Python `priest-core`.

**What's implemented:**
- All three providers: Ollama (NDJSON streaming), OpenAI-compatible (SSE streaming), Anthropic (SSE streaming)
- Session persistence: `InMemorySessionStore` + `SqliteSessionStore` (rusqlite bundled)
- Profile loading: `FilesystemProfileLoader` (directory-based, matches Python reference) + built-in default profile
- Profile loader cache: per-instance, keyed on `(max_mtime_nanos, file_count)`; invalidates on any file change/add/remove
- Context assembly: `build_messages()` — mirrors `context_builder.py` exactly
- `PriestEngine::run()` and `stream()` — full spec-compliant implementations
- Error types: `PriestError` (thiserror enum) with `.code()` and `.details()` helpers
- Schema types: all request/response types as structs with serde derive; `PriestResponse::ok()` computed method
- `SPEC_VERSION` constant: `"2.0.0"`

**Dependencies:** tokio, reqwest (rustls-tls), serde/serde_json, thiserror, rusqlite (bundled), async-trait, chrono, uuid, base64, futures, bytes.

**Zero system dependencies** — `rusqlite` bundled builds sqlite3 from source; `rustls-tls` avoids OpenSSL.

**Test suite:** 49 unit tests across 5 test files:
- `context_builder.rs` — 21 tests (all algorithm steps, dedup, trim, canonical strings)
- `profile_loader.rs` — 7 tests (load, default, error, cache hit with pinned mtime, cache invalidation x2, rules+custom)
- `engine.rs` — 10 tests (run, errors, session lifecycle, metadata)
- `session_store.rs` — 9 tests (InMemory x4, SQLite x5 including cross-reopen persistence)
- `streaming.rs` — 2 tests (chunks, session save after stream)

**Spec version targeted:** 2.0.0 (asserted in `SPEC_VERSION`).
