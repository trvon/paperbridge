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
paperbridge status
paperbridge library query --q "machine learning" --limit 3
```

For Zotero Desktop local API mode, use:

```bash
paperbridge config set backend_mode local
```

## DOI / Crossref

Resolve a DOI to structured citation metadata (title, authors, year, journal, abstract):

```bash
paperbridge papers resolve-doi --doi "10.1038/nature12373"
```

Validate an item payload with online Crossref cross-checking:

```bash
paperbridge item validate --file item.json --online
```

## External Paper Search

Search across arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed, HuggingFace Papers, Semantic Scholar, CORE, and NASA ADS in one call. Sources run in parallel; failures and timeouts per source are non-fatal.

```bash
paperbridge papers search --q "vision transformers" --limit 5
paperbridge papers search --q "attention is all you need" --sources arxiv,openalex,semantic_scholar
paperbridge papers search --q "CRISPR Cas9" --sources europe_pmc,pubmed
paperbridge papers search --q "graph neural networks" --sources dblp,openreview
```

Source values for `--sources`: `arxiv`, `crossref`, `openalex` (alias `oa`), `europe_pmc` (alias `epmc`), `dblp`, `openreview` (alias `or`), `pubmed` (alias `pm`), `hugging_face` (alias `hf`), `semantic_scholar` (alias `s2`), `core`, `ads` (alias `nasa_ads`).

Results are deduplicated by DOI → arXiv ID → PMID → normalized title+first-author.

### API keys and source gating

**Always on (no key required):** arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed. Setting `ncbi_api_key` upgrades PubMed rate limits (3→10 req/s); setting `unpaywall_email` supplies OpenAlex's polite-pool `mailto=` hint.

**Key-gated (silently skipped when unconfigured):** HuggingFace, Semantic Scholar, CORE, NASA ADS. Skipped sources log at `debug` level so unauthenticated runs don't spam warnings.

Set via `config set` or env vars:

```bash
paperbridge config set hf_token <token>
paperbridge config set semantic_scholar_api_key <key>
paperbridge config set core_api_key <key>
paperbridge config set ads_api_token <token>
paperbridge config set ncbi_api_key <key>         # optional PubMed rate-limit upgrade
paperbridge config set unpaywall_email <email>    # enables OA-PDF enrichment on resolve-doi

# or, at runtime
export HF_TOKEN=...
export SEMANTIC_SCHOLAR_API_KEY=...
export CORE_API_KEY=...
export ADS_API_TOKEN=...
export NCBI_API_KEY=...
export UNPAYWALL_EMAIL=you@example.com
```

Env vars (or the `PAPERBRIDGE_`-prefixed variants) override values from `config.toml`. To disable a configured key again: `paperbridge config set hf_token unset`.

### DOI resolution enrichment

When `unpaywall_email` is configured, `paperbridge papers resolve-doi` enriches the Crossref response with an `oa_pdf_url` (best open-access PDF) from Unpaywall. Omit the email and the field is simply absent — no external call is made.

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
