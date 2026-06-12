# DEVLOG

## 2026-06-12 — v2.4.0 — tool calling, structured streaming (spec 2.4.0 sync)

Syncs the spec 2.4.0 features (reference: priest-typescript / Python priest-core 2.4.0).

- **Tool calling (caller executes):** `PriestRequest.tools` / `tool_choice` / `tool_exchange`, `PriestResponse.tool_calls`, `finished_reason: "tool_calls"`. Wire mappings for all three providers (OpenAI tools with JSON-string arguments, Anthropic tool_use/tool_result with merged user messages, Ollama tools with synthesized `call_N` ids and `tool_name` results). Tool exchange turns are never persisted in sessions.
- **`run_with_tools()` + `ToolExecutor` trait** (`src/tool_loop.rs`): generic call → execute → re-call loop with a default-approve `approve` hook, iteration cap, and exchange trace.
- **`PriestEngine::stream_events()`:** structured event stream ending in `Done` with the full `PriestResponse`. Like the existing `stream()`, the provider stream is collected before emission; native streaming tool-call deltas are not yet surfaced in this SDK — use `run()` / `run_with_tools()` for tool calling.
- **Cancellation:** Rust maps the spec's cancellation concept to dropping the future/stream; `REQUEST_ABORTED` and `IMAGE_LOAD_ERROR` error variants added for code-table parity.
- `AdapterCallOptions` added to the adapter trait (breaking for third-party adapters; all known implementers are in-house).
- `SPEC_VERSION` → "2.4.0". Tests: 6 new in `tests/tool_calling.rs`.

Known gaps: multimodal image building (spec 2.0) and true incremental streaming remain unimplemented in this SDK.

---

## 2026-05-08 — v2.3.0 — optional profile memory loading

- Added `FilesystemProfileLoader::with_include_memories(root, false)` so host apps can load profile identity/rules/custom files without injecting `memories/`
- When memory loading is disabled, `memories/*.md` and `*.txt` files are ignored and not tracked for cache invalidation
- Updated `SPEC_VERSION` and crate version to `2.3.0`

---

## 2026-04-25 — v2.2.0 — json_schema structured output

Added `json_schema`, `json_schema_name`, and `json_schema_strict` fields to `OutputSpec` (serde defaults: `None`, `"response"`, `false`).

- **OpenAI-compat:** `response_format:{type:"json_schema", json_schema:{name, schema, strict}}` in `build_payload`.
- **Ollama (v0.5+):** `format:<schema_dict>` (schema cloned directly as `serde_json::Value`).
- **Anthropic:** `build_payload` now accepts `output_spec`; schema description injected into system string for both `complete` and `stream`.
- `json_schema` takes precedence over `provider_format` when both are set.
- `SPEC_VERSION` → `"2.2.0"`

---

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
