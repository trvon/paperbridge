# formal/tla

TLA+ model of identity merges, in-flight citation fetches and review
decisions in the citation store. Design:
[docs/design/citation-graph.md](../../docs/design/citation-graph.md)
("Identity", "Concurrency", "Formal verification").

## Run

```sh
formal/tla/run.sh
```

Needs Java 11+ (CI uses Temurin 25) and `curl`. Downloads `tla2tools.jar`
v1.7.4 from the tlaplus GitHub release into
`$XDG_CACHE_HOME/paperbridge/tla` (default `~/.cache/paperbridge/tla`) and
checks its SHA-256. Environment:

| Variable | Meaning |
|---|---|
| `TLA2TOOLS_JAR` | use this jar (hash still checked) |
| `TLA_CACHE_DIR` | download cache directory |
| `TLC_WORKERS` | TLC `-workers` for the main config (default `1`; CI uses `auto`); bug and witness configs always run with `1` so the reported invariant is deterministic |

The script exits non-zero unless the main config passes with TLC's
"No error has been found" and every bug/witness config fails with its
expected `Invariant <name> is violated` line. A parse error, deadlock,
wrong invariant or pass on a bug config is a failure. TLC state files go
to a temp dir.

CI: job `formal` in `.github/workflows/ci.yml`.

## Model

One expansion (seed work, one direction, one source), fetched in 2 pages
by 2 workers (CLI and MCP server) sharing the store. Each store write is
one atomic action, matching `BEGIN IMMEDIATE`.

Constants: works `w1..w3` (symmetric), external IDs `A`, `A2` (two IDs of
the seed), `B`, `C`. Page 1 is `A cites B`, page 2 is `A cites C`. Strong-ID
matches that a fetch may report: `{B, C}` and `{A2, C}` (the second turns
the seed's reference to `C` into a self-citation).

| Variable | Rust concept |
|---|---|
| `idMap` | `work_ids`: external ID to base `work_id` |
| `works` | allocated `works` rows |
| `merges` | active `merge_log` rows (`undone_at IS NULL`) |
| `edges` | `edge_sources` (citing, cited) by base work |
| `decision` | `review_works.state` per work row |
| `reviewLog` | `review_log` prior states kept on conflict |
| `cursor` | `fetch_log.cursor` |
| `pc`, `page`, `snapshot` | per-worker task state; IDs resolved before the await |
| `rawEdges`, `known`, `replayOK` | ghosts for the invariants |

| Action | Constrains |
|---|---|
| `Resolve` | read cursor, resolve page IDs before the HTTP call |
| `FetchPage` | the await; no transaction held |
| `CommitPage` | one transaction: re-resolve IDs, insert missing `work_ids` (`ON CONFLICT DO NOTHING`), insert edges by base work, mark new cited works `candidate`, `cursor = max(cursor, page)` |
| `Merge` | DiscoverEqual + merge in one transaction: roots computed inside, row links two distinct roots; differing states give `needs_decision` with both prior states logged; loser's row kept |
| `UndoMerge` | set `undone_at` |
| `Decide` | `review decide` on the resolved (root) row |
| `Crash` / `Restart` | worker dies between transactions; restart resumes from the committed cursor |

## Invariants

| Invariant | Meaning |
|---|---|
| `TypeOK` | types |
| `SingleParent` | a work is loser in at most one active merge |
| `NoSelfMerge` | no merge row with winner = loser |
| `Acyclic` | following active merges never returns to the start |
| `ResolvesToRoot` | every ID and work resolves to a root |
| `ReadEdgesRootsNoSelfLoop` | read view endpoints are roots; resolved self-loops dropped |
| `EdgeFaithful` | read view = committed raw ID pairs resolved through `work_ids` and active merges, after any merges and undos |
| `NoOrphanWork` | every work is reachable from an external ID |
| `NoDecisionLost` | each live decision is the resolved state, or the resolved row is `needs_decision` and `review_log` holds that state for a work in the class |
| `CursorImpliesEdges` | every page up to the cursor has its edges stored |
| `ReplayIdempotent` | committing an already committed page changes nothing |

## Seeded bugs and witnesses

Each must be rejected. `run.sh` checks the invariant named.

| Config | Change | Violates |
|---|---|---|
| `_BugStaleSnapshot` | commit uses IDs resolved before the await | `EdgeFaithful` (both workers mint a work for `B`; the second edge points at an orphan) |
| `_BugMergeOverwrite` | merge keeps one side's state | `NoDecisionLost` (seed overwritten by candidate) |
| `_BugStoreRoot` | edges stored by root | `ReplayIdempotent` (replay after a merge adds a root edge) |
| `_BugStoreRootUndo` | same, without `ReplayIdempotent` | `EdgeFaithful` (after undo, `C`'s work appears to cite `B`) |
| `_BugSplitCommit` | cursor and edges in two transactions | `CursorImpliesEdges` |
| `_WitnessNeedsDecision` | design | `NeverNeedsDecision`: a conflict is reachable |
| `_WitnessSelfLoop` | design | `NeverResolvedSelfLoop`: an unfiltered resolved self-loop is reachable |

## Results

TLC 2.19 (tla2tools v1.7.4), Java 25, Apple M4 Max, `run.sh`. Bug and
witness rows are always `-workers 1`. For bug rows, depth is the
counterexample length (BFS, so shortest).

| Config | Result | Generated | Distinct | Depth | Wall |
|---|---|---|---|---|---|
| `MergeFetchDecide` (`-workers 1`) | pass | 3,480,539 | 728,304 | 22 | 73-81 s |
| `MergeFetchDecide` (`-workers auto`) | pass | 3,480,539 | 728,304 | 22 | 14 s (14 cores) |
| `_BugStaleSnapshot` | `EdgeFaithful` | 291 | 127 | 7 | 2-3 s |
| `_BugMergeOverwrite` | `NoDecisionLost` | 333 | 125 | 8 | 2 s |
| `_BugStoreRoot` | `ReplayIdempotent` | 2,010 | 852 | 11 | 3 s |
| `_BugStoreRootUndo` | `EdgeFaithful` | 7,333 | 3,158 | 12 | 3 s |
| `_BugSplitCommit` | `CursorImpliesEdges` | 20 | 12 | 4 | 2 s |
| `_WitnessNeedsDecision` | `NeverNeedsDecision` | 333 | 125 | 8 | 2 s |
| `_WitnessSelfLoop` | `NeverResolvedSelfLoop` | 333 | 125 | 8 | 2 s |

## Changes from the design's spec outline

- Edges are stored by base work (the `work_id` an external ID maps to) and
  resolved at read. Storing roots fails replay and undo (`_BugStoreRoot*`).
  "No edge endpoint resolves to a non-root after commit" is stated as
  `EdgeFaithful` plus `ReadEdgesRootsNoSelfLoop`.
- "No self-loops at resolution" is split: no self-merge rows, and the read
  path drops resolved self-loops. Self-loops are reachable from real data
  (`_WitnessSelfLoop`), so they cannot be prevented at merge time.
- `NoDecisionLost` accepts the prior state logged under any work in the
  class: with merges `w2 -> w3` (same state, no log) then `w1 -> w3`
  (conflict), the plan's "both prior states" are `w3`'s and `w1`'s, not
  `w2`'s. TLC found the stricter per-row version false.
- `NoOrphanWork` added; `MaxUndo = 1` added; `Decide` only from
  `candidate` / `needs_decision`. Both bound the state space.
- The snapshot is recorded only in the stale-snapshot variant; the design's
  commit never reads it.
- The design motivates re-resolving inside the transaction with "a merge
  may land during the await". With base-work storage that case is
  harmless; what breaks a pre-await snapshot is a concurrent mint of the
  same ID and an `UndoMerge` during the await.
- No seeded bug needs `Crash`/`Restart`: `_BugSplitCommit` already
  violates `CursorImpliesEdges` in the half-committed state that readers
  can see. Crash is kept so the main config covers restart from the
  committed cursor.
- Not modeled: multiple directions/sources, generations, staleness,
  versions, the strong-ID veto, more than one review.

## Obligations on the Rust store not stated in the design

1. Merge rows link two current roots, computed inside the merge
   transaction (gives `SingleParent`, `Acyclic`).
2. `edge_sources` stores the base `work_id` from `work_ids`, never a
   resolved root; reads resolve through active merges and drop resolved
   self-loops.
3. Commit inserts missing `work_ids` with `ON CONFLICT DO NOTHING` and then
   reads the mapping back in the same transaction; it must never use a
   `work_id` minted before reading the row.
4. `cursor = max(cursor, page)`; a replayed page never moves it back.
5. A merge writes only the winner's review row; the loser's row is kept so
   undo restores it.
6. `candidate` is written only where the resolved root has no state.

Open: the design says any differing states give `needs_decision`, so
`seed` + `candidate` or `included` + `candidate` also conflict. After
`Decide` on a merged row and then undo, the loser's pre-merge state comes
back. Both are allowed by the model; the design should choose.
