# Usage

## Install

### Quick install (recommended)

```bash
./setup.sh
```

`setup.sh` will:

- build `paperbridge` in release mode
- install the binary to `~/.local/bin` (or first arg)
- initialize config at `$XDG_CONFIG_HOME/paperbridge/config.toml`
- print OpenCode and Pi configuration snippets

### Manual install

```bash
cargo build --release
cp target/release/paperbridge ~/.local/bin/paperbridge
```

## Get Started

1. Initialize config:

```bash
paperbridge config init --interactive
```

Interactive init now asks `Zotero source (cloud/local)`.

- `cloud` keeps normal Zotero Web API flow.
- `local` configures desktop API defaults (`api_base=http://127.0.0.1:23119/api`, `library_type=user`, `user_id=0`, `api_key=<unset>`).

1. Validate config:

```bash
paperbridge config validate
```

1. Query items:

```bash
paperbridge query -q "machine learning" --limit 3
```

CLI output is readable text by default. Put the global `--json` flag before
the command when a script or agent needs the stable JSON envelope:

```bash
paperbridge --json library query -q "machine learning" --limit 3
```

1. Prepare read-aloud chunks:

```bash
paperbridge read-search -q "machine learning" --result-index 0 --max-chars-per-chunk 1200
```

## Configuration

Precedence:

1. Built-in defaults
2. TOML file (`$XDG_CONFIG_HOME/paperbridge/config.toml`)
3. Environment (`PAPERBRIDGE_*`)

Legacy compatibility: `ZOTERO_MCP_*` env vars are still accepted.

Required:

- `PAPERBRIDGE_LIBRARY_TYPE=user` + `PAPERBRIDGE_USER_ID=<id>`
  or
- `PAPERBRIDGE_LIBRARY_TYPE=group` + `PAPERBRIDGE_GROUP_ID=<id>`

Optional:

- `PAPERBRIDGE_API_KEY=<key>`
- `PAPERBRIDGE_API_BASE=https://api.zotero.org`
- `PAPERBRIDGE_TIMEOUT_SECS=20`
- `PAPERBRIDGE_LOG_LEVEL=info`
- `PAPERBRIDGE_TRACE_SPANS=off|close|json` (per-stage span timings on stderr; see `docs/dev/profiling.md`)
- `PAPERBRIDGE_INSTITUTION_ACCESS_MODE=off|fallback|prefer`
- `PAPERBRIDGE_INSTITUTION_RESOLVER_URL=https://resolver.example.edu/openurl`
- `PAPERBRIDGE_INSTITUTION_GATEWAY_URL=https://proxy.example.edu/login?url=`

Institutional access is optional and disabled by default. `config init
--interactive` and `config doctor --setup` accept separate holdings-resolver and
authentication-gateway URLs but never ask for institutional credentials. The
legacy `PAPERBRIDGE_INSTITUTION_ACCESS_URL` remains supported.

### Local Zotero Desktop API mode

If Zotero Desktop is running with local API enabled:

```bash
paperbridge config set api_base http://127.0.0.1:23119/api
```

### Config commands

```bash
paperbridge config path
paperbridge config get
paperbridge config get library_type
paperbridge config set library_type user
paperbridge config set user_id 123456
paperbridge config resolve-user-id --login username
paperbridge config validate
```

`resolve-user-id` accepts a username or numeric ID and prints the numeric Zotero user ID.

`config init --interactive` accepts user login as:

- Zotero username, or
- numeric Zotero user ID

If `PAPERBRIDGE_API_KEY` is set, init can resolve user ID from the key endpoint.

## MCP usage

Run stdio MCP server:

```bash
paperbridge serve                 # full compatibility surface
paperbridge serve --profile core  # six discovery/read tools
```

MCP returns typed structured content and compatible JSON text. See
[model-output migration notes](docs/design/model-output-remediation.md) for
bounded reads, nullable library totals, fixed search windows, and recovery errors.

Generate client snippets:

```bash
paperbridge config snippet --target opencode
paperbridge config snippet --target claude
paperbridge config snippet --target pi
```

## CLI usage

```bash
paperbridge query -q "graph learning" --limit 5
paperbridge collections --top-only
paperbridge read --item-key ITEMA --max-chars-per-chunk 1200
paperbridge read-search -q "graph learning" --result-index 0
paperbridge papers structure --key ITEMA
paperbridge papers query --key ITEMA --selector "sections[0].heading"
paperbridge papers access --url https://example.org/article  # opens the default browser
paperbridge --json papers access --doi 10.1038/nature12373 --no-open  # JSON only
paperbridge papers access --doi 10.1038/nature12373 --browser google-chrome-stable
paperbridge papers fetch --doi 10.1145/3460120.3484766      # PDF via your signed-in Chrome
paperbridge papers fetch --doi-file dois.txt --out ~/papers # batch, one JSON line per paper
paperbridge backend-info
```

Search the off-disk YAMS research workspace and open the selected paper in a
second call:

```bash
paperbridge papers search -q "What drives detection in GNN" --sources research
paperbridge papers open --hit-id "research:<yams-hash>" --want structure --max-chars 30000
```

`research` (alias `yams`) groups files from one paper project into a stable
hit. `access.content_state=ready` means the hit is openable from YAMS;
`stale` means the indexed content needs to be restored or re-indexed.

Structured-paper output is powered by GROBID (optional, with Docker auto-spawn) and falls back to Zotero's stored full text. See [docs/structured-paper.md](docs/structured-paper.md) for configuration, precedence, timing, and troubleshooting.

## Validation and create flows

Note: write operations currently work in cloud Web API mode. Local Zotero desktop mode remains read-focused for now.

Validate an item payload before writing:

```bash
paperbridge validate-item --file item.json
```

Create a collection:

```bash
paperbridge create-collection --name "P4 Papers"
```

Create an item from JSON:

```bash
paperbridge create-item --file item.json
```

Update a collection from JSON:

```bash
paperbridge update-collection --file collection-update.json
```

Update an item from JSON:

```bash
paperbridge update-item --file item-update.json
```

Delete a collection from JSON:

```bash
paperbridge delete-collection --file delete-collection.example.json
```

Delete an item from JSON:

```bash
paperbridge delete-item --file delete-item.example.json
```

## Fetch through your signed-in Chrome

`papers access` picks its browser in this order: `--browser <cmd>`, the
`browser_command` config key (`PAPERBRIDGE_BROWSER_COMMAND`), `$BROWSER`, then
the desktop default. The command is split on whitespace and run without a
shell; `%s` marks where the URL goes.

`papers fetch` needs a Chrome that you launched with remote debugging and a
dedicated profile, signed in to your institution:

```bash
google-chrome-stable --remote-debugging-port=9222 --no-first-run --no-default-browser-check --user-data-dir="$HOME/.local/share/paperbridge/chrome-profile"
```

Keep that Chrome window open for as long as you fetch. Closing it removes the
DevTools endpoint ("No Chrome DevTools endpoint answered"), and the
institutional sign-in may be a session cookie that does not survive a restart.
`--no-first-run --no-default-browser-check` matter on a fresh profile: without
them Chrome waits on its first-run screens and opens no DevTools port.

```bash
paperbridge papers fetch --doi 10.1145/3460120.3484766
paperbridge --json papers fetch --doi 10.1145/3460120.3484766 --out ~/papers
paperbridge papers fetch --doi 10.1145/3460120.3484766 --no-ingest --out .
paperbridge papers fetch --doi-file dois.txt --delay-secs 10
```

| Flag | Meaning |
|---|---|
| `--doi <doi>` / `--doi-file <file>` | one paper, or a list (one DOI per line, `#` comments) |
| `--cdp-url <url>` | DevTools endpoint, loopback only (default `http://127.0.0.1:9222`) |
| `--out <dir>` | keep a copy of the PDF as `<doi with _>.pdf` |
| `--no-ingest` | save the PDF only; do not add it to the corpus |
| `--force` | fetch even when the corpus already has full text for the DOI |
| `--delay-secs <n>` | pause between papers in a batch (default 5) |
| `--launch-browser` | start Chrome on the Paperbridge profile if none answers; close it afterwards |
| `--headless` | with `--launch-browser`: no window (sign in once without it first) |
| `--keep-browser` | with `--launch-browser`: leave the started Chrome running |

```bash
paperbridge browser start [--headless] [--profile <dir>] [--chrome <cmd>] [--cdp-url <url>]
paperbridge browser status     # endpoint, profile, restore setting, gateway cookie counts
paperbridge browser stop [--dry-run]   # clean exit so cookies are saved; only the Chrome on that profile
```

`browser start` sets the profile to "Continue where you left off"
(`session.restore_on_startup = 1`), which makes Chrome keep the proxy's session
cookie across restarts. On the Microsoft sign-in page, tick "Stay signed in".
`browser status` reports cookie counts and expiry times only, never values.

Each result has `status`: `ok`, `auth_required`, `no_pdf`, `gateway_menu` (the
proxy did not recognise the URL), `not_entitled` (the publisher shows a preview
only), or `failed`, with a `reason`, and `route`
(`direct`, `resolver_menu`, `gateway_publisher`, `gateway_doi`, or, for an
open-access copy, `publisher` or `doi`) for how the
publisher page was reached. A non-`ok` result exits non-zero. Paperbridge stores no credentials
or cookies; the session stays in your browser profile. Details and the
provider rule table: [docs/papers.md](docs/papers.md#fetch-through-your-signed-in-chrome).
