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

For Zotero Desktop local API mode:

```bash
paperbridge config set backend_mode local
```

## Paper search & discovery

Search across arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed,
HuggingFace Papers, Semantic Scholar, CORE, NASA ADS, and ScholarAPI in parallel.
Local cached results from Paperseed are used conservatively: cached duplicates are annotated/preferred, and cache-only hits surface only for strong matches unless you explicitly include `paperseed`.

```bash
paperbridge papers search -q "intrusion detection" --limit 3 --max-results 10
paperbridge papers search -q "attention is all you need" --sources arxiv,semantic_scholar
paperbridge papers search -q "attention is all you need" --sources paperseed  # cache only
paperbridge papers resolve-doi --doi 10.1038/nature12373
```

Results are paginated (`--offset`, `--max-results`) and deduplicated by DOI,
arXiv ID, PMID, and normalized title+author. Unconfigured key-gated sources are
silently skipped.

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

- `papers {structure,query}` returns structured JSON (metadata, sections,
  references) suitable for section-aware agents.
- `library read...` returns Vox-ready text chunks from Zotero or a cached paper.

Structured parsing uses Zotero's indexed full-text by default and can optionally
use [GROBID](https://github.com/kermitt2/grobid) for richer section and
reference extraction. See [docs/structured-paper.md](docs/structured-paper.md).

## Smart cache behavior

When papers are cached locally, existing routes become smarter without new
commands:

- `get_pdf_text` / `get_item_fulltext` fall back to searching the local cache
  by the key as a natural-language query when Zotero is unreachable.
- `prepare_item_for_vox` / `prepare_search_result_for_vox` prefer cached papers.
- `get_paper_structure` / `query_paper` build a fallback structure from cached
  full-text when called with a cached paper id.

## Local corpus & caching

Paperseed manages a content-addressed local corpus for lawful paper storage,
full-text querying, and license-gated seed manifests:

```bash
paperbridge paperseed corpus status
paperbridge paperseed corpus import ./paper.pdf --license cc-by
paperbridge paperseed corpus ingest --metadata item.json --file paper.pdf --license cc-by
paperbridge paperseed corpus query -q "induction heads"
paperbridge paperseed corpus export --format bibtex

paperbridge paperseed seed check --paper-id <id>
paperbridge paperseed seed create --paper-id <id>
```

The corpus is stored under `$XDG_DATA_HOME/paperbridge/paperseed` (defaults to
`~/.local/share/paperbridge/paperseed`).

Seeding is license-gated: private or unknown-license material may be stored and
searched locally, but seed manifests are created only when redistribution is
allowed.

### YAMS experimental backend

When `paperseed_yams_enabled = true` and the `yams` binary is available, Paperseed
uses YAMS for storage, full-text indexing, and semantic search. If YAMS is
unavailable, the system falls back to the local JSON corpus automatically.

```toml
paperseed_enabled = false
paperseed_auto_download = true
paperseed_yams_enabled = true
# paperseed_corpus_root = "/path/to/corpus"
```

## Config doctor

```bash
paperbridge config doctor              # check config health
paperbridge config doctor --setup      # interactively fill missing values
paperbridge config doctor --verbose    # detailed diagnostics
paperbridge config doctor --json       # machine-readable output
```

## MCP server

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
