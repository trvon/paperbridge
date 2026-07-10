---
name: paperbridge
description: Use when a task involves Zotero (search, collections, items, PDFs, full-text), DOI/Crossref resolution, searching external paper sources (arXiv, HuggingFace Papers, Semantic Scholar, OpenAlex, etc.), or retrieving locally cached papers from the Paperseed corpus. Provides both a CLI (`paperbridge ...`) and an MCP server (`paperbridge serve`). Prefer MCP tools when available; fall back to CLI invocation otherwise.
---

# paperbridge

Rust CLI + MCP server bridging Zotero (cloud or desktop local API), external
paper indexes, and a local Paperseed cache. Use it for literature search,
reference resolution, structured paper parsing, and preparing paper content
for downstream agents.

> **MCP availability.** When connected to `paperbridge serve`, this guide is
> also served as the prompt `paperbridge_skill` (`prompts/get` with
> `name: "paperbridge_skill"`).

## When to use

- Search a Zotero library or browse collections, tags, attachments.
- Resolve a DOI to structured metadata (title, authors, year, journal, abstract).
- Search external paper indexes: arXiv, Crossref, OpenAlex, Europe PMC, DBLP,
  OpenReview, PubMed, HuggingFace Papers, Semantic Scholar, CORE, NASA ADS, ScholarAPI.
- Retrieve full-text or structured content from a Zotero attachment or a cached paper.
- Validate, create, update, or delete Zotero items and collections.
- Import or query the local Paperseed corpus (`paperseed_enabled = true`).

## Modes

- **MCP (preferred in agent contexts):** use the registered `paperbridge` MCP
  server tools directly — they mirror the CLI commands below.
- **CLI:** `paperbridge <domain> <action>`. All data commands print JSON on
  stdout; errors go to stderr. Pipe through `| jq` for inspection.

## First-time setup

```bash
paperbridge config init --interactive
paperbridge config validate
paperbridge status
```

Backend modes: `cloud` (api.zotero.org, needs `api_key` + `user_id`),
`local` (Zotero Desktop at `http://127.0.0.1:23119`, no key), `hybrid`
(local reads, cloud writes).

## Core recipes

### Search — library, external, and cached

```bash
# Zotero library (paginated envelope)
paperbridge library query --query "diffusion models" --limit 10

# External papers (compact by default; --limit is PAGE size)
paperbridge papers search --query "intrusion detection" --per-source 3 --limit 10
paperbridge papers search --query "attention is all you need" --sources arxiv,openalex
paperbridge papers search --query "attention is all you need" --sources paperseed  # cache only
paperbridge papers search --query "transformers" --limit 5 --offset 10 --detail full

# Open a hit after search (await-friendly path)
paperbridge papers open --hit-id "arxiv:1706.03762" --want metadata,structure
paperbridge papers open --doi "10.1038/nature12373" --want metadata
paperbridge papers open --url "https://example.org/paper.pdf" --want fulltext
```

Results are deduplicated by DOI → arXiv ID → PMID → normalized
title+first-author. Each hit includes `hit_id`, `ids`, `match`, `access`, and
`next` suggested tools. Default `detail=compact` omits full abstracts.
`diagnostics` lists sources that ran, were skipped (missing key), or failed.

MCP tools:
- `search_papers { query|q, limit?, limit_per_source?, sources?, cache?, offset?, detail?, abstract_max_chars? }`
  → `{ query, total_count, offset, limit, has_more, next_offset, detail, hits, diagnostics }`
- `open_paper { hit_id?|doi?|arxiv_id?|item_key?|paper_id?|attachment_key?|url?, want?, max_chars? }`
- `search_items` / `list_collections` → paginated envelopes (`hits`, not bare arrays)

Canonical source wire names: `arxiv`, `paperseed`, `crossref`, `openalex`,
`europe_pmc`, `dblp`, `openreview`, `pubmed`, `hugging_face`,
`semantic_scholar`, `core`, `ads`, `scholarapi` (aliases still accepted).

Always-on (no key): arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed.
Key-gated (appear in `diagnostics.sources_skipped` when unset): HuggingFace,
Semantic Scholar, CORE, NASA ADS, ScholarAPI.

### Resolve a DOI

```bash
paperbridge papers resolve-doi --doi "10.1038/nature12373"
```

When `unpaywall_email` is configured, the response includes `oa_pdf_url`.

### Read content — prefer `open_paper`

```bash
# After search: open by hit_id / DOI / arXiv / Zotero key
paperbridge papers open --hit-id "arxiv:1706.03762" --want fulltext --max-chars 8000
paperbridge papers open --item-key ABCD1234 --want structure
paperbridge papers structure --key ABCD1234

# Vox read-aloud only (not plain fulltext)
paperbridge library read --item-key ABCD1234
paperbridge library read-search -q "sparse attention" --result-index 0 --search-limit 5
```

**Agent spine:** `search_items` / `search_papers` → `open_paper` →
`query_paper` / `resolve_doi`. Use Vox `prepare_*` tools only for read-aloud.

MCP tools:
- `open_paper { hit_id|doi|arxiv_id|item_key|paper_id|attachment_key|url, want, max_chars? }` — preferred
- `get_pdf_text` / `get_item_fulltext` — low-level attachment/cache paths
- `prepare_vox_text` / `prepare_item_for_vox` / `prepare_search_result_for_vox` — Vox chunks only

### Structured paper content

Returns a typed JSON structure with sections, references, and figures.
Works with both Zotero items and cached paper IDs.

```bash
paperbridge papers structure --key ABCD1234
paperbridge papers query --key ABCD1234 --selector "sections[0].text"
paperbridge papers query --key ABCD1234 --selector "metadata.doi"
```

MCP tools: `get_paper_structure { item_key, attachment_key? }`, `query_paper
{ item_key, selector, attachment_key? }`. Both accept Zotero keys or cached
paper IDs. When a cached paper has no extracted fulltext, metadata is still
returned with empty sections (no 404s).

Selectors use dotted paths with bracket indexing (`sections[2].text`,
`references[0].title`). The `source` field tells you the provenance:
`grobid`, `zotero_fulltext`, or `grobid_unavailable`.

### Paper → skill scaffold

Turn a paper's structure into a deterministic SKILL.md scaffold (YAML
frontmatter + markdown body). The mapping is mechanical — abstract →
"When to use", method/design/implementation → "Method", evaluation/results →
"Evaluation", plus limitations and key references. The output is a *scaffold*
for an agent to refine into a real operating procedure, not a finished skill.

```bash
paperbridge papers skill --key ABCD1234 > SKILL.md
paperbridge papers skill --key <cached-paper-id>
```

MCP tool: `prepare_paper_for_skill { item_key, attachment_key? }` → returns
`{ name, description, markdown }`. Accepts Zotero keys or cached paper IDs
(same routing as `get_paper_structure`). Fidelity follows the structure's
`source`: a `zotero_fulltext` scaffold is heuristic and should be verified
against the paper. The CLI prints the raw markdown for piping into a file.

### Local Paperseed corpus

Manage the content-addressed local cache and license-gated seed manifests:

```bash
paperbridge paperseed corpus status
paperbridge paperseed corpus import ./paper.pdf --license cc-by
paperbridge paperseed corpus ingest --metadata item.json --file paper.pdf --license cc-by
paperbridge paperseed corpus query -q "induction heads"
paperbridge paperseed corpus export --format bibtex

paperbridge paperseed seed check --paper-id <id>
paperbridge paperseed seed create --paper-id <id>
```

Imported PDFs have their text automatically extracted and stored in the
corpus for full-text search. YAMS provides an experimental
storage/search backend when `paperseed_yams_enabled = true`.

**OA auto-mirroring & DOI resolution.** With `paperseed_auto_download = true`,
search hits are mirrored into the corpus. Hits that already carry an open-access
PDF url are downloaded directly; hits that only expose a DOI (common for
metadata-only sources like Crossref, PubMed, and DBLP) are resolved to an open
PDF via Unpaywall, falling back to OpenAlex's best OA location. Set
`unpaywall_email` for best Unpaywall coverage. Mirrored files are stored with an
`unknown` license (cached but not seedable).

### Write Zotero items & collections

Write ops take a JSON file on disk. Cloud backend requires `api_key` with
write scope.

```bash
paperbridge item validate --file item.json --online
paperbridge item create --file item.json
paperbridge item update --file item.json
paperbridge item delete --file item.json
paperbridge collection create --name "ML 2025"
```

### Run as MCP server

```bash
paperbridge serve
paperbridge config snippet --target claude
paperbridge config snippet --target opencode
```

## Key config keys

| key | purpose |
|---|---|
| `backend_mode` | `cloud`, `local`, `hybrid` |
| `api_key` | Zotero API key — **redacted in `config get` unless `--show-secret`** |
| `user_id` | numeric Zotero user ID |
| `group_id` | numeric group ID (optional) |
| `library_type` | `user` or `group` |
| `paperseed_enabled` | enable local Paperseed corpus (default `false`) |
| `paperseed_auto_download` | automatically mirror OA PDFs into local corpus (default `true`) |
| `paperseed_yams_enabled` | use YAMS as experimental storage/search backend (default `true`) |
| `paperseed_corpus_root` | override corpus path |
| `hf_token`, `semantic_scholar_api_key`, `core_api_key`, `ads_api_token`, `scholarapi_key` | gate external sources |
| `ncbi_api_key` | optional PubMed rate-limit upgrade |
| `unpaywall_email` | enables OA-PDF enrichment |
| `grobid_url` | GROBID endpoint; if set, auto-spawn is disabled |
| `grobid_auto_spawn` | launch GROBID via Docker (default `false`) |
| `grobid_image` | Docker image for auto-spawn |
| `log_level` | `error`, `warn`, `info`, `debug`, `trace` |

`paperbridge config get` masks secrets by default. Pass `--show-secret` to reveal.

## Gotchas

- **Cloud api_base must be HTTPS** (or `http://localhost` for local mode).
- **`--limit` is page size** (default 10). Fan-out uses `--per-source` / MCP `limit_per_source`.
- **Search is compact by default** — pass `--detail full` / `detail: "full"` for abstracts. Full abstracts default to 280 characters; set `abstract_max_chars=0` for unlimited text.
- **Search results are paginated** — use `offset`/`limit`; check `has_more` / `next_offset`. Later pages expand the per-source fetch window automatically, up to a safe window of 200.
- **Key-gated sources skip loudly** — see `diagnostics.sources_skipped` / `sources_failed`.
- **Cached papers are conservative by default**: default cache-only hits need strong relevance, and `--sources` without `paperseed` excludes cache hits. Use `--sources paperseed` for explicit cache-only search.
- **PDF text extraction** happens automatically during local corpus import — no separate step needed.
- **Fulltext can be large** — prefer `open_paper` with `max_chars` (default 8000) or structure selectors. Vox tools use `--max-chars-per-chunk`.
- **Write operations need `version` on update/delete** (Zotero optimistic concurrency). Re-fetch if you get HTTP 412.
- **`config get api_key` no longer prints the raw key** — it prints `(set, N chars — pass --show-secret to reveal)`.
- **Legacy flat commands** (`query`, `create-item`, `backend-info`, `search-papers`, …) still work but emit a deprecation warning. Prefer the canonical domain paths.

## Verify install

```bash
paperbridge --version
paperbridge status
paperbridge config validate
```

## Contributors

CLI surface changes must be reviewed against
[`docs/design/cli-design.md`](design/cli-design.md).
MCP/search/return-shape and agent discover→read changes must also follow
[`docs/design/llm-interface.md`](design/llm-interface.md) and the backlog in
[`docs/design/llm-interface-tasks.md`](design/llm-interface-tasks.md).
