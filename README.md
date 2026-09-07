# paperbridge

Rust MCP + CLI bridge for Zotero library access, external paper discovery,
DOI/Crossref resolution, structured paper parsing, read-aloud (Vox) preparation,
and local open-access paper caching.

[Paperseed](crates/paperseed/README.md) is vendored under `crates/paperseed` for
local corpus storage and license-gated seed manifests. When available,
[YAMS](https://github.com/trvon/yams) ([docs](https://yamsmemory.ai)) provides
an experimental storage/search backend with full-text indexing.

## Install

```bash
# npm
npm install -g paperbridge

# Homebrew
brew tap trvon/paperbridge && brew install paperbridge

# From source
./setup.sh
```

Pre-built binaries are published to
[GitHub Releases](https://github.com/trvon/paperbridge/releases).

## Get started

```bash
paperbridge config init --interactive
paperbridge config doctor --setup
paperbridge config validate
paperbridge status
paperbridge library query -q "machine learning" --limit 3
paperbridge library read-search -q "machine learning" --result-index 0
paperbridge papers search -q "retrieval augmented generation" --limit 5
paperbridge papers query --key ABCD1234 --selector "metadata.doi"
```

Structured CLI results are human-readable by default. Add the global `--json`
flag for scripts and agents, either before or after the command:

```bash
paperbridge --json papers search -q "retrieval augmented generation" --limit 5
paperbridge library query -q "machine learning" --limit 3 --json
```

With `--json`, runtime failures return `{ "error", "reason", "try" }` JSON on
stderr and a non-zero exit code. Stdout remains empty on failure, so successful
payload pipelines are not contaminated by error output.

MCP tool results remain JSON. Content-native commands such as shell
completions, `papers skill`, BibTeX export, and client configuration snippets
keep their native output formats. `paperseed corpus export` defaults to BibTeX;
pass `--json` to export the corpus as JSON.

For Zotero Desktop local API mode:

```bash
paperbridge config set backend_mode local
```

## Paper search & discovery

Search the local YAMS research workspace plus arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed,
HuggingFace Papers, Semantic Scholar, CORE, NASA ADS, and ScholarAPI in parallel.
Local cached results from Paperseed are used conservatively: cached duplicates are annotated/preferred, and cache-only hits surface only for strong matches unless you explicitly include `paperseed`.

```bash
paperbridge papers search --query "intrusion detection" --per-source 3 --limit 10
paperbridge papers search -q "attention is all you need" --sources arxiv,semantic_scholar
paperbridge papers search -q "attention is all you need" --sources paperseed  # cache only
paperbridge papers search -q "What drives detection in GNN" --sources research
paperbridge papers resolve-doi --doi 10.1038/nature12373
paperbridge papers access --doi 10.1038/nature12373
```

Institutional library access is optional and disabled by default. Interactive
setup separates an OpenURL holdings resolver from an EZproxy/OpenAthens sign-in
gateway and enables fallback routing without storing institutional credentials:

```bash
paperbridge config doctor --setup
paperbridge config set institution_access_mode off       # disable at any time
paperbridge papers access --url https://example.org/article
```

`papers access` checks configured holdings, ranks full-text provider options,
and opens the best route in the default browser. Use the global `--json` flag
for structured resolver status and access options; `--no-open` suppresses the
browser side effect. In `fallback` mode, open-access DOI results keep their
direct OA URL; `prefer` routes them through the configured institutional profile
first.

Results are paginated (`--offset`, `--limit`) and deduplicated by DOI, arXiv
ID, PMID, and corroborated title+author, preserving complementary identifiers.
Pages reuse a fixed per-source prefix (`--per-source`, default 10, max 200);
restart at offset 0 with a larger prefix to broaden discovery. Paper-search
`total_count` counts this candidate window, not the entire provider index.
Library totals may be `null` with `count_kind: "unknown"`; use `has_more`.
Unconfigured key-gated sources appear in
`diagnostics.sources_skipped`.

Springer journal articles can appear through Crossref, OpenAlex, and other
indexes, but Paperbridge does not currently expose a dedicated `springer`
source. Publisher-hosted full text still requires an open PDF URL, a Zotero
attachment, an institutional access route, or a cached Paperseed copy.

If `paperseed_enabled` and `paperseed_auto_download` are on, open-access PDFs
are mirrored into the local corpus in background threads so they become
available to all existing paper routes over time.

See [docs/papers.md](docs/papers.md) for API key setup and source details.

## Structured paper workflows

```bash
paperbridge papers structure --key ABCD1234
paperbridge papers query --key ABCD1234 --selector "sections[0].text"
paperbridge library read --item-key ABCD1234
paperbridge library read-search -q "transformers" --result-index 0
```

- `papers {structure,query}` returns structured metadata, sections, and
  references suitable for section-aware agents; add `--json` for JSON output.
- `library read...` returns Vox-ready text chunks from Zotero or a cached paper.

Structured parsing uses Zotero's indexed full-text by default and can optionally
use [GROBID](https://github.com/kermitt2/grobid) for richer section and
reference extraction. See [docs/structured-paper.md](docs/structured-paper.md).

## Smart cache behavior

When papers are cached locally, existing routes become smarter without new
commands:

- `get_pdf_text` / `get_item_fulltext` accept exact cached paper IDs; failed
  Zotero identifiers never fall back to a relevance search for another paper.
- `prepare_item_for_vox` / `prepare_search_result_for_vox` prefer cached papers.
- `get_paper_structure` / `query_paper` build a fallback structure from cached
  full-text when called with a cached paper id.

## Local corpus & caching

Paperseed manages a content-addressed local corpus for lawful paper storage,
full-text querying, and license-gated seed manifests:

```bash
paperbridge paperseed corpus status
paperbridge paperseed corpus list
paperbridge paperseed corpus show <id-or-unique-hash-prefix>
paperbridge paperseed corpus import ./paper.pdf --license cc-by
paperbridge paperseed corpus import ./large.pdf --license cc-by --no-fulltext
paperbridge paperseed corpus ingest --metadata item.json --file paper.pdf --license cc-by
paperbridge paperseed corpus query -q "induction heads"
paperbridge paperseed corpus export --format bibtex
paperbridge paperseed corpus remove <id-or-unique-hash-prefix>
paperbridge paperseed corpus reindex

paperbridge paperseed seed check --paper-id <id>
paperbridge paperseed seed create --paper-id <id>
```

The corpus is stored under `$XDG_DATA_HOME/paperbridge/paperseed` (defaults to
`~/.local/share/paperbridge/paperseed`).

`corpus status` reports both paper and index document counts and warns on drift.
Full text is stored in content-addressed `text/` blobs rather than inline in
`corpus.json`; `--no-fulltext` defers extraction until first read. PDF extraction
does not perform OCR.

Seeding is license-gated: private or unknown-license material may be stored and
searched locally, but seed manifests are created only when redistribution is
allowed.

### YAMS experimental backend

When `paperseed_yams_enabled = true` and the `yams` binary is available,
Paperseed uses verified, synchronous YAMS indexing for imports and OA mirrors
and stores the resulting `yams_hash`. `papers search --sources research` also
searches off-disk YAMS paper projects, collapses project fragments, and reports
`access.content_state` as `ready` or `stale`. If YAMS is unavailable, the system
falls back to the local JSON corpus automatically.

```toml
paperseed_enabled = false
paperseed_auto_download = true
paperseed_yams_enabled = true
# paperseed_corpus_root = "/path/to/corpus"

institution_access_mode = "off" # off, fallback, prefer
# institution_resolver_url = "https://resolver.example.edu/openurl"
# institution_gateway_url = "https://proxy.example.edu/login?url="
```

## Config doctor

```bash
paperbridge config doctor              # check config health
paperbridge config doctor --setup      # interactively fill missing values
paperbridge config doctor --verbose    # detailed diagnostics
paperbridge config doctor --json       # machine-readable output
```

## MCP server

Use `paperbridge serve --profile core` for the six-tool discovery/read spine;
plain `serve` retains the full tool surface for compatibility. Results include
output schemas and `structuredContent`, plus compact JSON text for older hosts.
The complete tool-result wire payload is capped at 64 KiB; oversized responses
return a bounded error with recovery guidance, never a silently clipped document.

Prefer `papers open --want structure` for a bounded outline, then select a field
with `--selector`. String selections and fulltext use UTF-8 byte cursors;
chunks report continuation through `chunks_page`. Inspect `metadata_status`,
`metadata_page`, `structure_page`, and provenance before treating results as
complete evidence. See [the migration notes](docs/design/model-output-remediation.md).


```bash
paperbridge serve
```

Generate client config snippets:

```bash
paperbridge config snippet --target claude
paperbridge config snippet --target opencode
```

When connected, agents should fetch the `paperbridge_skill` prompt for the full
operating guide.

## Documentation

- [Full usage and command reference](USAGE.md)
- [External paper search, DOI / Crossref, API keys](docs/papers.md)
- [Structured paper parsing (GROBID + fallbacks)](docs/structured-paper.md)
- [Paperseed local corpus and seeding](crates/paperseed/README.md)
- [Shell completions](docs/completions.md)
- [Agent operating guide (`paperbridge_skill`)](docs/skill.md)
- [Design notes](docs/design/README.md)
- [Contributing and local quality checks](CONTRIBUTING.md)
