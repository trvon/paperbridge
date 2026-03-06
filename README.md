# paperbridge

`paperbridge` is a Rust MCP + CLI bridge for Zotero search, collections, and PDF/full-text retrieval.

## Get Started

```bash
./setup.sh
paperbridge config init --interactive
paperbridge config validate
paperbridge query --q "machine learning" --limit 3
```

For Zotero Desktop local API mode, use:

```bash
paperbridge config set api_base http://127.0.0.1:23119/api
```

## Documentation

- Full usage and command reference: `USAGE.md`
- Contributing and local quality checks: `CONTRIBUTING.md`

## Run locally

```bash
paperbridge serve
```
