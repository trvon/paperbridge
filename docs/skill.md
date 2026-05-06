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
# Zotero library
paperbridge library query -q "diffusion models" --limit 10

# External papers + local cache (cached results prioritized first)
paperbridge papers search -q "intrusion detection" --limit 3 --max-results 10
paperbridge papers search -q "attention is all you need" --sources arxiv,semantic_scholar

# Paginated (agents should page through large result sets)
paperbridge papers search -q "transformers" --max-results 5 --offset 10
```

Results are deduplicated by DOI → arXiv ID → PMID → normalized
title+first-author. Cached papers appear with `source: "paperseed"` and a
`cache.cached` annotation. All cached hits are sorted ahead of external
results.

MCP tool: `search_papers { query, limit_per_source?, sources?, offset?, limit? }`.
Returns `{ query, total_count, offset, limit, hits: [...] }`. Use `offset` and
`limit` to page through large result sets.

Available source values: `arxiv`, `paperseed` (local cache), `crossref`,
`openalex` (`oa`), `europe_pmc` (`epmc`), `dblp`, `openreview` (`or`),
`pubmed` (`pm`), `hugging_face` (`hf`), `semantic_scholar` (`s2`), `core`,
`ads` (`nasa_ads`), `scholarapi` (`scholar`).

Always-on (no key): arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed.
Key-gated (silent skip when unset): HuggingFace, Semantic Scholar, CORE, NASA ADS, ScholarAPI.

### Resolve a DOI

```bash
paperbridge papers resolve-doi --doi "10.1038/nature12373"
```

When `unpaywall_email` is configured, the response includes `oa_pdf_url`.

### Read full-text — Zotero or cached paper

```bash
# Zotero attachment
paperbridge library read --item-key ABCD1234
paperbridge library read --item-key ABCD1234 --attachment-key PDF5678

# Search then read (picks best attachment)
paperbridge library read-search -q "sparse attention" --result-index 0 --search-limit 5
```

**Cache fallback:** `get_pdf_text` and `get_item_fulltext` automatically
search the local Paperseed cache when Zotero is unreachable. Pass a title, DOI,
or paper ID as the key — the route treats it as a natural-language query
against cached papers. If a match is found with extracted fulltext, it is
returned directly.

MCP tools:
- `get_pdf_text { attachment_key }` — Zotero attachment or cache query
- `get_item_fulltext { attachment_key }` — same fallback behavior
- `prepare_vox_text { text?, attachment_key?, max_chars_per_chunk? }` — chunks for Vox
- `prepare_item_for_vox { item_key, attachment_key?, max_chars_per_chunk? }` — prefers cached papers
- `prepare_search_result_for_vox { q, result_index?, ... }` — search → cached-paper check → Zotero fallback

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
- **Search results are paginated** — use `offset`/`limit` to page through large sets. The `total_count` field tells you how many remain.
- **Cached papers are prioritized first** in search results (regardless of `--sources` filter). Look for `cache.cached: true` and `source: "paperseed"`.
- **PDF text extraction** happens automatically during local corpus import — no separate step needed.
- **Read output can be large** — always set `--max-chars-per-chunk` when feeding into an LLM.
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
