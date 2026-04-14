# paperbridge

`paperbridge` is a Rust MCP + CLI bridge for Zotero search, collections, PDF/full-text retrieval, and DOI/Crossref resolution.

## Install

### npm

```bash
npm install -g paperbridge
```

### Homebrew (macOS / Linux)

```bash
brew tap trvon/paperbridge
brew install paperbridge
```

### GitHub Releases

Download a pre-built binary from [Releases](https://github.com/trvon/paperbridge/releases) for your platform (macOS arm64, macOS x86_64, Linux x86_64, Windows x86_64).

### From Source

```bash
./setup.sh
```

## Shell Completions

Generate a completion script for your shell and install it:

```bash
# bash (Linux)
paperbridge completions bash | sudo tee /etc/bash_completion.d/paperbridge

# bash (macOS, Homebrew)
paperbridge completions bash > "$(brew --prefix)/etc/bash_completion.d/paperbridge"

# zsh — install to fpath, then enable compinit in ~/.zshrc
mkdir -p ~/.zfunc
paperbridge completions zsh > ~/.zfunc/_paperbridge
# add to ~/.zshrc (once):
#   fpath=(~/.zfunc $fpath)
#   autoload -U compinit && compinit

# zsh — or source directly per-shell (no install):
#   source <(paperbridge completions zsh)

# fish
paperbridge completions fish > ~/.config/fish/completions/paperbridge.fish

# PowerShell — add to $PROFILE
paperbridge completions powershell | Out-String | Invoke-Expression
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`.

## Get Started

```bash
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

## External Paper Search

Search across arXiv, HuggingFace Papers, Semantic Scholar, and Crossref in one call. Sources run in parallel; failures and timeouts per source are non-fatal.

```bash
paperbridge search-papers --q "vision transformers" --limit 5
paperbridge search-papers --q "attention is all you need" --sources arxiv,semantic_scholar
```

Results are deduplicated by DOI → arXiv ID → normalized title+author.

### API keys and source gating

arXiv and Crossref are always queried. HuggingFace and Semantic Scholar are **only fanned out when an API key is configured** — otherwise they are silently skipped (visible at `debug` level) so an unauthenticated run never spams rate-limit warnings.

Set either via `config set` or env vars:

```bash
paperbridge config set hf_token <token>
paperbridge config set semantic_scholar_api_key <key>

# or, at runtime
export HF_TOKEN=...
export SEMANTIC_SCHOLAR_API_KEY=...
```

Env vars (`HF_TOKEN`, `SEMANTIC_SCHOLAR_API_KEY`, or the `PAPERBRIDGE_`-prefixed variants) override values from `config.toml`. To disable a configured key again: `paperbridge config set hf_token unset`.

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
