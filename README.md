# paperbridge

`paperbridge` is a Rust MCP + CLI bridge for Zotero search, collections, PDF/full-text retrieval, and DOI/Crossref resolution.

## Get Started

```bash
./setup.sh
paperbridge config init --interactive
paperbridge config validate
paperbridge query --q "machine learning" --limit 3
```

For Zotero Desktop local API mode, use:

```bash
paperbridge config set backend_mode local
```

## DOI / Crossref

Resolve a DOI to structured citation metadata (title, authors, year, journal, abstract):

```bash
paperbridge resolve-doi --doi "10.1038/nature12373"
```

Validate an item payload with online Crossref cross-checking:

```bash
paperbridge validate-item --file item.json --online
```

## MCP Server

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

- Full usage and command reference: `USAGE.md`
- Contributing and local quality checks: `CONTRIBUTING.md`
