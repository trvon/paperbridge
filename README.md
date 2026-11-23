# paperbridge

Literature search and retrieval for LLM agents. paperbridge searches Zotero,
13 external paper and book indexes and local caches, resolves DOIs, follows
citations, and returns clean text and structure through a CLI and an MCP
server.

## Why paperbridge

An agent doing a literature review needs more than one search box. It needs
the reference list and the citing papers, the text of what it cites, and to
know when a paper has been retracted. paperbridge fetches and caches that
data. The agent decides relevance and stance.

## What it provides

- Zotero access over the Web API or the Zotero Desktop local API
- Parallel search over arXiv, Crossref, OpenAlex, Semantic Scholar, Europe PMC,
  PubMed, DBLP, OpenReview, CORE, NASA ADS, HuggingFace Papers, ScholarAPI and
  Open Library, plus the local Paperseed corpus and a YAMS research workspace
- DOI resolution with retraction and correction notices (Crossref, Retraction
  Watch, OpenAlex)
- References, cited-by and related works, cached in a local citation store
- Structured paper reads: sections, references, selectors, Vox read-aloud chunks
- PDF fetch through your own signed-in browser, and a license-gated local corpus
- A CLI with human or `--json` output, and an MCP server over stdio

## Install

```bash
# npm
npm install -g paperbridge

# Homebrew
brew tap trvon/paperbridge && brew install paperbridge

# From source
./setup.sh
```

Release binaries are on [GitHub Releases](https://github.com/trvon/paperbridge/releases).
Config lives in `$XDG_CONFIG_HOME/paperbridge/config.toml`; caches and the
corpus in `$XDG_DATA_HOME/paperbridge`.

## Start

```bash
paperbridge config init --interactive
paperbridge config doctor --setup

paperbridge papers search -q "retrieval augmented generation" --limit 5
paperbridge papers resolve-doi --doi 10.1038/nature14539
paperbridge papers cited-by 10.1038/nature14539 --limit 10
paperbridge papers open --doi 10.1038/nature14539 --want structure
paperbridge library query -q "machine learning" --limit 3
```

Add `--json` before the command for scripts and agents. Run
`paperbridge --help` or `paperbridge <command> --help` for the full reference.

## MCP

```bash
paperbridge serve                 # full tool surface
paperbridge serve --profile core  # six-tool discovery/read core
```

```json
{
  "mcpServers": {
    "paperbridge": {
      "command": "paperbridge",
      "args": ["serve"]
    }
  }
}
```

`paperbridge config snippet --target claude` prints this for your client.
Connected agents should load the `paperbridge_skill` prompt first.

## Documentation

- [Usage and command reference](USAGE.md)
- [Paper sources, API keys, institutional access, browser fetch](docs/papers.md)
- [Structured paper parsing](docs/structured-paper.md)
- [Paperseed corpus and YAMS backend](crates/paperseed/README.md)
- [Agent operating guide (`paperbridge_skill`)](docs/skill.md)
- [Shell completions](docs/completions.md)
- [Design notes](docs/design/README.md)
- [Contributing](CONTRIBUTING.md)

## Project

- GitHub: <https://github.com/trvon/paperbridge>
- License: MIT
