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

2. Validate config:

```bash
paperbridge config validate
```

3. Query items:

```bash
paperbridge query --q "machine learning" --limit 3
```

4. Prepare read-aloud chunks:

```bash
paperbridge read-search --q "machine learning" --result-index 0 --max-chars-per-chunk 1200
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
paperbridge serve
```

Generate client snippets:

```bash
paperbridge config snippet --target opencode
paperbridge config snippet --target claude
paperbridge config snippet --target pi
```

## CLI usage

```bash
paperbridge query --q "graph learning" --limit 5
paperbridge collections --top-only
paperbridge read --item-key ITEMA --max-chars-per-chunk 1200
paperbridge read-search --q "graph learning" --result-index 0
paperbridge paper structure --key ITEMA
paperbridge paper query --key ITEMA --selector "sections[0].heading"
paperbridge backend-info
```

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
