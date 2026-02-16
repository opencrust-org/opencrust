# OpenCrust Contributor Roadmap

This is a personal contribution roadmap starting on **February 16, 2026**.
It is designed to move the project from scaffold to stable core, then layer in a custom memory system.

## Current State (as of 2026-02-16)

- Workspace and crate boundaries are in place.
- Core TODOs still open:
  - WebSocket message routing to agent runtime is not implemented (`crates/opencrust-gateway/src/ws.rs`).
  - Plugin install command is TODO (`crates/opencrust-cli/src/main.rs`).
  - sqlite-vec extension loading is TODO (`crates/opencrust-db/src/vector_store.rs`).
- CI requires `check`, `test`, `clippy`, and `fmt` (`.github/workflows/ci.yml`).

## Milestones

- **M0 (Week 1):** Local environment and CI parity.
- **M1 (Weeks 2-3):** Test coverage for existing logic.
- **M2 (Weeks 4-5):** Complete core runtime flow (WS -> runtime, CLI plugin install).
- **M3 (Weeks 6-9):** Memory MVP integrated into runtime.
- **M4 (Weeks 10-12):** Memory quality improvements and evaluation.

## PR-by-PR Plan

### PR 0 - Environment Baseline

- [ ] Ensure dependency resolution works locally (disable offline cargo mode if enabled).
- [ ] Run:
  - `cargo check --workspace`
  - `cargo test --workspace`
  - `cargo clippy --workspace --all-targets`
  - `cargo fmt --all -- --check`
- [ ] Record any platform-specific setup notes in `CONTRIBUTING.md`.

File targets:
- `CONTRIBUTING.md`

Exit criteria:
- All CI-equivalent commands run locally.

### PR 1 - Security Unit Tests

- [ ] Add unit tests for prompt injection detection, sanitization, and channel id validation.
- [ ] Add unit tests for allowlist open/restricted modes.
- [ ] Add unit tests for pairing generation/claim/expiration behavior.

File targets:
- `crates/opencrust-security/src/validation.rs`
- `crates/opencrust-security/src/allowlist.rs`
- `crates/opencrust-security/src/pairing.rs`

Exit criteria:
- All new tests pass.

### PR 2 - Config and Plugin Loader Tests

- [ ] Add tests for YAML/TOML config load fallback behavior.
- [ ] Add tests for `ensure_dirs`.
- [ ] Add tests for plugin manifest discovery and invalid manifest handling.

File targets:
- `crates/opencrust-config/src/loader.rs`
- `crates/opencrust-plugins/src/loader.rs`

Exit criteria:
- Config and plugin loading are covered by deterministic tests.

### PR 3 - Media and DB Primitive Tests

- [ ] Add tests for media extension parsing and MIME mapping.
- [ ] Add migration-level tests for session/vector store table creation.

File targets:
- `crates/opencrust-media/src/types.rs`
- `crates/opencrust-db/src/session_store.rs`
- `crates/opencrust-db/src/vector_store.rs`

Exit criteria:
- DB schema creation behavior has regression protection.

### PR 4 - Implement CLI Plugin Install

- [ ] Replace TODO install command with real install logic:
  - Validate source path.
  - Copy plugin folder into `~/.opencrust/plugins/<plugin-id>`.
  - Validate `manifest.json` after copy.
- [ ] Add CLI output for success/failure paths.
- [ ] Add tests for install happy path and invalid plugin path.

File targets:
- `crates/opencrust-cli/src/main.rs`
- `crates/opencrust-plugins/src/loader.rs` (reuse/extend validation helpers)

Exit criteria:
- `opencrust plugin install <path>` works end-to-end.

### PR 5 - Gateway Message Routing

- [ ] Replace WebSocket echo with structured inbound message handling.
- [ ] Validate and sanitize inbound text using security crate.
- [ ] Route messages to agent runtime entrypoint and send runtime response over WS.
- [ ] Keep session lifecycle cleanup robust on disconnect/errors.

File targets:
- `crates/opencrust-gateway/src/ws.rs`
- `crates/opencrust-gateway/src/state.rs`
- `crates/opencrust-agents/src/runtime.rs`
- `crates/opencrust-security/src/validation.rs`

Exit criteria:
- WS path is no longer echo-only and can return runtime-generated text.

### PR 6 - Gateway Integration Tests

- [ ] Add tests for `/health`, `/api/status`, and WS connect/message/close flow.
- [ ] Add at least one test for malformed WS payload handling.

File targets:
- `crates/opencrust-gateway/src/router.rs`
- `crates/opencrust-gateway/src/ws.rs`
- `crates/opencrust-gateway/tests/gateway_integration.rs` (new)

Exit criteria:
- Core gateway behavior is covered by integration tests.

## Memory System Track (MVP -> V2)

### Memory MVP Goals

- Persist conversation history.
- Retrieve recent context quickly.
- Retrieve semantically relevant context.
- Inject retrieved context into runtime before provider call.

### PR 7 - Memory Data Model

- [ ] Introduce memory model types for entries and retrieval queries/results.
- [ ] Add schema for memory entries and optional embeddings metadata.

File targets:
- `crates/opencrust-db/src/lib.rs`
- `crates/opencrust-db/src/memory_store.rs` (new)
- `crates/opencrust-db/src/migrations.rs`

Exit criteria:
- Memory tables can be created/migrated in local and in-memory DBs.

### PR 8 - Memory Store API

- [ ] Implement `MemoryStore` with APIs:
  - `append_entry(...)`
  - `recent_messages(session_id, limit)`
  - `semantic_search(session_id, embedding, k)`
  - `delete_session_memory(session_id)`
- [ ] Add unit tests around inserts, recency ordering, and basic similarity behavior.

File targets:
- `crates/opencrust-db/src/memory_store.rs`
- `crates/opencrust-db/src/vector_store.rs` (if shared logic is needed)

Exit criteria:
- Memory API is stable and test-covered.

### PR 9 - Runtime Memory Integration

- [ ] On inbound/outbound messages, persist entries to `MemoryStore`.
- [ ] Build runtime context from:
  - recent messages
  - semantic matches
- [ ] Inject context into request payload passed to provider.

File targets:
- `crates/opencrust-gateway/src/ws.rs`
- `crates/opencrust-gateway/src/state.rs`
- `crates/opencrust-agents/src/runtime.rs`
- `crates/opencrust-agents/src/providers.rs` (if request model needs extension)

Exit criteria:
- Runtime uses memory context for completions.

### PR 10 - Memory Evaluation Harness

- [ ] Add repeatable fixtures representing real conversations.
- [ ] Add evaluation assertions for relevance and recency mix.
- [ ] Add benchmarking command/logging for retrieval latency.

File targets:
- `crates/opencrust-agents/tests/memory_eval.rs` (new)
- `crates/opencrust-db/tests/memory_store_eval.rs` (new)

Exit criteria:
- Memory retrieval quality can be measured and compared across changes.

### PR 11 - Memory V2 Improvements

- [ ] Add summarization/compaction for long sessions.
- [ ] Add score fusion strategy (recency + semantic relevance).
- [ ] Add per-channel retention policy controls in config.

File targets:
- `crates/opencrust-db/src/memory_store.rs`
- `crates/opencrust-agents/src/runtime.rs`
- `crates/opencrust-config/src/model.rs`
- `crates/opencrust-config/src/loader.rs`

Exit criteria:
- Long-running sessions remain performant with useful context quality.

## Suggested Weekly Cadence

- Monday: pick one PR scope and open a draft PR immediately.
- Midweek: land tests first, then implementation.
- Friday: run full local CI command set and tidy docs.

## Definition of Done for Any PR

- [ ] Scope is small and single-purpose.
- [ ] Tests included or updated.
- [ ] `cargo fmt`, `cargo clippy`, and `cargo test` pass locally.
- [ ] User-facing behavior documented if changed.

