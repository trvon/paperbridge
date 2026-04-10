# paperbridge

MCP and CLI bridge for Zotero search, collections, PDF/full-text retrieval, and DOI/Crossref resolution.

## Install

```bash
npm install -g paperbridge
```

## Usage

```bash
paperbridge config init --interactive
paperbridge config validate
paperbridge query --q "machine learning" --limit 3
```

## MCP Server

Run as an MCP server for Claude, OpenCode, or any MCP-compatible host:

```bash
paperbridge serve
```

## DOI / Crossref

Resolve a DOI to structured citation metadata:

```bash
paperbridge resolve-doi --doi "10.1038/nature12373"
```

## Other install methods

- **Homebrew**: `brew tap trvon/paperbridge && brew install paperbridge`
- **GitHub Releases**: [Download binaries](https://github.com/trvon/paperbridge/releases)
- **From source**: `./setup.sh`

## Documentation

See the full [README on GitHub](https://github.com/trvon/paperbridge).
