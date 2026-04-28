# External paper search & DOI resolution

## DOI / Crossref

Resolve a DOI to structured citation metadata (title, authors, year, journal, abstract):

```bash
paperbridge papers resolve-doi --doi "10.1038/nature12373"
```

Validate an item payload with online Crossref cross-checking:

```bash
paperbridge item validate --file item.json --online
```

### DOI resolution enrichment

When `unpaywall_email` is configured, `paperbridge papers resolve-doi` enriches the Crossref response with an `oa_pdf_url` (best open-access PDF) from Unpaywall. Omit the email and the field is simply absent — no external call is made.

## External paper search

Search across arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed, HuggingFace Papers, Semantic Scholar, CORE, NASA ADS, and ScholarAPI in one call. Sources run in parallel; failures and timeouts per source are non-fatal and log only at `debug` level.

```bash
paperbridge papers search --q "vision transformers" --limit 5
paperbridge papers search --q "attention is all you need" --sources arxiv,openalex,semantic_scholar
paperbridge papers search --q "CRISPR Cas9" --sources europe_pmc,pubmed
paperbridge papers search --q "graph neural networks" --sources dblp,openreview
```

Source values for `--sources`: `arxiv`, `crossref`, `openalex` (alias `oa`), `europe_pmc` (alias `epmc`), `dblp`, `openreview` (alias `or`), `pubmed` (alias `pm`), `hugging_face` (alias `hf`), `semantic_scholar` (alias `s2`), `core`, `ads` (alias `nasa_ads`), `scholarapi` (alias `scholar`).

Results are deduplicated by DOI → arXiv ID → PMID → normalized title+first-author.

## API keys and source gating

**Always on (no key required):** arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed. Setting `ncbi_api_key` upgrades PubMed rate limits (3→10 req/s); setting `unpaywall_email` supplies OpenAlex's polite-pool `mailto=` hint.

**Key-gated (silently skipped when unconfigured):** HuggingFace, Semantic Scholar, CORE, NASA ADS, ScholarAPI. Skipped sources log at `debug` level so unauthenticated runs don't spam warnings.

Set via `config set` or env vars:

```bash
paperbridge config set hf_token <token>
paperbridge config set semantic_scholar_api_key <key>
paperbridge config set core_api_key <key>
paperbridge config set ads_api_token <token>
paperbridge config set scholarapi_key <key>
paperbridge config set ncbi_api_key <key>         # optional PubMed rate-limit upgrade
paperbridge config set unpaywall_email <email>    # enables OA-PDF enrichment on resolve-doi

# or, at runtime
export HF_TOKEN=...
export SEMANTIC_SCHOLAR_API_KEY=...
export CORE_API_KEY=...
export ADS_API_TOKEN=...
export SCHOLARAPI_KEY=...
export NCBI_API_KEY=...
export UNPAYWALL_EMAIL=you@example.com
```

Env vars (or the `PAPERBRIDGE_`-prefixed variants) override values from `config.toml`. To disable a configured key again: `paperbridge config set hf_token unset`.
