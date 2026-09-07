# paperbridge — LLM / Agent Interface Design

## Purpose

Authoritative contract for MCP + CLI surfaces used by agents (and humans
piping JSON). Complements [cli-design.md](cli-design.md): that document
governs command graph and help; this one governs **wire params, result
envelopes, search quality, and discover→read execution**.

Any change to MCP tool schemas, search/list JSON shapes, source enum wire
names, skill recipes for search/open, or fulltext open paths must be
reviewed against this document.

Companion backlog: [llm-interface-tasks.md](llm-interface-tasks.md).

## Goals

- One obvious path: **search → open → query/structure**.
- Token-cheap defaults; opt into verbose fields.
- Stable IDs for follow-up tool calls (no fragile field re-copy).
- Honest diagnostics when sources skip, fail, or time out.
- Safe page sizes; never dump unbounded result sets or full PDFs by default.
- CLI and MCP share the same vocabulary (names + aliases).

## Non-goals

- Calling Vox or managing playback lifecycle.
- Tight coupling to a specific agent host beyond MCP + JSON stdout.
- Perfect multi-source ranking for every query (but ranking must not
  routinely bury exact-title / exact-ID matches when sources return them).

## Param dictionary (canonical)

| Concept | Canonical name | Aliases (accept forever) | Default | Notes |
|---------|----------------|--------------------------|---------|--------|
| Free-text / id query | `query` | `q` | required when searching | Same for library + papers |
| Page size | `limit` | — | **10** (search/list) | Max 50 for agent defaults; never default 0=all |
| Per-source fan-out | `limit_per_source` | CLI legacy `--limit` | 10 | Fixed source prefix, max 200; offset never expands it. Increase it and restart at offset 0 to broaden. CLI prefers `--per-source`. |
| Pagination offset | `offset` | `start` | 0 | Library and paper-search rows; bounded fulltext uses a returned UTF-8 byte offset |
| Source filter | `sources` | — | all enabled | Canonical wire names below |
| Cache mode | `cache` | — | `auto` | `auto` \| `include` \| `only` \| `off` |
| Detail level | `detail` | — | `compact` | `compact` \| `full` |
| Abstract cap | `abstract_max_chars` | — | 280 when abstract included | 0 = unlimited (full detail only) |

### Source wire names (canonical)

Prefer **skill/CLI forms** as the single wire form for MCP JSON + CLI:

`research`, `arxiv`, `paperseed`, `hugging_face`, `semantic_scholar`, `crossref`,
`openalex`, `europe_pmc`, `dblp`, `openreview`, `core`, `ads`, `pubmed`,
`scholarapi`.

Keep serde aliases for snake_case variants (`open_alex`, `open_review`,
`scholar_api`, …) so older clients do not break. Schema examples and skill
docs must teach **only** the canonical set.

## List / search envelope

All list and search returns (library items, collections, papers) use:

```json
{
  "query": "optional echo when applicable",
  "total_count": 26,
  "offset": 0,
  "limit": 10,
  "has_more": true,
  "next_offset": 10,
  "hits": [],
  "diagnostics": {
    "sources_ok": ["openalex", "arxiv"],
    "sources_skipped": [
      {"source": "semantic_scholar", "reason": "missing_api_key"}
    ],
    "sources_failed": [
      {"source": "core", "reason": "timeout"}
    ]
  }
}
```

Rules:

- `count_kind` is `candidate_window` for merged paper searches; this total is not the global index count. A fixed source prefix avoids pagination-induced reranking, but upstream changes can still reorder results between calls.
- Library `total_count` is nullable: `count_kind=unknown` when not proven, `exact` only when exhaustion proves the count. `has_more` uses a fetched sentinel, never an invented extra row.
- Do not return a bare JSON array for primary agent surfaces.
- `limit == 0` is rejected or clamped with an actionable error (do not mean “all”).
- `diagnostics` may be omitted for pure local library calls that cannot fail
  per-source; prefer always present with empty arrays when cheap.
- CLI `--json` output may be pretty-printed; human-readable output is the CLI
  default. MCP may use compact JSON to save tokens if the host allows (prefer
  compact for MCP when changing serializers).

## Compact hit contract (`detail=compact`)

```json
{
  "hit_id": "arxiv:1706.03762",
  "source": "arxiv",
  "title": "Attention Is All You Need",
  "authors": ["Ashish Vaswani", "Noam Shazeer"],
  "year": "2017",
  "ids": {
    "doi": "10.48550/arXiv.1706.03762",
    "arxiv": "1706.03762"
  },
  "match": {
    "kind": "exact_title",
    "score": 0.98
  },
  "access": {
    "pdf": true,
    "cached": false,
    "full_text": false
  },
  "next": ["open_paper", "resolve_doi", "get_paper_structure"]
}
```

`detail=full` may add abstract (capped unless unlimited), venue, citation_count,
urls, `oa_pdf_url`, `relevance_score`, `cache` object, etc.

### `hit_id` rules

Stable, deterministic, preferred order of minting:

1. `arxiv:{versionless_id}`
2. `doi:{normalized_doi}`
3. `zotero:{item_key}`
4. `paperseed:{paper_id}`
5. `research:{yams_hash}` for grouped YAMS research documents
6. `url:{canonical_url}` last resort. This is intentionally reversible so a
   stateless `open_paper { hit_id }` call can retrieve URL-only hits.

PMID-only records prefer a supported PDF/HTTP URL identity when available; retain `ids.pmid`. With no usable URL/DOI/cache identity, `pmid:` remains a discovery-only identifier with no advertised open action. Explicit PMID opens return actionable unsupported-resolution errors, not cache placeholders.

Same logical paper from different sources should prefer the same id when an
identifier is shared (dedupe merge must promote best id set onto the kept hit).

`access.content_state` is optional for remote results and required for local
research hits: `ready` is directly openable; `stale` means discovery metadata
survives but YAMS cannot read the indexed blob or original path. Agents must not
infer full-text availability from a search snippet.

## Query planning (search)

Before multi-source fan-out, classify `query`:

| Class | Detection | Behavior |
|-------|-----------|----------|
| DOI | DOI shape / doi.org URL | Resolve-first; optional search backup |
| arXiv | `1706.03762`, `arxiv:…`, abs URL | arXiv id path + annotate |
| PMID | all-digits PMID heuristic when scoped | PubMed-first |
| Title phrase | quoted string or Title-Case multi-word | Title-primary adapters |
| Topic | free text | Broad multi-source |

### Per-source adapters (minimum)

- **arXiv**: prefer `ti:"…"` / `id:` when class is title/id; avoid raw
  `all:{long phrase}` as the only strategy.
- **Crossref**: prefer bibliographic/title-oriented params over bare `query=`
  when class is title.
- **OpenAlex**: preserve relevance; attach verified ids; do not promote
  obviously non-canonical DOIs without flagging.

### Ranking order (descending)

1. Exact DOI / arXiv / PMID match to query
2. Exact normalized title
3. Title phrase containment (tighter titles beat looser)
4. Token coverage
5. Citation count (when present)
6. Cache BM25 / relevance_score
7. Source bias (stable tie-break only)

Expose `match.kind` so agents can refuse low-confidence top hits.

### ID hygiene

- Normalize DOIs (strip resolver prefixes; lowercase).
- Strip arXiv versions for identity.
- Flag or demote hits with high citation_count but impossible year/DOI shape
  when better-identified candidates exist (`doi_status`, `year` sanity).

## Discover → read execution

### Canonical open tool

```text
open_paper {
  hit_id? | doi? | arxiv_id? | item_key? | paper_id? | attachment_key? | url?,
  want: ["metadata" | "fulltext" | "structure" | "chunks"],
  max_chars?,
  offset?,
  selector?,
  max_chars_per_chunk?
}
```

Resolution order: explicit ids → `hit_id` parse → YAMS research bundle → exact
cache identity → Zotero → OA PDF download (**await**, not fire-and-forget) →
error with recovery steps.

Defaults:

- `want` default `["metadata"]` or `["structure"]` for paper keys (choose one
  and document; prefer `metadata` then let agent request more).
- Fulltext always respects `max_chars` (default 8,000), reports `total_chars`,
  and returns `next_offset` when another page is available. `next_offset` is a
  UTF-8 byte position; call the same tool with that exact `offset` to continue.
- `max_chars` is 1–32,000 per requested content view. Without a selector, structure returns a bounded outline with counts and `structure_page.omitted_fields`. Select against the complete tree first, then bound the selection; selected strings support UTF-8 byte offset paging. Object/array selections expose omissions and selector guidance, not a fabricated cursor.
- Chunks-only requests preserve pagination in `chunks_page`. Metadata reports `metadata_status` (`retrieved`, `identifier_only`, `failed`) and `metadata_page` completeness. Missing cache identities fail explicitly.
- `content_provenance` and `structure_provenance` separate origin/retrieval from parser. Paperseed/YAMS/direct PDF text must not be labelled Zotero.
- MCP `query_paper` wraps arbitrary selected JSON as `{value}` for an object-root schema. CLI keeps native selected JSON.
- All MCP results expose `outputSchema` and `structuredContent` with compact text compatibility. A 64 KiB whole-result budget (both representations) returns a bounded recovery error on overflow. Key/citation metadata must not be silently replaced to meet that budget.

Low-level tools (`get_pdf_text`, `get_item_fulltext`, `get_paper_structure`)
remain for power users but skill default path teaches `open_paper`.

### Library list envelope

`search_items` / `list_collections` gain the same pagination envelope.
Default library search should prefer parent works (exclude lone attachments
unless `item_type=attachment` or an explicit include flag).

## Write schema ergonomics

- `create_item`: default `creators`, `tags`, `collections` to `[]` when omitted.
- `update_item`: `clear_parent` optional, default `false`.
- Nested write params stay structured; do not require ceremony fields for
  no-op paths.

## Errors (MCP + CLI JSON)

Every user-facing error body should support:

```json
{
  "error": "what failed",
  "reason": "why if known",
  "try": ["exact next tool call or CLI command", "..."]
}
```

CLI runtime errors emit this envelope on stderr when `--json` is selected and
return a non-zero exit code; stdout remains empty. Clap parse/usage errors occur
before runtime dispatch and retain Clap's native formatting.

CLI and MCP share `{error,reason,try,retryable,recovery}`; recovery entries contain a tool name and arguments and never execute automatically. Network error bodies and parse previews are bounded/redacted before exposing them. MCP execution errors use `isError:true`; validation failures use `invalid_params` with the envelope in `data`. Oversized results provide a bounded-read recovery action when the original tool/arguments support it.

## Default MCP tool spine (skill)

Primary (discovery + read):

1. `search_items` (library)
2. `search_papers`
3. `open_paper` (new)
4. `query_paper` (or folded into open)
5. `resolve_doi`
6. `backend_info` / status

Secondary (skill sections, not default “start here”):

- Vox prepare tools
- Write tools
- Paperseed admin
- `prepare_paper_for_skill`

Server `instructions` lists the spine and points at prompt `paperbridge_skill`.
`serve --profile core` advertises and permits only these six tools; `full` remains
the compatibility default. Tool annotations distinguish mutations, destructive
operations, and reads; cache/materialization paths conservatively are not marked
read-only. Server identity uses the Paperbridge package name/version.

## Skill / docs rules

- Teach canonical param names and source wire forms only.
- Never document `library read` as plain fulltext (it is Vox chunks).
- Document `prepare_search_result_for_vox` true behavior (papers search first,
  then cache/Zotero) or rename for honesty.
- Gotchas: compact default, `limit` default 10, diagnostics, `hit_id` → open.

## Required review checklist

- [ ] Params match the dictionary (aliases only for back-compat)?
- [ ] List/search uses the envelope (not a bare array)?
- [ ] Default `limit` is bounded; compact by default?
- [ ] Hits include `hit_id` and usable `next` / ids?
- [ ] Multi-source paths emit diagnostics for skip/fail/timeout?
- [ ] Discover→read works with only `hit_id` or DOI/arXiv (no attachment key required)?
- [ ] Fulltext/structure responses are truncated or selectable by default?
- [ ] Skill + MCP descriptions match implementation?
- [ ] CLI flag names do not invert MCP meaning (`limit` vs per-source)?
- [ ] Acceptance tests in `llm-interface-tasks.md` still pass?

## Verification corpus (regression)

Minimum live or mocked cases after changes:

1. Query `Attention Is All You Need` → top hit is Vaswani et al.; id includes
   arXiv `1706.03762` and/or a verified DOI; not a meme-title paper.
2. Query bare arXiv id `1706.03762` → exact match first.
3. Query DOI → resolve path returns structured metadata; open works.
4. `search_papers` without keys → diagnostics list skipped key-gated sources;
   always-on sources still return.
5. Default search payload for broad query is compact and under a documented
   size budget (e.g. ≤ ~4–6 KB for 10 hits without full abstracts).
6. `open_paper` on OA hit returns fulltext or structure without a prior Zotero
   import race.
7. Library search returns envelope + `has_more`; attachment-only noise reduced
   by default.
8. `create_item` with only `item_type` + `title` validates/creates without
   forcing empty arrays in the client payload.
9. CLI `papers search --help` and MCP schema agree on `limit` vs
   `limit_per_source`.
10. Skill examples run against the built CLI surface.
11. Query `What drives detection in GNN` with source `research` → consolidated
    paper-2 hit first; opening its `research:` id yields structured evaluation
    content within the second tool call.
