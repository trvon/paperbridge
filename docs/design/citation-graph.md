# paperbridge — Citation Graph Design

Status: draft v2, revised after audit (2026-11-22). Not yet implemented.

## Purpose

Give agents the primitives a literature review needs between papers:
references and cited-by lists, citation contexts, retraction and correction
notices, related works, and a persistent record of what a review found and
decided. paperbridge fetches, stores, and ranks. The calling agent judges
relevance and stance. This keeps the AGENTS.md boundary: paperbridge returns
clean data; reasoning stays with the caller.

Must be reviewed against [cli-design.md](cli-design.md) and
[llm-interface.md](llm-interface.md) before any CLI or MCP surface lands.

## Goals

- Snowball both directions from seed papers, resumable across sessions.
- Cache every fetched edge with per-source provenance and time; refetch only
  what can change, within explicit request budgets.
- Show retraction and correction notices on opened and resolved papers.
- Return citation contexts and intents raw, so the agent can classify stance.
- Rank candidates by connection to the review, aware of fetch coverage.
- Record review state (search log, screening decisions) reproducibly.
- Later, optionally project the graph into YAMS (stage 6).

## Non-goals

- Support/contrast labels produced by paperbridge (Scite-style).
- Full-text extraction tables. Batch read is a separate design.
- Bibliometrics beyond what review ranking needs.
- Making YAMS or Zotero the source of truth.
- Auto-merging works on title/author similarity.

## Sources and budgets

Field names, limits and costs must be re-verified in stage 1 and recorded in
this table; values below are the audit's findings as of 2026-11-22.

| Need | Primary | Fallback | Known limits |
|---|---|---|---|
| References | OpenAlex `referenced_works` (in the work object; batch via OR-filter over IDs) | Semantic Scholar `/paper/{id}/references` | — |
| Cited-by | OpenAlex `filter=cites:{W}` with cursor | Semantic Scholar `/paper/{id}/citations` | S2 cannot page past offset+limit 10,000 (live 400) |
| Contexts, intents, influential | Semantic Scholar citations/references `fields=contexts,intents,isInfluential` | none | Citing `paperId` can be null |
| Retraction / correction | Crossref `updated-by` on the retracted work (includes `retraction-watch` source) | OpenAlex `is_retracted` | `update-to` sits on the notice, not the work |
| Related works | OpenAlex `related_works`; S2 recommendations | none | — |

Requirements from the limits:

- `PAPERBRIDGE_OPENALEX_API_KEY`, optional. OpenAlex documents keyed
  access with a daily budget; unkeyed calls still succeeded on 2026-11-22
  (PR #68), so the key raises the budget rather than gating access.
- `PAPERBRIDGE_SEMANTIC_SCHOLAR_API_KEY` is required for review expansion,
  not optional.
- A per-source token bucket. The request router limits concurrency and
  cooldown, not rate (`src/request_router.rs:86-119`).
- Per-round and per-day request budgets per source, reported in
  `diagnostics` when hit, so an expansion stops cleanly instead of failing.
- All HTTP in CI is mocked (AGENTS.md); no live calls.
- Licence notes per source for storing and returning context text.

## Store decision

| Option | Holds non-library papers | Edge refresh | Filterable props | Removal by caller | Verdict |
|---|---|---|---|---|---|
| Zotero | No | API, rate-limited | No | Yes | Output layer only |
| YAMS KG (0.23) | Yes | Yes (upsert) | No (JSON blob) | No | Later projection target |
| Paperseed JSON index | — | Rewrites file | — | — | Unsuitable |
| **SQLite `citegraph.sqlite`** | Yes | Yes | Yes | Yes | **Source of truth** |

### YAMS findings (repo `../yams`, verified 2026-11-22)

- Write path: MCP `graph` tool `action="ingest"` → `KgIngestRequest`
  (`include/yams/daemon/ipc/proto/ipc_envelope.proto:813-845`; handler
  `src/daemon/components/dispatcher/request_dispatcher_graph.cpp:661`).
  Nodes, edges (`relation`, `weight`, JSON `properties`), aliases.
- Edges upsert: `addEdgesUnique` is `ON CONFLICT(src,dst,relation) DO
  UPDATE`; weight = MAX, properties replaced when new weight >= old
  (`src/metadata/knowledge_graph_store_sqlite.cpp:1118-1131`). Nodes upsert
  with properties replaced. Re-ingesting at constant weight refreshes edges.
- `skip_existing_nodes/edges` are parsed (`src/mcp/tool_registry.cpp:1663`)
  and serialized but never read by the handler.
- Response counts echo request sizes, not rows written
  (`request_dispatcher_graph.cpp:789-791`); unresolved edges are dropped
  silently (`src/daemon/components/WriteCoordinator.cpp:1295`).
- No node/edge removal reachable from MCP or CLI. `DeleteEdgesByRelationOp`
  is internal and global (would remove every `cites` edge).
- No CLI ingest: paperbridge only shells out to `yams`
  (`crates/paperseed/src/yams.rs:191`); `yams graph` has no ingest.
- Cleanup hazards: `deleteNodesForDocumentHash` removes nodes whose
  `properties.document_hash` matches (edges cascade); `deleteOrphanedNodes`
  also removes nodes whose `file_path` / `source_file` is not a known
  document and `doc:`/`blob:` keys (`knowledge_graph_store_sqlite.cpp:1948-1971`).
  Re-index recreates `doc:<hash>` nodes, cascading edges to them.
- `yams graph --explore` is CWD-scoped and file-oriented; only
  `--node-key paper:… -r cites` would walk papers.
- `graph_delta_v1` / `graph_adapter_v1` are specs with no implementation.
- Graph sync between daemons is last-writer-wins (`docs/roadmap.md:38`).

Zotero holds curated results: included papers in a collection per review,
screening reasons as tags/notes, written on `review export --to zotero`.

## Data model (`$XDG_DATA_HOME/paperbridge/citegraph.sqlite`)

Schema version in `PRAGMA user_version`; forward-only migrations.

```sql
works(
  work_id INTEGER PRIMARY KEY,       -- opaque, local, never exposed
  title TEXT, year INT, venue TEXT,
  meta_fetched_at TEXT)

work_ids(scheme TEXT, value TEXT, work_id INT, source TEXT, seen_at TEXT,
         PRIMARY KEY(scheme, value))
  -- scheme: doi | openalex | semantic_scholar | arxiv | pmid | zotero | paperseed
  -- value normalized by the single shared normalizer (see Identity)

merge_log(id INTEGER PRIMARY KEY, winner INT, loser INT, reason TEXT,
          evidence TEXT, at TEXT, undone_at TEXT)
  -- append-only; resolution is union-find over rows where undone_at IS NULL

work_versions(work INT, version_of INT, kind TEXT, source TEXT,
              PRIMARY KEY(work, version_of))
  -- kind: preprint | published | revision; versions are never merged

edge_sources(
  citing INT, cited INT, source TEXT,
  first_seen TEXT, last_seen TEXT,
  intents TEXT, is_influential INT,
  PRIMARY KEY(citing, cited, source))
  -- an edge "exists" if any source row is fresh; see Freshness

contexts(citing INT, cited INT, source TEXT, text_hash TEXT, text TEXT,
         first_seen TEXT, PRIMARY KEY(citing, cited, source, text_hash))

unresolved_refs(citing INT, source TEXT, raw TEXT, seen_at TEXT)
  -- citations whose target has no ID (e.g. S2 null paperId)

work_notices(work INT, type TEXT, source TEXT, notice_doi TEXT,
             date TEXT, checked_at TEXT,
             PRIMARY KEY(work, type, source, notice_doi))
  -- type: retraction | correction | expression_of_concern | reinstatement

fetch_log(
  work INT, direction TEXT, source TEXT, generation INT,
  started_at TEXT, finished_at TEXT, cursor TEXT,
  total_reported INT, fetched INT, truncated INT, complete INT,
  PRIMARY KEY(work, direction, source, generation))
  -- direction: refs | cited_by | related | meta | notices
  -- a --refresh starts a new generation; interrupted runs keep their cursor

reviews(review_id TEXT PRIMARY KEY, name TEXT, question TEXT,
        created_at TEXT, deleted_at TEXT)
review_works(review_id, work INT, state, reason, round INT, decided_at,
             PRIMARY KEY(review_id, work))
  -- state: seed | candidate | included | excluded | needs_decision
review_log(review_id, at, action, detail)    -- searches, rounds, decisions, merges
```

### Identity

- `work_id` is an opaque integer. Agents only see `hit_id`s minted by the
  existing rules (`docs/design/llm-interface.md:123-134`); this design adds
  no new `hit_id` schemes. Wire source names follow llm-interface.md
  (`semantic_scholar`, not `s2`).
- One DOI normalizer for the store (consolidate `src/hit_enrich.rs:292`,
  `src/service.rs:2852`, `src/internals.rs:28`), including lowercasing,
  percent-decoding, prefix stripping, and trailing-punctuation removal.
- Merges happen only on a strong-ID match: the same normalized external ID
  reported for two works. The existing strong-ID veto
  (`src/external/mod.rs:593-610`) blocks merges that contradict another ID.
  No title/author merges.
- Merging appends to `merge_log`; nothing is rewritten. Reads resolve
  through union-find over active merges, so a merge can be undone by setting
  `undone_at`.
- Review conflict on merge: if winner and loser have different states in a
  review (any difference, including `seed` vs `candidate`), the resolved row
  becomes `needs_decision`, both prior states are kept in `review_log`, and
  the agent sees it in `review status`. A merge writes only the winner's
  row; the loser's row is kept.
- Undo after a decision: a decision made on the merged row after the merge
  carries to both works when the merge is undone. If no decision was made
  after the merge, undo restores each work's own prior state.
- Versions: preprint and published records are separate works linked in
  `work_versions`. The arXiv DOI (`10.48550/arXiv.*`) and a publisher DOI are
  versions, not a veto conflict. When Semantic Scholar folds both into one
  paper, its ID maps to the published work and the version link is recorded.
  Ranking and display aggregate edges across versions; storage does not.

### Freshness

| Data | Refetch policy |
|---|---|
| References | Once; refetch on `--refresh` (new generation) |
| Cited-by | TTL 30 days for works in an active review, run by `review refresh`; on demand otherwise |
| Notices | TTL 30 days; always checked before `included` |
| Contexts | With the edge fetch that returned them; never pruned automatically |

- `last_seen` per source only moves forward.
- An edge source is **stale** when a later complete fetch of the same work,
  direction and source did not return it. Stale rows are kept for audit;
  ranking and export exclude them.
- Truncated fetches never mark rows stale; output reports `truncated` and
  coverage (`fetched` / `total_reported`).

## Concurrency

The CLI and a long-running MCP server can open the store at the same time.

- WAL mode, `busy_timeout` (5 s), `BEGIN IMMEDIATE` for every write.
- Never hold a transaction across an HTTP await. Fetch a page, then commit
  the page's edges and its cursor in one transaction.
- Resolve external IDs to `work_id` inside the write transaction, not before
  the fetch (a merge may land during the await).
- One writer connection per process; the MCP server serializes writes.
- Add the SQLite dependency (`rusqlite`, bundled) with cargo-deny review.

## Algorithms

All run in Rust over SQLite; review scale (~5k works, ~50k edges) needs no
graph library.

- **Snowball round:** frontier = seeds and included works not expanded in
  the needed direction at the current generation. Fetch refs and cited-by
  within budget; new works enter as `candidate` with `round = n`.
- **Candidate enrichment:** before ranking, batch-fetch reference lists for
  candidates (OpenAlex OR-filter). Without it, co-citation and coupling
  collapse to adjacency.
- **Ranking:** score from edges to seeds/included, co-citation and
  bibliographic coupling with included works, normalized by coverage of the
  works they come from. Global citation count is a tie-breaker. Each
  candidate returns `why` (e.g. `cites seed`, `co-cited with 3 included`) and
  `truncated` when its evidence comes from capped fetches.
- **Saturation:** report how many of the last round's candidates were
  included. The agent decides to stop; termination is not a system property.
- **Notice gate:** moving a work to `included` triggers a notice check if
  stale; a retracted work can be included only with an explicit reason.

## Surface (subject to cli-design / llm-interface review)

`review` is a new top-level domain group. It needs the documented
sprawl-check exception (`cli-design.md:200-204`) and a Canonical Command Map
row (`cli-design.md:237-249`).

CLI:

```
paperbridge papers refs <id> [--limit N] [--refresh]
paperbridge papers cited-by <id> [--limit N] [--offset N] [--refresh]
paperbridge papers related <id>
paperbridge review create <name> --question "..."
paperbridge review list
paperbridge review seed <review> <id>...
paperbridge review expand <review> [--direction both|refs|cited-by] [--budget N]
paperbridge review refresh <review>
paperbridge review candidates <review> [--limit N] [--offset N]
paperbridge review decide <review> <id> include|exclude --reason "..."
paperbridge review status <review>
paperbridge review export <review> [--format json|dot|bibtex] [--to zotero]
paperbridge review delete <review>
```

MCP: `get_citations` (refs / cited-by / related via `direction`),
`review_create`, `review_seed`, `review_expand`, `review_candidates`,
`review_decide`, `review_status`. An MCP-only agent can run a whole review.

Outputs use the llm-interface envelope (`count_kind`, `has_more`,
`diagnostics`) and compact hits plus `why`, `notices`, `edge_sources`,
`truncated`. Contexts only with `include=contexts` and within the 64 KiB
result budget (`llm-interface.md:220`).

## Formal verification

YAMS verifies retrieval topology in Lean 4 (`formal/topology`). Nothing here
is a static theorem, so Lean is not used. paperbridge adds one TLA+ spec,
checked by TLC in CI with a pinned `tla2tools.jar`, under `formal/tla/`.

**`MergeFetchDecide.tla`**: identity merges interleaved with in-flight
fetches and review decisions.

- Variables: `idMap` (external ID → work), `merges` (active merge pairs),
  `edges`, `decision` (review state per work), `cursor`, per-worker `pc`
  and `snapshot` (IDs a worker resolved before its await).
- Actions: `Resolve`, `FetchPage` (await), `CommitPage` (edges + cursor,
  atomic, IDs re-resolved inside), `DiscoverEqual` / `Merge`, `UndoMerge`,
  `Decide`, `Crash` / `Restart`.
- Invariants: every ID resolves to a root; the merge relation is acyclic; no
  self-loops at resolution; no edge endpoint resolves to a non-root after
  commit; no decision is lost by a merge (conflicts become
  `needs_decision`); a committed cursor implies its page's edges are
  present; replaying a page is idempotent.
- Bound: 3 works, 4 IDs, 2 workers, 1 review, 2 pages, 1 crash, symmetry
  on works. Target under 1M states and about a minute in CI; measure and
  record in the spec header.

TLC findings that bind the implementation (PR #69):

1. A merge joins the two current roots, computed inside the merge
   transaction.
2. Edges store the work an external ID first mapped to and resolve through
   active merges at read time; self-loops that appear after resolution are
   dropped. Rewriting edges to roots breaks replay and undo.
3. New external IDs insert with `ON CONFLICT DO NOTHING` and the mapping is
   read back in the same transaction.
4. `cursor = max(cursor, page)`; a replayed page never moves it back.
5. A merge during a fetch's await is harmless under (2). The races that
   matter are another worker creating a work for the same ID, and an undo
   landing during the await.
6. `candidate` is written only where the resolved root has no state.
7. `review decide` on a merged row writes every work row in the class and
   sets `decided_at`.
8. Undo is per merge: if `decided_at` is later than the merge, both works
   keep the current decision; otherwise the winner gets its logged prior
   state and the loser keeps its own row.
9. Undo is LIFO per winner, or the winner's state is recomputed from the
   merges that remain active. Restoring an earlier merge's logged state
   while a later merge is active loses decisions.

Rust mirrors the same invariants with proptest. Fetch-log resume and
sequential review state are covered by proptest only, not TLA+.

## Staging

0. Dependency advisories fixed so hooks pass (PR #66).
1. Notices: Crossref `updated-by` plus OpenAlex `is_retracted` on
   `open_paper` and `resolve_doi`, no store. Batch for search hits later
   (avoid N+1). Re-verify source limits; add API key config.
2. Store and read-through cache: `citegraph.sqlite`, opaque IDs, `work_ids`,
   `edge_sources`, `fetch_log` with truncation, budgets and token buckets,
   `papers refs` / `cited-by` / `related`, `get_citations`. Lands with
   `MergeFetchDecide.tla` and its CI job.
3. Review state and `review *` commands, enrichment, ranking, saturation.
4. Skill recipe: literature review workflow in `docs/skill.md`.
5. Zotero export of review results.
6. Optional: YAMS projection, after the YAMS PR.

## YAMS projection (stage 6, optional)

One-way from SQLite, with node key `paper:<stable key>`, where the stable key
is the strongest external ID at projection time and re-keying is recorded.
Node properties never include `document_hash`, `file_path`, or `source_file`.
Edge relation `cites`, properties `{sources, intents, is_influential}` at a
constant weight so upserts refresh them. Held papers link with `held_as` to
`doc:<hash>`, re-projected after re-index.

Constraints from yams#607 (graph ingest/remove):

- Every projected node and edge carries `properties.ns = "paperbridge"`;
  only namespaced rows survive document and orphan cleanup.
- Never set `snapshot_id` or `canonical_key` (snapshot pruning deletes them).
- Read `unresolved_edges` from ingest output; the CLI exits 0 even when
  edges were dropped.
- Ingest is chunked and not atomic; recover from a failure by re-ingesting
  (upserts are idempotent).
- Remove with `--ns paperbridge --relation cites` for edge refreshes;
  removing the namespace without a relation also drops foreign edges that
  touch our nodes.
- `held_as` edges to `doc:<hash>` still vanish on re-index; re-project.
- Detect support via `yams graph ingest --help` or by handling the error;
  there is no capability flag.

### Parallel YAMS PR (prerequisite for stage 6)

Implemented as yams#607 (base `experimental`).

1. `yams graph ingest --file <json>` CLI (paperbridge talks to YAMS via CLI).
2. Truthful ingest counts (rows inserted / updated / dropped) and report
   unresolved edges instead of dropping them silently.
3. Scoped removal: delete nodes and edges by a caller namespace property
   (e.g. `ns=paperbridge`), never global by relation.
4. Exempt namespaced external nodes from document-hash and orphan cleanup.
5. Remove or implement the dead `skip_existing_*` flags.

## Open questions

- New crate (`citegraph`) or module in `paperseed`? Leaning new crate.
- Whether `review export --to zotero` should also run on `decide`.
- Exact OpenAlex credit costs per request type (sources disagree).
