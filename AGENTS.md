# paperbridge — Agent Instructions

## Project Overview

paperbridge is a lightweight Rust MCP + CLI bridge for searching and retrieving Zotero library content for LLM workflows.

Primary goal for v1:
- make it easy to find items, fetch PDF/full-text content, and hand the resulting text to Vox

Boundary with Vox:
- keep Vox integration shallow
- do not tightly couple transport/session/state between paperbridge and vox
- paperbridge returns clean text/chunks; callers choose when/how to call Vox

## Build & Test

```bash
cargo check                   # type-check only
cargo test                    # run all tests
cargo clippy -- -D warnings   # lint — must pass with zero warnings
cargo fmt --check             # formatting gate
cargo check --tests           # compile test targets
```

The first three must pass cleanly before submitting changes.

## Architecture (Component-Oriented)

| Module | Purpose |
|--------|---------|
| `main.rs` | Entry point, config loading, MCP startup |
| `cli.rs` | Clap CLI parser (`serve`, `query`, `collections`, `read*`, `config`) |
| `server.rs` | MCP tool handlers and response mapping |
| `service.rs` | Shared application service used by MCP tools and standalone CLI |
| `zotero_api.rs` | Typed Zotero Web API client (auth, pagination, retries/backoff) |
| `models.rs` | DTOs for Zotero responses + MCP output models |
| `pdf.rs` | Attachment/PDF retrieval helpers and text extraction orchestration |
| `chunking.rs` | Deterministic long-text chunking for read-aloud workflows |
| `config.rs` | TOML config + env overrides (`PAPERBRIDGE_*`, legacy `ZOTERO_MCP_*`) |
| `error.rs` | `thiserror`-based error enum and user-safe conversion |
| `lib.rs` | Re-exports for tests/support code |

## MCP Tool Scope (v1)

Keep scope tight and practical:

- `search_items` — metadata search (title/creator/year/tag/collection)
- `list_collections` — collection discovery (top-level or all)
- `get_item` — item metadata + child attachment references
- `get_item_fulltext` — indexed full-text for attachment when available
- `get_pdf_text` — fetch/extract text from target PDF attachment
- `prepare_vox_text` — return normalized chunks ready for Vox `say`
- `prepare_item_for_vox` — choose best attachment for an item and return Vox-ready chunks
- `prepare_search_result_for_vox` — query, select result, and return Vox-ready chunks

Notes:
- `prepare_vox_text` returns text payload/chunks only; it should not call Vox directly
- direct Zotero write operations are out of scope for initial release

## Code Conventions

- **Edition 2024**
- **Visibility**: prefer `pub(crate)` over broad `pub`
- **Errors**: central crate-specific error enum using `thiserror`
- **Clippy**: treat all warnings as errors (`-D warnings`)
- **Tests**: inline `#[cfg(test)] mod tests` where practical; use dedicated integration tests for HTTP behavior
- **Comments**: add only when needed for non-obvious logic
- Avoid `unwrap()`/`expect()` in production paths; surface actionable errors

## Config Precedence

1. Compiled defaults (`Config::default()`)
2. TOML file (`$XDG_CONFIG_HOME/paperbridge/config.toml`)
3. Environment variables (`PAPERBRIDGE_*`)

Expected keys:

- `PAPERBRIDGE_API_KEY`
- `PAPERBRIDGE_LIBRARY_TYPE` (`user` or `group`)
- `PAPERBRIDGE_USER_ID`
- `PAPERBRIDGE_GROUP_ID`
- `PAPERBRIDGE_API_BASE` (default `https://api.zotero.org`)
- `PAPERBRIDGE_LOG_LEVEL`

## Testing Strategy

- Unit tests for:
  - query construction
  - config parsing/precedence
  - chunking behavior and edge cases
  - error mapping
- Integration tests (mock HTTP server) for:
  - pagination
  - rate limiting/backoff (`429`, `Backoff`, `Retry-After`)
  - conditional requests (`If-Modified-Since-Version`, `304`)
  - missing full-text / attachment paths
- No live Zotero API calls in CI

## CI Expectations

CI should run at minimum:

- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo check --tests`

Prefer also running full `cargo test` in CI when runtime is acceptable.

## Known Limitations (Initial)

- API throughput and availability are constrained by Zotero rate limits
- Some attachments may not have indexed full-text content
- OCR quality is external to this project (depends on source PDFs / Zotero indexing)
- Vox handoff is intentionally decoupled; playback lifecycle remains in Vox/client layer
