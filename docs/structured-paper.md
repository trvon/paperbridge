# Structured paper parsing (GROBID + fallbacks)

`paperbridge` can return a paper as a typed JSON structure instead of a raw
full-text blob. Agents and scripts can then select into that structure with a
dotted path (e.g. `metadata.doi`, `sections[2].heading`, `references[0].title`)
instead of re-parsing prose.

## The structure

```
PaperStructure {
  item_key:       String,
  attachment_key: Option<String>,
  metadata:       PaperMetadata { title, authors[], year, doi, abstract },
  sections:       [ PaperSection { heading, level, text } ],
  references:     [ PaperReference { title, authors, year, doi, raw } ],
  figures:        [ PaperFigure { label, caption } ],
  source:         PaperStructureSource,
}
```

`source` tells you where the structure came from and how much you can trust
the breakdown:

- `{ "kind": "grobid" }` — parsed via GROBID. Real section hierarchy with
  heading levels, a proper reference list, and figure captions.
- `{ "kind": "zotero_fulltext" }` — GROBID was not configured; the text came
  from Zotero's stored full text. You get `metadata` plus a single
  `sections[0]` containing the whole body. For a lot of agent queries (pull
  the abstract, grep the text, summarize) this is the correct, cheap choice.
- `{ "kind": "grobid_unavailable", "reason": "..." }` — GROBID was configured
  but the call failed; the service fell back to Zotero full text. The
  `reason` string explains what happened (Docker missing, container never
  came up, HTTP error, TEI parse error, …). Agents should surface or retry
  rather than silently trust the body.

## CLI

```bash
paperbridge paper structure --key ABCD1234
paperbridge paper query --key ABCD1234 --selector "metadata.doi"
paperbridge paper query --key ABCD1234 --selector "sections[2].heading"
paperbridge paper query --key ABCD1234 --selector "references[0].title"
```

Both commands accept an optional `--attachment-key` when an item has more than
one PDF attached.

## MCP tools

- `get_paper_structure { item_key, attachment_key? }` — returns the full
  `PaperStructure`.
- `query_paper { item_key, selector, attachment_key? }` — returns the value
  at the selector path.

Selectors are dotted paths with `[i]` bracket indexing. Arrays are indexed
numerically; maps are indexed by key. Out-of-range indices or missing keys
return `null` rather than erroring.

## Configuring GROBID

GROBID is **opt-in**. Out of the box, `paperbridge` returns
`source=zotero_fulltext`. There are three ways to turn on real parsing:

### 1. Point at a remote or pre-running GROBID

```bash
paperbridge config set grobid_url https://grobid.example.org
# or, for a container you already run yourself:
paperbridge config set grobid_url http://localhost:8070
```

This is the right choice if you have a shared GROBID in your infra or you
manage your own container lifecycle.

### 2. Let paperbridge auto-spawn GROBID via Docker

Requires Docker on `PATH`. On first paper request, `paperbridge` runs roughly
`docker run -d --rm -p 8070:8070 <grobid_image>` and waits up to 180s for the
container to report ready on `/api/isalive`.

```bash
paperbridge config set grobid_url unset       # must be unset — see precedence
paperbridge config set grobid_auto_spawn true
# optional: override the image
paperbridge config set grobid_image lfoppiano/grobid:0.8.1
```

The interactive init flow (`paperbridge config init --interactive`) also
prompts for these keys.

### Precedence (the gotcha)

**If `grobid_url` is set, that URL wins — auto-spawn is never attempted.** If
the URL is unreachable the service returns
`source=grobid_unavailable { reason }` + a Zotero full-text body. It does
**not** fall back to spawning a container.

To use auto-spawn, `grobid_url` must be unset. The interactive init encodes
this: the auto-spawn prompt only appears when the URL prompt was left blank.

## Timing

- **Cold start, image not pulled:** ~60–120s while Docker pulls
  `lfoppiano/grobid:0.8.1` (~200MB). You only pay this once per machine.
- **Warm container, first request:** ~5–10s while GROBID loads its models.
- **Steady state:** ~2–4s per paper for GROBID parsing, plus the Zotero PDF
  fetch.
- **Repeat calls within one process:** near-instant. `paperbridge` keeps an
  in-process cache keyed by `(item_key, attachment_key, cache_version)`.

If you auto-spawned the container, it stays up until the process that launched
it exits (or until you `docker stop` it manually). Subsequent `paperbridge`
invocations detect the running instance via `/api/isalive` and reuse it.

## Troubleshooting

All failures surface as `source=grobid_unavailable { reason }` with a Zotero
full-text body — the tool never crashes on a paper, it degrades.

| `reason` contains | What it means | Fix |
|---|---|---|
| `failed to exec \`docker\`` | Docker is not on `PATH` | install Docker, or set `grobid_url` to a remote instance |
| `\`docker run\` failed` | Docker rejected the invocation | check the stderr in the reason; usually port conflict on 8070 |
| `did not become ready within 180s` | Container started but `/api/isalive` never answered | image is probably still pulling — retry in a minute; or `docker logs` the container |
| HTTP 4xx/5xx from GROBID | The PDF upload or parse failed | try a different attachment, or inspect GROBID logs |
| TEI parse error | GROBID returned XML we couldn't parse | open an issue with the item key; fall-back body is still returned |

## Relevant config keys

| key | purpose |
|---|---|
| `grobid_url` | remote or local GROBID endpoint; if set, auto-spawn is disabled |
| `grobid_auto_spawn` | when `grobid_url` is unset, launch GROBID via `docker run` on first request (default `false`) |
| `grobid_image` | Docker image used by auto-spawn (default `lfoppiano/grobid:0.8.1`) |
| `grobid_timeout_secs` | HTTP timeout for GROBID requests (default 120) |
