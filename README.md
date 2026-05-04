# paperbridge

`paperbridge` is a Rust MCP + CLI bridge for Zotero library access, external
paper discovery, DOI/Crossref resolution, structured paper parsing,
read-aloud/Vox preparation, and local open-access paper caching.

Paperbridge owns the user-facing CLI, MCP/API surface, and configuration.
[Paperseed](crates/paperseed/README.md) is vendored under `crates/paperseed` as
the local corpus + seed-manifest engine. Experimental YAMS-backed caching uses
[YAMS](https://github.com/trvon/yams) when available.

## For agents

When `paperbridge serve` is registered as an MCP server, fetch the operating guide
via `prompts/get` with `name: "paperbridge_skill"` — it enumerates every tool,
calling conventions, and recipes. Start there before composing tool calls.

## Install

```bash
# npm
npm install -g paperbridge

# Homebrew (macOS / Linux)
brew tap trvon/paperbridge && brew install paperbridge

# From source
./setup.sh
```

Pre-built binaries (macOS arm64/x86_64, Linux x86_64, Windows x86_64) are
published to [GitHub Releases](https://github.com/trvon/paperbridge/releases).

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

## Config doctor

Use doctor after upgrades or when behavior looks off:

```bash
paperbridge config doctor
paperbridge config doctor --setup
paperbridge config doctor --verbose
paperbridge config doctor --json
```

`doctor` is concise by default. `--setup` interactively fills important missing
settings, including Paperseed corpus/caching values and the experimental YAMS
toggle. Enable YAMS to use it when its daemon is running while keeping the local corpus as a
fallback; disable it for local-dir-only corpus behavior. `--verbose` and `--json`
are for advanced troubleshooting and automation.

## External paper discovery

```bash
paperbridge papers search -q "retrieval augmented generation" --limit 5
paperbridge papers resolve-doi --doi 10.1038/nature12373
```

Paperbridge can search external paper indexes and return open-access PDF URLs
when sources provide them. If Paperseed is enabled and
`paperseed_auto_download` is left on, open-access results with PDF URLs can be
mirrored into the local corpus automatically.

If a paper is already cached locally, the existing paper routes become smarter
without adding new commands:

- `papers search` includes local cached papers and prioritizes cached matches.
- `get_pdf_text` / `prepare_vox_text` can read cached papers directly.
- `get_paper_structure` / `query_paper` can build a fallback structure from
  cached full text.
- `prepare_item_for_vox` and `prepare_search_result_for_vox` automatically use
  cached papers when they are the best available source.

See [docs/papers.md](docs/papers.md) for source coverage and API key behavior.

## Structured paper workflows

```bash
paperbridge papers structure --key ABCD1234
paperbridge papers query --key ABCD1234 --selector "sections[0].text"
paperbridge library read --item-key ABCD1234
paperbridge library read-search -q "transformers" --result-index 0
```

Paperbridge supports two levels of paper retrieval:

- `paperbridge papers {structure,query}` returns structured JSON for section-aware agents and
  scripts.
- `paperbridge library read...` prepares Vox-ready text chunks from Zotero or a
  cached paper.

Structured parsing uses configured full text by default and can optionally use
[GROBID](https://github.com/kermitt2/grobid) for richer section/reference
extraction.

See [docs/structured-paper.md](docs/structured-paper.md) for the full structure,
fallback model, and GROBID setup.

## Experimental local corpus + cache

Paperbridge can optionally keep a local paper cache through vendored
[Paperseed](crates/paperseed/README.md). This is useful when you want external
paper discovery to become locally readable automatically over time.

When enabled:

- external search results can be mirrored into the local corpus in the
  background,
- cached papers are surfaced through existing routes instead of separate cache
  commands,
- local corpus search can contribute hits to `papers search`, and
- licensed files can still be managed explicitly through `paperbridge paperseed`.

Paperseed can also use
[YAMS](https://github.com/trvon/yams) ([docs](https://yamsmemory.ai))
experimentally as a storage/search backend when the `yams` binary is available
and its daemon is running. In that mode, Paperbridge/Paperseed prefer the local
cache automatically and fall back to the JSON corpus when YAMS is unavailable.

## Paperseed corpus and seeding

Paperseed commands are exposed through Paperbridge so they inherit Paperbridge
config:

```bash
paperbridge paperseed corpus status
paperbridge paperseed corpus import ./paper.pdf --license user-owned-private
paperbridge paperseed corpus ingest --metadata item.json --file paper.pdf --license cc-by
paperbridge paperseed corpus query -q "induction heads"
paperbridge paperseed corpus export --format bibtex

paperbridge paperseed seed check --paper-id <id>
paperbridge paperseed seed create --paper-id <id>
paperbridge paperseed p2p status
```

By default, the corpus is stored under the XDG data directory
(`$XDG_DATA_HOME/paperbridge/paperseed` or
`~/.local/share/paperbridge/paperseed`). When
`paperseed_yams_enabled = true`, Paperseed experimentally mirrors imports into
[YAMS](https://github.com/trvon/yams) if the `yams` binary is detected and the
YAMS daemon is running, then tries YAMS retrieval first with the JSON corpus as
a fallback. Set `paperseed_yams_enabled = false` to use only the local corpus
directory.

Seeding is license-gated. User-private or unknown-license material may be stored
and searched locally, but seed manifests are created only when redistribution is
allowed.

Relevant config keys:

```toml
paperseed_enabled = false
paperseed_auto_download = true
paperseed_yams_enabled = true # experimental; falls back to local corpus if yams/daemon is unavailable
# paperseed_corpus_root = "/path/to/corpus"
```

## MCP server

Run as an MCP server for Claude, OpenCode, or any MCP-compatible host:

```bash
paperbridge serve
```

Generate client config snippets:

```bash
paperbridge config snippet --target claude
paperbridge config snippet --target opencode
```

## Documentation

- [Full usage and command reference](USAGE.md)
- [External paper search, DOI / Crossref, API keys](docs/papers.md)
- [Structured paper parsing (GROBID + fallbacks)](docs/structured-paper.md)
- [Paperseed local corpus and seeding](crates/paperseed/README.md)
- [Shell completions](docs/completions.md)
- [Agent operating guide (`paperbridge_skill`)](docs/skill.md)
- [Design notes](docs/design/README.md)
- [Contributing and local quality checks](CONTRIBUTING.md)
