---
name: paperbridge
description: Use when a task involves Zotero (search, collections, items, PDFs, full-text), DOI/Crossref resolution, or searching external paper sources (arXiv, HuggingFace Papers, Semantic Scholar, OpenAlex, Europe PMC, DBLP, OpenReview, CORE, NASA ADS, PubMed, ScholarAPI). Provides both a CLI (`paperbridge ...`) and an MCP server (`paperbridge serve`) that expose the same capabilities. Prefer the MCP tools when available in the host; fall back to CLI invocation otherwise.
---

# paperbridge

`paperbridge` is a Rust CLI + MCP server that bridges Zotero (cloud or desktop local API) and external paper indexes. Use it for literature search, reference resolution, and preparing paper content for downstream agents.

> **Availability over MCP.** When a host is connected to `paperbridge serve`, this guide is also served as the prompt `paperbridge_skill` (call `prompts/get` with `name: "paperbridge_skill"`). That means connected agents get the operating guide automatically — no local checkout required.

## When to use

- User references Zotero library, collections, tags, attachments, or wants to pull full-text from their library.
- User provides a DOI and needs structured metadata (title, authors, year, journal, abstract).
- User asks to find papers on a topic across arXiv / HuggingFace / Semantic Scholar / Crossref / OpenAlex / Europe PMC / DBLP / OpenReview / CORE / NASA ADS / PubMed / ScholarAPI.
- User wants to validate or create/update Zotero items from JSON payloads.

## Modes

- **MCP (preferred in agent contexts):** if a `paperbridge` MCP server is registered in the host, use its tools directly — they mirror the CLI commands below.
- **CLI:** `paperbridge <domain> <action>`. The command graph is layered by domain (`library`, `item`, `collection`, `papers`, `config`, `status`). All data commands print JSON on stdout; errors go to stderr. Pipe through `| jq` for inspection.

## First-time setup

```bash
paperbridge config init --interactive   # prompts for backend mode, api_key, user_id, etc.
paperbridge config validate
paperbridge status                       # confirm active backend & capabilities
```

Backend modes: `cloud` (api.zotero.org, requires `api_key` + `user_id`), `local` (Zotero Desktop local API on `http://127.0.0.1:23119`, no key needed), `hybrid` (local reads, cloud writes).

To switch: `paperbridge config set backend_mode local`.

## Core recipes

### Search Zotero library
```bash
paperbridge library query -q "diffusion models" --limit 10
paperbridge library query -q "Karpathy" --qmode titleCreatorYear --item-type journalArticle
paperbridge library collections --top-only --limit 20
```

### Resolve a DOI to structured metadata
```bash
paperbridge papers resolve-doi --doi "10.1038/nature12373"
```

When `unpaywall_email` is configured, the response is enriched with an `oa_pdf_url` (best open-access PDF) from Unpaywall — otherwise that field is omitted. Unpaywall requires only an email address for attribution, not an API key.

### Read full-text of one item (for summarization, quoting, downstream prompts)
```bash
paperbridge library read --item-key ABCD1234
paperbridge library read --item-key ABCD1234 --attachment-key PDF5678
# Chunked output for context-window-aware consumers:
paperbridge library read --item-key ABCD1234 --max-chars-per-chunk 8000
# Or combined: search, pick a result index, then prepare it:
paperbridge library read-search -q "sparse attention" --result-index 0 --search-limit 5
```

### Query structured paper content (agent-friendly)
Return a paper as a typed JSON structure, then select into it with a dotted path:

```bash
paperbridge paper structure --key ABCD1234
paperbridge paper query --key ABCD1234 --selector "sections[0].heading"
paperbridge paper query --key ABCD1234 --selector "metadata.doi"
```

- MCP tools: `get_paper_structure { item_key, attachment_key? }` and `query_paper { item_key, selector, attachment_key? }`.
- Selectors use dotted paths with bracket indexing (e.g. `sections[2].text`, `references[0].title`).
- The response includes `source`, one of:
  - `{ "kind": "grobid" }` — parsed via GROBID; real section / heading / reference breakdown.
  - `{ "kind": "zotero_fulltext" }` — Zotero's stored full text only; one `sections[0]` body blob. Correct for most agent queries when GROBID isn't configured.
  - `{ "kind": "grobid_unavailable", "reason": "..." }` — GROBID was configured but the call failed; the service fell back to Zotero full text. Check the reason.
- **Precedence:** if `grobid_url` is set, it wins — Docker auto-spawn is never attempted. To use auto-spawn, leave `grobid_url` unset and set `grobid_auto_spawn=true`. See [docs/structured-paper.md](structured-paper.md) for the full flow, timing, and troubleshooting.

### Search external paper sources
Sources run in parallel; failures/timeouts per source are non-fatal and log only at `debug` level. Results dedupe by DOI → arXiv ID → PMID → normalized title+first-author. `--sources` is parse-validated (invalid values fail before any network call).

```bash
paperbridge papers search -q "vision transformers" --limit 5
paperbridge papers search "vision transformers" --limit 5
paperbridge papers search -q "attention is all you need" --sources arxiv,openalex,semantic_scholar
paperbridge papers search -q "CRISPR Cas9" --sources europe_pmc,pubmed
paperbridge papers search -q "graph neural networks" --sources dblp,openreview
```

`-q`, `--q`, and the positional query form are equivalent; prefer `-q` for Unix-style shorthand. `--q` remains supported for compatibility, and positional queries are convenient for quick shell/agent calls.

Available source values for `--sources`: `arxiv`, `crossref`, `openalex` (alias `oa`), `europe_pmc` (alias `epmc`), `dblp`, `openreview` (alias `or`), `pubmed` (alias `pm`), `hugging_face` (alias `hf`), `semantic_scholar` (alias `s2`), `core`, `ads` (alias `nasa_ads`), `scholarapi` (aliases `scholar`, `scholar_api`, `scolarapi`).

**Always on (no key needed):** arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed. PubMed and OpenAlex will upgrade rate limits / polite-pool priority if `ncbi_api_key` / `unpaywall_email` is set.

**Key-gated (silently skipped when unconfigured):** HuggingFace, Semantic Scholar, CORE, NASA ADS, ScholarAPI.

```bash
paperbridge config set hf_token <token>
paperbridge config set semantic_scholar_api_key <key>
paperbridge config set core_api_key <key>
paperbridge config set ads_api_token <token>
paperbridge config set scholarapi_key <key>
paperbridge config set ncbi_api_key <key>         # optional: upgrades PubMed 3→10 req/s
paperbridge config set unpaywall_email <email>    # enables OA-PDF enrichment on resolve-doi
# or env: HF_TOKEN, SEMANTIC_SCHOLAR_API_KEY, CORE_API_KEY, ADS_API_TOKEN, SCHOLARAPI_KEY, NCBI_API_KEY, UNPAYWALL_EMAIL
```

### Create / update / delete Zotero items & collections
All write ops take a JSON file on disk (schema is validated before send). Cloud backend requires `api_key` with write scope.

```bash
paperbridge item validate --file item.json --online    # pre-check, with Crossref cross-ref
paperbridge item create --file item.json
paperbridge item update --file item.json               # file must include "key" + "version"
paperbridge item delete --file item.json
paperbridge collection create --name "ML 2025" --parent-collection ABCD1234
paperbridge collection update --file collection.json
paperbridge collection delete --file collection.json
```

### Run as MCP server
```bash
paperbridge serve                               # stdio transport
paperbridge config snippet --target claude      # print ready-to-paste MCP config
paperbridge config snippet --target opencode
```

## Key config keys

| key | purpose |
|---|---|
| `backend_mode` | `cloud`, `local`, `hybrid` |
| `api_key` | Zotero API key (cloud/hybrid writes) — **redacted in `config get` unless `--show-secret`** |
| `user_id` | numeric Zotero user ID (cloud). Resolve with `paperbridge config resolve-user-id --login <username>` |
| `group_id` | numeric group ID (optional, for group libraries) |
| `library_type` | `user` or `group` |
| `cloud_api_base` / `local_api_base` | override default endpoints |
| `hf_token`, `semantic_scholar_api_key`, `core_api_key`, `ads_api_token`, `scholarapi_key` | gate external sources (silent skip when unset) |
| `ncbi_api_key` | optional PubMed rate-limit upgrade (3→10 req/s); PubMed still runs without it |
| `unpaywall_email` | enables OA-PDF enrichment on `papers resolve-doi`; also sent as OpenAlex `mailto` polite-pool hint |
| `grobid_url` | remote or local GROBID endpoint (e.g. `http://localhost:8070`); if set, auto-spawn is disabled |
| `grobid_auto_spawn` | when `grobid_url` is unset, launch GROBID via `docker run` on first request (default `false`) |
| `grobid_image` | Docker image used by auto-spawn (default `lfoppiano/grobid:0.8.1`) |
| `grobid_timeout_secs` | HTTP timeout for GROBID requests (default 120) |
| `log_level` | `error`, `warn`, `info`, `debug`, `trace` |

`paperbridge config get` masks secrets by default. Use `--show-secret` when you genuinely need the value.

## Gotchas

- **Cloud api_base must be HTTPS** (or `http://localhost` for local mode). The CLI refuses to transmit `api_key` over cleartext.
- **DOI lookups hit Crossref** — be mindful of rate limits on bursty batch work; prefer caching results.
- **Read output can be large**: always set `--max-chars-per-chunk` when feeding into an LLM with a finite context window.
- **Write operations require `version` on update/delete** to avoid clobbering concurrent edits (Zotero optimistic concurrency). Re-fetch the item first if you get HTTP 412.
- **`config get api_key` no longer prints the raw key** — it prints `(set, N chars — pass --show-secret to reveal)`.
- **Legacy flat commands still work** (`query`, `create-item`, `backend-info`, `search-papers`, …) but emit a deprecation warning on stderr and will be removed in 0.4.0. Prefer the canonical domain paths above.

## Verify install / health

```bash
paperbridge --version
paperbridge status
paperbridge config validate
```

If invoking via npm (`npm install -g paperbridge`) fails silently, the per-platform binary may have been shipped without +x; re-run with `--include=optional` or chmod the installed binary.

## Contributors

CLI surface changes must be reviewed against [`docs/design/cli-design.md`](design/cli-design.md). That document defines paperbridge's 11-principle CLI design methodology and contains the required-review checklist for any PR touching `src/cli.rs`, user-visible error copy, or this skill.
