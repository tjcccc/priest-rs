# DEVLOG

## 2026-08-27 — v2.9.0 — Provider-executed web search

- Added `PriestRequest.provider_tools` and `ProviderToolDefinition::WebSearch`
  separately from caller-executed function tools.
- Added default-deny adapter capability checks and clear `PROVIDER_ERROR`
  responses for unsupported provider/model combinations.
- OpenAI Responses maps hosted web search before coexisting function tools;
  provider tools never enter the caller tool loop.
- Session persistence, compaction state, and SQLite interoperability are
  unchanged; no migration is required.
- Verification: all 91 tests pass, and `cargo build --release` succeeds.

## 2026-08-22 — v2.8.1 — OpenAI Responses assistant-history replay

- Corrected Responses request serialization so prior assistant text uses
  `output_text`; system and user messages continue to use `input_text`.
- Added a focused regression and advanced the crate and declared protocol
  version to `2.8.1`.
- Session persistence, schemas, and the OpenAI-compatible adapter are unchanged.
- Verification: the touched provider passes `rustfmt --check`,
  `cargo test --all-targets` passes 90 tests, and `cargo package` verifies
  the 2.8.1 crate.

## 2026-07-27 — v2.8.0 — OpenAI Responses and provider-neutral reasoning

Synced the TypeScript reference and canonical protocol 2.8.0 behavior without changing the bundled SQLite schema, timestamps, session persistence, or the existing OpenAI-compatible Chat Completions adapter.

- Added `OpenAIResponsesProvider` with configurable base/exact URL, headers, and `reqwest::Client`; stateless tool continuation; JSON output formats; function tools; semantic SSE parsing; normalized finish/errors; and duplicate tool-terminal suppression.
- Added provider-neutral reasoning config, safe summaries, opaque request-local continuation, `ReasoningSummaryDelta`, reasoning-token usage, and `content_filter`.
- Mapped neutral reasoning to OpenAI Responses, Anthropic, and Ollama. Raw Ollama traces and raw Responses reasoning content are not surfaced. `run_with_tools()` carries recognized opaque state; durable sessions remain text-only.
- Extended adapter and engine structured events for reasoning/tool/usage data. The Rust engine retains its pre-existing buffered streaming model, but parses Responses SSE correctly across transport chunks and LF/CRLF frames.
- Added focused provider, safety, usage, streaming, engine, and tool-loop tests. `cargo test --all-targets`: 89 passed. The pre-existing multimodal message-building gap remains; `PriestRequest.images` is not yet wired into providers.

## 2026-06-27 — v2.6.1 — full spec sync (compaction, turn window, cached tokens, streaming usage)

Brings priest-rs to full parity with the spec at v2.6.1 (2.5.0 → 2.6.0 → 2.6.1) on the `run()` / `complete` path, mirroring the priest-core/priest-typescript reference. All additions are off/opt-in by default; the SQLite schema is unchanged, so pre-2.5 sessions remain interoperable.

**Partial-parity caveat — streaming path:** the adapter trait exposes only `complete` + text-`stream` (no native `usage` events — the pre-existing collected-text streaming limitation). So on `stream()` / `stream_events()`, **cached tokens are not reported and conversation compaction does not trigger** (the trigger needs the previous turn's reported input, which streaming never surfaces). Both features are fully functional on `run()`. Bringing streaming to parity requires native streaming usage in the providers — a separate, larger change.

- **Cached input tokens (spec 2.5.0):** `AdapterResult.cached_input_tokens` / `UsageInfo.cached_input_tokens`. Parsed from OpenAI-compat `usage.prompt_tokens_details.cached_tokens` and Anthropic `usage.cache_read_input_tokens`. None when omitted.
- **Conversation compaction (spec 2.5.0):** new `src/compactor.rs` (`should_compact`, `plan_compaction`, `build_summary_messages`; ratio 0.8, default keep 6, summary cap 1024). `PriestConfig.max_context_tokens` enables it; a chat turn crossing 80% of the budget folds older turns into a running summary and replays only `summary + recent tail`. State persists in session `metadata["__compaction"]` with **camelCase keys** (cross-SDK contract, `session/model.rs`). `engine.compact_session()` for a manual `/compact`; trigger measured on clean chat turns only (tool-exchange replays skipped).
- **Session turn window (spec 2.6.0):** `PriestConfig.session_context_turns` caps replayed turns; the context builder windows from `max(summarized_through, len-N)` and snaps an odd window down to a user turn.
- **OpenAI-compat streaming usage (spec 2.6.1):** streaming requests send `stream_options: {include_usage: true}` (overridable via `provider_options`).
- **Bug fix (pre-existing):** `SqliteSessionStore` was hardcoding `metadata = '{}'` on save and dropping it on read — session metadata never persisted. Now round-trips the `metadata` column as JSON, which the `__compaction` interop contract requires.
- `SPEC_VERSION` → "2.6.1"; Cargo + README spec references bumped to v2.6.1.
- Tests: `tests/compaction.rs` (17 — incl. a SQLite round-trip asserting the persisted `__compaction` camelCase bytes) plus cached-token parse tests in both providers. Full `cargo test` green (79).

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
