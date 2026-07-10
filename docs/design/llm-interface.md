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
| Per-source fan-out | `limit_per_source` | CLI legacy `--limit` | 10 | CLI flag rename: prefer `--per-source`; keep `--limit` as alias with deprecation path |
| Pagination offset | `offset` | `start` | 0 | Library + papers |
| Source filter | `sources` | — | all enabled | Canonical wire names below |
| Cache mode | `cache` | — | `auto` | `auto` \| `include` \| `only` \| `off` |
| Detail level | `detail` | — | `compact` | `compact` \| `full` |
| Field projection | `fields` | — | compact default set | Optional explicit list |
| Abstract cap | `abstract_max_chars` | — | 280 when abstract included | 0 = unlimited (full detail only) |

### Source wire names (canonical)

Prefer **skill/CLI forms** as the single wire form for MCP JSON + CLI:

`arxiv`, `paperseed`, `hugging_face`, `semantic_scholar`, `crossref`,
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

- Do not return a bare JSON array for primary agent surfaces.
- `limit == 0` is rejected or clamped with an actionable error (do not mean “all”).
- `diagnostics` may be omitted for pure local library calls that cannot fail
  per-source; prefer always present with empty arrays when cheap.
- Pretty-print JSON is fine for CLI; MCP may use compact JSON to save tokens
  if the host allows (prefer compact for MCP when changing serializers).

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
    "arxiv": "1706.03762",
    "pmid": null,
    "zotero_key": null,
    "paper_id": null
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
3. `pmid:{pmid}`
4. `zotero:{item_key}`
5. `paperseed:{paper_id}`
6. `url:{sha256_12 of canonical url}` last resort

Same logical paper from different sources should prefer the same id when an
identifier is shared (dedupe merge must promote best id set onto the kept hit).

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
  selector?,
  max_chars_per_chunk?
}
```

Resolution order: explicit ids → `hit_id` parse → cache → Zotero → OA PDF
download (**await**, not fire-and-forget) → error with recovery steps.

Defaults:

- `want` default `["metadata"]` or `["structure"]` for paper keys (choose one
  and document; prefer `metadata` then let agent request more).
- Fulltext always respects `max_chars` (default e.g. 8_000) with
  `truncated: true` and `total_chars` when clipped.
- Structure path may accept `selector` (same language as `query_paper`).

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

Map validation failures to MCP `invalid_params`; keep recovery text in the
message or structured data.

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

Server `instructions` string lists the spine and points at prompt
`paperbridge_skill`.

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
