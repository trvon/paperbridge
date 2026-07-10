# LLM interface — implementation tasks

Tracking backlog for the audit fixes described in
[llm-interface.md](llm-interface.md). Use this as the acceptance checklist
when implementing and when re-running the audit.

Status legend: `[ ]` open · `[~]` in progress · `[x]` done · `[-]` cancelled

---

## Phase 0 — Design lock

- [x] **T0.1** Author `docs/design/llm-interface.md` (param dictionary, envelopes, open_paper, ranking, checklist)
- [x] **T0.2** Author this task backlog with acceptance criteria
- [x] **T0.3** Link design docs from `docs/design/README.md` and `Agents.md` / skill “Contributors” as required review for MCP/search/return changes
- [x] **T0.4** Open GitHub epic + PR-slice issues; keep status here in sync
  - Epic: https://github.com/trvon/paperbridge/issues/18
  - PR slices: #19–#24 (see issue list / epic body)

---

## Phase A — Contract cleanup (safe defaults, naming, envelopes)

### A1 — Param dictionary unification

- [x] **T-A1.1** Accept `query` and `q` aliases on library + papers search (MCP + CLI + service)
- [x] **T-A1.2** Canonical page size param: `limit` (default **10**, max **50** agent-facing); reject or error on unbounded “return all”
- [x] **T-A1.3** Canonical per-source param: `limit_per_source` (default 10)
- [x] **T-A1.4** CLI: `--per-source` for fan-out; `--limit` is page size
- [x] **T-A1.5** CLI: `--max-results` alias of page `limit`
- [x] **T-A1.6** Unify pagination offset: `offset` canonical, `start` alias (library + papers, MCP + CLI)
- [x] **T-A1.7** Tests: clap parse + MCP param deserializers for all aliases

**Accept:** Calling either alias pair works; skill and `--help` teach only canonical names; default page size never returns full unpaged multi-source dump.

### A2 — Source wire names

- [x] **T-A2.1** Serialize `PaperSource` with canonical names (`openalex`, `openreview`, `scholarapi`, …)
- [x] **T-A2.2** Keep serde aliases for `open_alex`, `open_review`, `scholar_api`, etc.
- [x] **T-A2.3** Align schemars/MCP enum examples with canonical names
- [x] **T-A2.4** Update skill, README, USAGE examples to canonical-only

**Accept:** MCP tool schema enum values match skill; old aliases still deserialize.

### A3 — Search/list envelopes

- [x] **T-A3.1** `SearchPapersResult` always includes `has_more`, `next_offset`
- [x] **T-A3.2** Wrap library `search_items` in envelope (`hits`/`items`, total, offset, limit, has_more, next_offset)
- [x] **T-A3.3** Wrap `list_collections` similarly
- [ ] **T-A3.4** Surface Zotero total when available (`Total-Results` / equivalent) — heuristic `has_more` for now
- [x] **T-A3.5** CLI JSON matches MCP envelope (no bare arrays on primary paths)
- [ ] **T-A3.6** Migration note: breaking change for bare-array consumers; version / changelog

**Accept:** No primary agent list endpoint returns a top-level JSON array.

### A4 — Compact vs full detail

- [x] **T-A4.1** Add `detail: compact|full` (default `compact`) to `search_papers` (and library search if useful)
- [x] **T-A4.2** Compact hits omit full abstracts; authors capped
- [ ] **T-A4.3** Optional `fields` projection
- [x] **T-A4.4** MCP JSON: compact serialization for tool results
- [ ] **T-A4.5** Size budget test: 10 compact hits for a broad query stay under documented threshold

**Accept:** Default search is usable inside an LLM context window without manual `jq` surgery.

### A5 — Write schema ceremony

- [x] **T-A5.1** `ItemWriteRequest`: default empty `creators`/`tags`/`collections` when omitted (serde default + schema not required)
- [x] **T-A5.2** `ItemUpdateRequest`: `clear_parent` optional, default `false`
- [x] **T-A5.3** Validation still rejects truly invalid creates; tests for minimal create payload
- [ ] **T-A5.4** Update skill write examples

**Accept:** Minimal create/update tool calls succeed without dummy empty arrays / forced `clear_parent`.

---

## Phase B — Search quality, diagnostics, ranking

### B1 — Multi-source diagnostics

- [x] **T-B1.1** Change fan-out to report per-source outcome: ok / skipped (missing key) / failed / timeout / rate_limited
- [x] **T-B1.2** Attach `diagnostics` to `SearchPapersResult`
- [x] **T-B1.3** Do not swallow all failures as empty success without diagnostics
- [x] **T-B1.4** Unit tests for skipped key-gated sources and timeout classification

**Accept:** Agent can see why Semantic Scholar produced zero hits when key unset.

### B2 — Query planner

- [ ] **T-B2.1** Classify query: DOI | arXiv | PMID | title_phrase | topic (shared helper) — partial via DOI/arXiv normalize + arXiv adapter heuristics
- [x] **T-B2.2** DOI/arXiv short-circuit dispatch (already partial — complete + tests)
- [x] **T-B2.3** Title-phrase detection (quoted or multi-word → arXiv `ti:`)
- [ ] **T-B2.4** Planner unit tests (dedicated classifier module)

### B3 — Per-source adapters

- [x] **T-B3.1** arXiv: `ti:` / `id:` strategies; stop sole reliance on `all:{phrase}` for long titles
- [x] **T-B3.2** Crossref: `query.bibliographic` for multi-word titles
- [ ] **T-B3.3** OpenAlex: keep relevance; improve id attachment where API provides canonical ids
- [ ] **T-B3.4** Integration tests with wiremock for adapter URL shapes

### B4 — Ranking + match metadata

- [x] **T-B4.1** Expose `match.kind` + score on hits (exact_id / exact_title / phrase / tokens / weak)
- [x] **T-B4.2** Ranking order already id > exact title > phrase > tokens > cites (pre-existing)
- [ ] **T-B4.3** Year/DOI sanity: do not let meme phrase + cites bury exact title when present
- [x] **T-B4.4** Regression: exact title ranking unit test exists
- [x] **T-B4.5** Regression: arXiv id ranking unit test exists

### B5 — ID hygiene + confidence

- [x] **T-B5.1** Normalize DOI/arXiv when minting `hit_id` / `ids`
- [ ] **T-B5.2** Best-record merge (prefer complete id set, verified fields) instead of pure first-wins when safe
- [ ] **T-B5.3** Optional `doi_status` / confidence flags for suspicious DOIs
- [x] **T-B5.4** Prefer versionless arXiv ids in `ids` and `hit_id`

### B6 — hit_id + next actions

- [x] **T-B6.1** Mint `hit_id` per design rules on every paper hit
- [x] **T-B6.2** Populate `ids` object and `access` flags
- [x] **T-B6.3** Populate `next` suggested tools for agents
- [ ] **T-B6.4** Library item summaries get stable keys already (`key`); add envelope-level `next` if useful

**Accept:** Golden queries in design verification corpus pass under mock + optional live smoke.

---

## Phase C — Execution interface (discover → read)

### C1 — `open_paper` tool + CLI

- [x] **T-C1.1** Service API: resolve hit_id | doi | arxiv | item_key | paper_id | attachment_key
- [x] **T-C1.2** Await OA/cache materialization (agent path uses synchronous ingestion/direct extraction; auto-mirror remains background)
- [x] **T-C1.3** MCP tool `open_paper` with `want`, `max_chars`, `selector`
- [x] **T-C1.4** CLI `papers open` (canonical) mirroring MCP
- [x] **T-C1.5** Errors with recovery text when id not openable
- [ ] **T-C1.6** Tests: open by arXiv id, DOI, cache paper_id, zotero key

### C2 — Fulltext safety

- [x] **T-C2.1** `max_chars` + `total_chars` / `indexed_chars` on open fulltext returns
- [x] **T-C2.2** Default max on open fulltext path (8000)
- [ ] **T-C2.3** Optional offset/continuation token or “next chunk” guidance
- [ ] **T-C2.4** Align `get_pdf_text` / `get_item_fulltext` with same truncation options (low-level)

### C3 — Structure path clarity

- [x] **T-C3.1** `open_paper want=structure` reuses `get_paper_structure`
- [ ] **T-C3.2** Selector errors list available top-level keys + examples
- [x] **T-C3.3** Accept DOI/arXiv/paper_id keys via structure path when cached/Zotero resolve works

### C4 — Background mirror policy

- [x] **T-C4.1** Keep opportunistic mirror for search if desired, but document it as best-effort
- [x] **T-C4.2** Agent-facing open path does not depend on race for metadata; fulltext needs cache/Zotero
- [ ] **T-C4.3** Return `cache` status from open when materialization runs

**Accept:** From `search_papers` hit alone, agent can open fulltext/structure with one tool call and no Zotero write.

---

## Phase D — Tool surface, skill honesty, Vox/docs

### D1 — Descriptions match behavior

- [ ] **T-D1.1** Fix `prepare_search_result_for_vox` description (papers-first, not “Search Zotero” only) or rename
- [ ] **T-D1.2** Skill: separate “plain fulltext/structure” from “Vox read-aloud”
- [ ] **T-D1.3** Skill: never teach `library read` as fulltext
- [ ] **T-D1.4** Add plain fulltext CLI if missing (`library fulltext` or `papers open`)
- [ ] **T-D1.5** Server instructions string: 6-tool spine + skill prompt

### D2 — Default tool spine / progressive disclosure

- [ ] **T-D2.1** Document primary vs secondary tools in skill frontmatter/body
- [ ] **T-D2.2** Optionally group or annotate write/vox tools as secondary in descriptions
- [ ] **T-D2.3** Ensure `paperbridge_skill` prompt content regenerated/synced from `docs/skill.md`

### D3 — Library search quality

- [ ] **T-D3.1** Default exclude attachment-only noise (or filter `itemType` intelligently)
- [ ] **T-D3.2** Envelope + totals (ties A3)
- [ ] **T-D3.3** Actionable empty-result message (try papers search / broader q)

### D4 — Errors

- [ ] **T-D4.1** Structured error helper shared by CLI stderr/JSON and MCP messages
- [ ] **T-D4.2** Audit top error paths for `try: []` next steps (config, missing key, no PDF, 412 version)

### D5 — Docs matrix

- [ ] **T-D5.1** Update `docs/skill.md`, README, USAGE, CHANGELOG
- [ ] **T-D5.2** CLI design checklist review for any command renames
- [ ] **T-D5.3** `tests/cli_surface.rs` snapshots if help text changes
- [ ] **T-D5.4** Agents.md: reference llm-interface design for MCP/return changes

---

## Phase E — Verification & re-audit

- [ ] **T-E1** Automated tests covering design verification corpus (mock-first)
- [ ] **T-E2** Optional live smoke script (`scripts/llm-interface-smoke.sh`) for Attention / arXiv id / DOI / diagnostics
- [ ] **T-E3** Token-size measurement before/after for default `search_papers`
- [ ] **T-E4** Re-run full LLM interface audit; file residual issues as new tasks
- [ ] **T-E5** `cargo fmt`, `clippy -D warnings`, `cargo test`, `cargo check --tests`

---

## Suggested implementation PR slices

| PR | GitHub | Tasks | Risk |
|----|--------|-------|------|
| PR1 | [#19](https://github.com/trvon/paperbridge/issues/19) | A1–A2, A5 (params + sources + write defaults) | Low–med (schema) |
| PR2 | [#20](https://github.com/trvon/paperbridge/issues/20) | A3–A4 (envelopes + compact) | Med (breaking JSON) |
| PR3 | [#21](https://github.com/trvon/paperbridge/issues/21) | B1 + B6 (diagnostics + hit_id) | Low–med |
| PR4 | [#22](https://github.com/trvon/paperbridge/issues/22) | B2–B5 (planner, adapters, ranking) | Med |
| PR5 | [#23](https://github.com/trvon/paperbridge/issues/23) | C1–C4 (open_paper + fulltext safety) | Med–high |
| PR6 | [#24](https://github.com/trvon/paperbridge/issues/24) | D* + E* (skill, docs, smoke, re-audit) | Low |

Epic: [#18](https://github.com/trvon/paperbridge/issues/18)

---

## Residual / out of scope (track only)

- Host-side progressive tool loading (MCP client feature)
- Guaranteed OpenAlex/Crossref metadata correctness for third-party data
- OCR quality
- Vox playback lifecycle
