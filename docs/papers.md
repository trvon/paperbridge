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

## Optional institutional access

Setup accepts an OpenURL holdings resolver and, independently, an EZproxy or
OpenAthens authentication gateway without storing institutional credentials:

```bash
paperbridge config doctor --setup
paperbridge papers access --doi "10.1038/nature12373"
paperbridge papers access --url "https://example.org/article"
```

`institution_access_mode` controls selection:

- `off` (default): never rewrite source URLs
- `fallback`: keep discovered OA links direct, then use institutional holdings
- `prefer`: try institutional holdings before direct/OA access

In `fallback` mode, an Unpaywall OA PDF remains direct. In `prefer` mode—and
for closed-source fallback access—Paperbridge asks the resolver which
subscribed providers offer full text, returns ranked
`access_options`, and opens the best trusted option. `holdings_status`
distinguishes available, unavailable, unparsed, failed, and unchecked
resolution. Each option reports `auto_open`; foreign cross-origin options remain
visible but require manual selection. If lookup fails, Paperbridge refuses an
automatic browser launch and leaves the URL in JSON for inspection. Pass
`--no-open` for JSON-only automation. Paperbridge does not capture credentials
or browser cookies.

Legacy `institution_access_url` values are auto-classified as either a resolver
or gateway. New profiles should use `institution_resolver_url` and
`institution_gateway_url` so holdings decisions remain separate from sign-in.

## Fetch through your signed-in Chrome

`papers access` only resolves and opens a route. `papers fetch` goes the rest
of the way for one DOI: it opens the route in a browser you are signed in to,
finds the PDF, verifies it, and ingests it under its DOI.

### Browser choice for `papers access`

Order: `--browser <cmd>`, config `browser_command`
(`PAPERBRIDGE_BROWSER_COMMAND`), `$BROWSER` (colon-separated list), then the
desktop default (`xdg-open`, `open`). The command is split on whitespace and
run without a shell; `%s` marks the URL position, otherwise the URL is appended.

### Start Chrome

Chrome refuses remote debugging on its default profile, so use a dedicated
profile directory and sign in to your institution in that window once:

```bash
google-chrome-stable --remote-debugging-port=9222 --no-first-run --no-default-browser-check --user-data-dir="$HOME/.local/share/paperbridge/chrome-profile"
```

Keep that Chrome window open for as long as you fetch. Closing it removes the
DevTools endpoint ("No Chrome DevTools endpoint answered"), and the
institutional sign-in may be a session cookie that does not survive a restart.
`--no-first-run --no-default-browser-check` matter on a fresh profile: without
them Chrome waits on its first-run screens and opens no DevTools port.

### Keep the session across restarts

The proxy's cookie is usually a session cookie, and Chrome drops session
cookies when it starts unless the profile is set to "On startup: Continue
where you left off". Paperbridge can start the browser with that set:

```bash
paperbridge browser start     # starts Chrome on the Paperbridge profile; sign in once
paperbridge browser status    # endpoint, profile, restore setting, gateway cookie counts
paperbridge papers fetch --doi 10.1145/3460120.3484766
paperbridge browser stop      # clean exit, so cookies are written to the profile
```

- `browser start` does nothing if a browser already answers at `--cdp-url`.
  Otherwise it writes `"session": {"restore_on_startup": 1}` into
  `<profile>/Default/Preferences` (other keys are kept; a new profile gets a
  minimal file) and launches Chrome with
  `--remote-debugging-port=<port> --no-first-run --no-default-browser-check --user-data-dir=<profile>`.
  The file is edited only while no Chrome runs on the profile (Chrome's
  `SingletonLock`). One browser per profile directory.
- The profile is `$XDG_DATA_HOME/paperbridge/chrome-profile`
  (`~/.local/share/paperbridge/chrome-profile`), the same directory as the
  manual launch line; `--profile <dir>` overrides it. The Chrome command is
  `--chrome <cmd>`, else `browser_command` when it names Chrome or Chromium,
  else `google-chrome-stable`, `google-chrome`, `chromium`.
- On the Microsoft sign-in page, tick **Stay signed in**. That gives the
  identity provider a persistent cookie, so when the proxy's own cookie
  expires the next fetch passes through sign-in without a prompt.
- `papers fetch --launch-browser` starts that Chrome when none answers and
  closes it when the run ends (`--keep-browser` leaves it running). Add
  `--headless` to run without a window once you have signed in headed; a
  sign-in page in a headless run returns `auth_required` with "Run once
  without --headless to sign in". A headed browser that lands on a sign-in
  page is left open for you.
- `browser stop` closes only the Chrome running on the profile (`--profile`,
  default the Paperbridge profile): the process holding that profile's lock
  must have been started with the endpoint's `--remote-debugging-port`. Any
  other browser answering at `--cdp-url` is refused, with a message naming
  what answers there. `--dry-run` reports what would be closed. The report's
  `status` is `stopped` only when Chrome has exited and released the profile;
  if it is still running after 15 seconds the status is `not_stopped` and the
  command exits non-zero, because its cookies may not have been written.
- `--cdp-url` may be the browser's `ws://` address instead of `http://`.
  `status` and `stop` work on it, and `start` reports a browser that already
  answers there. Starting a browser needs the `http://` form: a `ws://`
  address carries an id that Chrome generates at each start.
- A browser Paperbridge starts runs in its own process group, so it outlives
  the shell or agent that ran `browser start`.
- `browser status` asks Chrome for its cookie list and reports only how many
  cookies it holds for the gateway host, how many are session cookies, and the
  earliest and latest expiry. A cookie counts when the browser would send it
  to that host: its domain is the host, or it is a domain cookie for the host
  or a parent of it. Cookies scoped to a subdomain of the host do not count. Names and values are discarded unread and are
  never printed, logged, or stored.

Checked with headless Chrome 157 on Linux: a session cookie set before
`browser stop` was sent again after a restart with `restore_on_startup = 1`,
and was gone after a restart without it
(`cargo test --test fetch_cdp -- --ignored session_cookie`). Chrome may still
discard cookies after a crash or a kill; use `browser stop` or close the
window normally.

### Privacy and guardrails

- Paperbridge stores no institutional username, password, or cookie, and never
  reads a cookie value. The session lives only in the browser profile
  directory, which is yours.
- It attaches over the Chrome DevTools Protocol on loopback only; `--cdp-url`
  rejects any non-loopback address.
- It never types into a page and reads no page storage. Cookies are touched
  only by `browser status`, which keeps counts and expiry times. A sign-in page
  ends the attempt with `auth_required`. A sign-in is an identity-provider
  host (Microsoft, Google, Okta, OpenAthens, `idp.`/`sso.`/`cas.` hosts), a
  SAML/Shibboleth/CAS/ADFS path, a publisher login form, or a proxy `/login`
  page that has a password field or is still holding the target URL. A host
  named `login.<proxy>` is not a sign-in by itself; the tab is brought to the front and left open.
  Sign in, then rerun the same command.
- It fetches only what your session can already read, from page context with
  the browser's own session. It does not work around a paywall: a page that
  answers with HTML instead of a PDF is reported as `no_pdf`.
- One tab at a time. A batch pauses `--delay-secs` (default 5) after each paper
  that opened a tab, skips papers already in the corpus, and stops opening
  tabs for a route host that asked for a sign-in.

### Commands

```bash
paperbridge papers fetch --doi 10.1145/3460120.3484766
paperbridge papers fetch --doi 10.1145/3460120.3484766 --no-ingest --out ~/papers
paperbridge papers fetch --doi-file dois.txt          # one JSON line per paper
```

Result fields: `doi`, `status`, `reason`, `title`, `route_url`, `landing_url`,
`route`, `provider`, `rule`, `pdf_url`, `bytes`, `saved_path`, `ingested`, `cached`,
`paper_id`, `replaced`, `extractor`, `extraction`, `next`.

| `status` | Meaning | What to do |
|---|---|---|
| `ok` | PDF verified (`%PDF-` magic, 8 KiB floor, `%%EOF` trailer) and ingested or saved; `cached: true` when the corpus already had it | `paperbridge papers open --doi <doi> --want fulltext` |
| `auth_required` | the route or PDF link is a sign-in page | sign in in the tab left open, rerun |
| `no_pdf` | no rule matched, or every candidate returned something other than a PDF | check entitlement with `papers access --doi <doi>` |
| `gateway_menu` | every route ended on EZproxy's "Database Menu": the proxy did not recognise the target URL. Not a sign-in problem; the tab is closed and later papers in a batch are still attempted | check the route with `papers access --doi <doi> --no-open`; the publisher may not be in the proxy's configuration |
| `not_entitled` | the publisher page was reached but shows a preview only (for example Springer's `"Access Type":"no-access"`): the institution does not license the item. Not a sign-in problem | request it through interlibrary loan or from the authors |
| `failed` | DOI not resolvable, Chrome not reachable, unverified route, or ingest error | follow `reason` |

### Routes

`route` says how the publisher page was reached:

| `route` | Meaning |
|---|---|
| `direct` | the resolved access route led to the publisher page (or the PDF) |
| `resolver_menu` | the route was the link resolver's menu page (the configured `institution_resolver_url` host); its best link was followed: "Full text" first, then "Article", then "Journal", up to three |
| `gateway_publisher` | the sign-in gateway (`institution_gateway_url`) wrapping the publisher's landing page from Crossref (an `http://` landing page is wrapped as `https://`: the proxy's insecure form is refused by Chrome); tried first, because proxy configurations match publisher hosts |
| `gateway_doi` | the sign-in gateway wrapping `https://doi.org/<doi>` |

The gateway routes are used when no resolver-menu link produced a PDF, or when
the first route ended on the proxy's database menu. For EZproxy the target is
appended to `url=` unencoded
(`https://proxy.example.edu/login?url=https://doi.org/10.1145/3460120.3484766`):
EZproxy reads everything after `url=` literally and shows its database menu for
an encoded target. OpenAthens redirector URLs keep the encoded form.

A link resolver sometimes answers with its menu page instead of redirecting.
Paperbridge reads that page's links only when the page is on the configured
resolver's origin, follows them in the same tab, and applies the provider
rules to wherever they land. A sign-in page on any of these routes is
`auth_required`.

### Provider rules

| Provider | Detected by host (direct or proxied) | PDF rule | How it is fetched |
|---|---|---|---|
| ACM | `dl.acm.org` | `/doi/pdf/<doi>?download=true` | same-origin `fetch` from the landing page |
| Springer | `link.springer.com` | `/content/pdf/<doi>.pdf` | same-origin `fetch` |
| IEEE Xplore | `ieeexplore.ieee.org` | `/stampPDF/getPDF.jsp?tp=&arnumber=N&ref=` (`N` from the URL or page) | same-origin `fetch` |
| ScienceDirect | `sciencedirect.com` | the page's `pdfft` link (or `/science/article/pii/<PII>/pdfft`) | tab navigates to the link, then fetches the PDF it lands on |
| Wiley | `onlinelibrary.wiley.com` | `/doi/pdfdirect/<doi>?download=true`, then `citation_pdf_url` | same-origin `fetch` |
| Any other | — | `<meta name="citation_pdf_url">` | same-origin `fetch`, or navigation when the link is on another origin |

A host is a provider's when it is that host or a subdomain of it. A proxied
host is recognised in both EZproxy forms, `dl.acm.org.ezproxy.example.edu` and
`dl-acm-org.example.idm.oclc.org`, and only under the host of
`institution_gateway_url` (for a `login.<proxy>` gateway, also under
`<proxy>`). A host that merely contains a publisher's name, such as
`dl.acm.org.other.example`, is treated as any other site. A
`citation_pdf_url` that names the publisher's own host is rewritten onto the
proxied origin of the landing page, so the request carries your session.

### Ingest, dedupe, and extraction fallback

Metadata comes from Crossref (`papers resolve-doi`): title, authors, year,
venue, abstract, DOI; license `user-owned-private`. An existing corpus entry
with the same DOI, or a DOI-less entry with the same file or title, is
replaced, leaving one entry (`replaced` lists removed ids).

After ingest, the built-in extractor's text is compared with `pdftotext`
(poppler) when that command is installed. If the built-in text is under 50% of
the `pdftotext` character count, or under 200 characters per page, the
`pdftotext` text is stored as the full text; the PDF is kept either way.
`extractor` reports `builtin`, `pdftotext`, or `none`, and `extraction` gives
both character counts, the page count, and the reason. If the built-in
extractor fails or its PDF library panics on a malformed file, the PDF is
ingested anyway and `pdftotext` supplies the text; the reason then starts with
`builtin_failed` or `builtin_panicked`.

MCP tool: `fetch_paper { doi, cdp_url?, force? }` — same flow and guardrails,
one paper per call, always ingests (no file output).

## External paper search

Search across arXiv, Crossref, OpenAlex, Europe PMC, DBLP, OpenReview, PubMed, HuggingFace Papers, Semantic Scholar, CORE, NASA ADS, and ScholarAPI in one call. Sources run in parallel; failures and timeouts per source are non-fatal and log only at `debug` level.

Springer journal articles are covered indirectly through general scholarly
indexes such as Crossref and OpenAlex. There is no dedicated `springer` value
for `--sources`, and Springer-hosted full text is not guaranteed unless an open
PDF, Zotero attachment, or Paperseed copy is available.

CLI results are human-readable by default. Add the global `--json` flag for
machine-readable output, for example `paperbridge --json papers search ...`.

```bash
paperbridge papers search -q "vision transformers" --limit 5
paperbridge papers search "vision transformers" --limit 5
paperbridge papers search -q "attention is all you need" --sources arxiv,openalex,semantic_scholar
paperbridge papers search -q "attention is all you need" --sources paperseed
paperbridge papers search -q "What drives detection in GNN" --sources research
paperbridge papers search -q "CRISPR Cas9" --sources europe_pmc,pubmed
paperbridge papers search -q "graph neural networks" --sources dblp,openreview
```

`-q`, `--q`, and the positional query form are equivalent; prefer `-q` for Unix-style shorthand. `--q` remains supported for compatibility.

Cache behavior: by default cache-only hits surface only for strong matches, while duplicate cached copies are annotated/preferred. `--sources paperseed` searches only the local cache; adding `paperseed` alongside live sources intentionally blends cache hits; `--cache off` disables cache lookup.

Source values for `--sources`: `research` (alias `yams`), `arxiv`, `paperseed`, `crossref`, `openalex` (alias `oa`), `europe_pmc` (alias `epmc`), `dblp`, `openreview` (alias `or`), `pubmed` (alias `pm`), `hugging_face` (alias `hf`), `semantic_scholar` (alias `s2`), `core`, `ads` (alias `nasa_ads`), `scholarapi` (aliases `scholar`, `scholar_api`, `scolarapi`).

`research` searches paper projects in global YAMS memory, including source
files whose original working-tree path is no longer on disk. Paper fragments
are grouped into one hit; `access.content_state=ready` means `open_paper` can
assemble readable source sections, while `stale` means only index metadata
survives and the document must be re-indexed. Paperseed imports and OA mirrors
use synchronous verified YAMS adds and persist `yams_hash`; cache summaries
expose `yams_indexed`.

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
