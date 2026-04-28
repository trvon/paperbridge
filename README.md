# paperbridge

`paperbridge` is a Rust MCP + CLI bridge for Zotero search, collections, PDF/full-text retrieval, and DOI/Crossref resolution.

## For agents

When `paperbridge serve` is registered as an MCP server, fetch the operating guide via `prompts/get` with `name: "paperbridge_skill"` — it enumerates every tool, calling conventions, and recipes. Start there before composing tool calls.

## Install

```bash
# npm
npm install -g paperbridge

# Homebrew (macOS / Linux)
brew tap trvon/paperbridge && brew install paperbridge

# From source
./setup.sh
```

Pre-built binaries (macOS arm64/x86_64, Linux x86_64, Windows x86_64) are published to [GitHub Releases](https://github.com/trvon/paperbridge/releases).

## Get started

```bash
paperbridge config init --interactive
paperbridge config validate
paperbridge status
paperbridge library query -q "machine learning" --limit 3
```

For Zotero Desktop local API mode: `paperbridge config set backend_mode local`.

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
- [Shell completions](docs/completions.md)
- [Agent operating guide (`paperbridge_skill`)](docs/skill.md)
- [Design notes](docs/design/README.md)
- [Contributing and local quality checks](CONTRIBUTING.md)
