# paperbridge

`paperbridge` is a Rust MCP + CLI bridge for Zotero search, collections, and PDF/full-text retrieval.

## Get Started

```bash
./setup.sh
paperbridge config init --interactive
paperbridge config validate
paperbridge query --q "machine learning" --limit 3
```

## Documentation

- Full usage and command reference: `USAGE.md`
- Contributing and local quality checks: `CONTRIBUTING.md`

## Run locally

```bash
paperbridge serve
```
