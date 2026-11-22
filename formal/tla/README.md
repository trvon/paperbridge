# formal/tla

TLA+ model of the citation store (`crates/citegraph`): identity merges
and undo, in-flight page fetches, fetch generations, version splits and
review decisions. Design:
[docs/design/citation-graph.md](../../docs/design/citation-graph.md).

## Run

```sh
formal/tla/run.sh              # pull-request set
TLA_FULL=1 formal/tla/run.sh   # also _Split, _Undo2, _PairVeto2, _RedoCycle (nightly in CI)
```

Needs Java 11+ (CI: Temurin 25) and `curl`. Downloads `tla2tools.jar`
v1.7.4 into `$XDG_CACHE_HOME/paperbridge/tla` and checks its SHA-256.

| Variable | Meaning |
|---|---|
| `TLA2TOOLS_JAR` | use this jar (hash still checked) |
| `TLA_CACHE_DIR` | download cache directory |
| `TLC_WORKERS` | `-workers` for passing configs (default `1`); bug and witness configs always use `1` so the reported invariant is deterministic |
| `TLA_FULL` | `1` adds the large configs |

`run.sh` fails unless every passing config exits 0 with "No error has been
found" and at least its minimum distinct-state count, and every bug and
witness config exits 12 with `Invariant <expected> is violated`.

CI: job `formal` in `.github/workflows/ci.yml` (pull requests, pushes to
main, weekly); `.github/workflows/formal-full.yml` (nightly and
`workflow_dispatch`) adds `_Split`, `_Undo2`, `_PairVeto2` and `_RedoCycle`.

## Model

Matches PR #70 (`crates/citegraph/src/store.rs`, `uf.rs`):

- Every external ID gets its own base work; work ids are allocated in
  order. IDs reported together are joined by logged merge rows that record
  the evidence base pair (`base_a`, `base_b`).
- A class is a connected component over the evidence pairs of active rows;
  its root is the smallest work id. Edges and `work_ids` point at base
  works; reads resolve through classes.
- Every write (`CommitPage`, `Merge`, `Attach`, `UndoMerge`, `Redo`,
  `VersionSplit`, `Decide`) is one atomic action, i.e. one `BEGIN
  IMMEDIATE` transaction.

IDs: `A` (seed), `A2`, `B`, `C`; `Arxiv` (constant) marks arXiv IDs.
Pages: generation 1 is `A -> B`, `C -> A` (C is a citing work with no
review row); generation 2 drops `C -> A`. Two-ID records (`Records2`):
`{B, C}`, `{A2, C}`, `{A, A2}`; via `A2`, `C` can join the seed's class,
making `C -> A` a resolved self-loop. With `Rec3 = TRUE` the three-ID
record `<<A2, B, C>>` (primary `A2`) is added; `ObserveRecord` joins the
primary's base to `B`'s and then `C`'s in one transaction, skipping a pair
already in one class.

| Action | Store operation |
|---|---|
| `Resolve`, `FetchPage` | read generation and cursor, then the HTTP await (no transaction) |
| `CommitPage` | `commit_page`: nothing for a finished or superseded generation; else mint base works (`ON CONFLICT DO NOTHING`, read back), upsert edges by base with `last_seen = MAX(last_seen, generation)`, `candidate` for cited roots with no row, `cursor = max(cursor, page)` |
| `Merge` | `observe` with two known IDs: join two classes with a row recording the evidence bases; vetoed if it would reunite the sides of a user-undone pair and (with `VersionVeto`) by mixed versions |
| `Attach` | `observe` with a known and a new ID: mint a base work for the new ID and join it to the known ID's base |
| `ObserveRecord` | `observe` of a three-ID record, all joins in one transaction |
| `UndoMerge` | `undo_merge`: latest active row of its class only; vetoes the pair |
| `Redo` | `redo_merge(m)`: lift m's pair; if its works are in different classes, insert a new row, refused (writes nothing) if any other separation would be reunited |
| `RedoMany` | `redo_merges({m1, m2})`: lift both pairs, join each exempt from both, all-or-nothing |
| `RedoRefused` | marker: a single redo is refused by another separation |
| `SkipSame` | `VetoMode = "skippath"` only: a two-ID record already in one class logs its pair |
| `SkipVetoed` | a record for a vetoed pair is skipped |
| `VersionSplit` | `split_versions`, see verdicts |
| `Decide` | `review decide` on a root: writes every row in the class, stamps `decided_seq` |
| `Refresh` | `begin_fetch --refresh`: new generation |
| `Crash`, `Restart` | worker dies between transactions; resumes from the committed cursor |

Merge rows carry the winner/loser roots (`min`/`max`), the evidence pair,
both prior review states (`review_log`), and a ghost `why` (the true
evidence pair, which differs from `ev` only in the `attach_root` bug).
`root` caches the class roots computed from `merges`; it adds no states.

## Invariants

| Invariant | Meaning |
|---|---|
| `TypeOK` | types, including row shape, `reviewLog`, `mseq`, `decidedAt`, `post` |
| `Forest` | active rows form a forest (`UnionFind::load` never sees a cycle) |
| `NoSelfMerge` | winner differs from loser |
| `WinnerIsRoot` | the outermost row's winner is its class root (store.rs `Corrupt` check) |
| `GuardsAgree` | the latest-in-class guard and the IsRoot+same-winner guard allow the same rows |
| `AlwaysUndoable` | every class with an active row has a row both guards allow |
| `EvidenceHolds` | every active row's true evidence pair is in one class |
| `IDsFollowEvidence` | every ID resolves to a class holding a base it was evidenced with |
| `ReadEdgesRootsNoSelfLoop` | read view endpoints are roots; resolved self-loops dropped |
| `EdgeFaithful` | read view = committed raw pairs resolved now |
| `NoOrphanWork` | every work has an external ID |
| `UndoCarriesDecision` | undoing a row whose class was decided afterwards leaves both works with that decision |
| `UndoRestoresPrior` | undoing a row with no later decision gives each work its prior state |
| `NoDecisionLost` | each live decision is the resolved state, or the root is `needs_decision` and an active row of the same class logged it (`review_log` keyed by merge id) |
| `NeverInnerRemoved` | the design never removes a non-outermost row (passing configs only) |
| `CursorImpliesEdges` | every page up to a generation's cursor has its edges stored |
| `CursorMonotonic` | a commit never lowers the cursor |
| `ReplayIdempotent` | a replayed page changes no IDs, works or edges; it may only add a `candidate` row to a root with none |
| `SupersededNoWrite` | a commit to a finished or superseded generation writes nothing |
| `FreshIsNewest` | edges with `last_seen >=` the newest complete generation are exactly that generation's edges |
| `VetoHolds` | PR #70's: every pair whose latest row is a user undo lies in two different classes |
| `NoActiveVetoedRow` | no active row has a vetoed evidence pair |
| `NoSpuriousVeto` | only pairs that were undone are vetoed, and a skipped record's pair was undone |
| `NoReunionWithoutRedo` | the two classes a user undo split are never one class again unless a `Redo` joins them |

Deadlock checking is on but vacuous: an idle worker can always `Resolve`.

## Configs

Passing configs (all `NW = 4`, 2 workers, 1 crash, 1 review):

| Config | Budgets | Purpose |
|---|---|---|
| `MergeFetchDecide` | 2 pages, 1 undo, `VersionVeto`, no arXiv IDs | main |
| `_Gen` | 2 generations, no undo | refresh, supersession, staleness |
| `_GenNoGuard` | `_Gen` with `gen_no_guard`, without `SupersededNoWrite` | shows MAX stamping alone keeps freshness correct |
| `_SplitVeto` | `B` arXiv, `VersionVeto`, 1 split, adds `NeverSplit` | with the join-time veto, `version_split` is unreachable |
| `_Split` (nightly) | `B` arXiv, no version veto, 1 undo, 1 split | version split on legacy rows |
| `_Undo2` (nightly) | 2 undos | undo, redo, undo |
| `_Rec3` | main + three-ID record, class-level veto | sticky undo with multi-ID records |
| `_RedoCycle` (nightly) | records `{A, A2}`, `{A2, C}`, `{B, C}`, `{A, B}`, 2 undos, no crash | two undone pairs separating the same classes; redo and `redo_merges` |
| `_PairVeto2` (nightly) | main with the pair-level veto (PR #70), two-ID records only | pair-level is enough without three-ID records |

`VetoMode` is `"class"` (the design) in every config except the veto
comparisons below.

Seeded bugs (one per obligation; the invariant TLC reports):

| Config | Defect | Violates |
|---|---|---|
| `_BugStaleSnapshotRoot` | commit uses a root resolved before the await, where an undo/merge changed it during the await | `EdgeFaithful` |
| `_BugStaleSnapshotNoReadBack` | mints for IDs new at snapshot time and uses the minted id without reading back | `EdgeFaithful` |
| `_BugStaleSnapshotOrphan` | mints for IDs new at snapshot time, reads back | `NoOrphanWork` |
| `_BugMergeOverwrite` | merge keeps the winner's state | `NoDecisionLost` |
| `_BugMergeNoFind` | merge uses bases, not classes | `Forest` |
| `_BugStoreRoot` | edges stored by root | `CursorImpliesEdges` |
| `_BugStoreRootUndo` | same, without replay/cursor/fresh checks | `EdgeFaithful` |
| `_BugSplitCommit` | cursor and edges in two transactions | `CursorImpliesEdges` |
| `_BugCursorRegress` | `cursor = page` (3 pages) | `CursorMonotonic` |
| `_BugCandUnconditional` | `candidate` written over existing state | `NoDecisionLost` |
| `_BugDecideRootOnly` | decide writes the root row only | `UndoCarriesDecision` |
| `_BugUndoRestoresLoser` | undo restores the loser's old state despite a later decision | `UndoCarriesDecision` |
| `_BugUndoNotLifo` | undo guard is IsRoot only | `WinnerIsRoot` |
| `_BugUndoInner` | undo guard is same-winner LIFO only | `NoDecisionLost` |
| `_BugVetoRoots` | veto keyed on roots | `NoSpuriousVeto` |
| `_BugVetoIgnored` | merge ignores the veto | `VetoHolds` |
| `_BugAttachRoot` | join row records the matched ID's root, not its base (`_Undo2`) | `EvidenceHolds` |
| `_BugRedoOldRow` | redo reactivates the old row (`_Undo2`) | `WinnerIsRoot` |
| `_BugStampOverwrite` | `last_seen = generation`, not MAX (`_Gen`) | `FreshIsNewest` |
| `_BugGenNoGuard` | superseded commits write (`_Gen`) | `SupersededNoWrite` only |
| `_BugSplitBypass` | PR #70 `split_versions`: undo regardless of the guard (`_Split`) | `NoDecisionLost` |
| `_BugSplitMarkNd` | blocked split marks the root `needs_decision` (`_Split`) | `UndoCarriesDecision` |
| `_BugRec3PairVeto` | pair-level veto with a three-ID record | `VetoHolds` |
| `_BugRec3SkipLog` | option (b): skipped pairs logged against the rows the same record created | `VetoHolds` |
| `_BugRec3SkipPath` | option (b'): skipped pairs logged against every row on their path | `VetoHolds` |
| `_BugRedoUnchecked` | redo not checked against other separations (`_RedoCycle`) | `VetoHolds` |

The last four also violate `NoReunionWithoutRedo` in the same state when
`VetoHolds` is left out.

Witnesses (each `Never*` must be violated): `NeedsDecision`,
`ResolvedSelfLoop`, `SeedCandidateConflict`, `CursorFull`, `Replay`,
`UndoCarried`, `DependentMerge` (a row whose evidence is not its two
roots), `AttachNonRoot`, `NoStateMerge`, `Redo`, `Superseded`,
`StaleEdge`, `Split`, `SplitBlocked`, `Rec3Veto` (a three-ID record hits
the veto), `Rec3Skip` (a three-ID record joins one pair and skips the
other), and `Rec3Redo` (redo with the class-level veto and a three-ID
record), `RedoRefused` (two undone pairs separate the same classes, so
each single redo is refused) and `RedoMany` (`redo_merges` of both
succeeds).

## Verdicts

**Undo guard.** Use the store's guard: only the latest active row of the
class may be undone. While only outermost rows are removed it equals
IsRoot plus same-winner LIFO (`GuardsAgree` holds in every passing config,
and every class always has an undoable row, `AlwaysUndoable`). IsRoot
alone is wrong: it admits an inner undo, after which the class's outermost
row has a winner that is no longer the root (`_BugUndoNotLifo` violates
`WinnerIsRoot`), so PR #70's `Corrupt` check refuses every later undo in
that class. That is the "merge that can never be undone" the store author
described. Same-winner LIFO alone fails `NoDecisionLost` (`_BugUndoInner`).

**version_split bypass.** Unsafe. Undoing an inner row keeps classes
consistent (`Forest`, `EvidenceHolds`, `EdgeFaithful`, `GuardsAgree`,
`AlwaysUndoable` and `WinnerIsRoot` all hold in `_BugSplitBypass`), but
breaks review state: `NoDecisionLost`, `UndoRestoresPrior` and
`UndoCarriesDecision` each fail. Marking the root `needs_decision` instead
also fails (`_BugSplitMarkNd`, `UndoCarriesDecision`): after a later undo
the conflict row can end up on the other side. Safe rule (the design
here): split only when the row is the outermost row of its class;
otherwise change nothing and report the split as pending, so it is
retried once the outer rows are undone. With the join-time
`mixes_versions` veto a direct arXiv-DOI row cannot be created, so
`split_versions` only fires on rows written before that veto existed
(`_SplitVeto` proves it unreachable).

**Sticky undo with three-ID records.** A pair-level veto is bypassed
(`_BugRec3PairVeto`). TLC's shortest trace: `A2-C` and `B-C` are joined,
the user undoes `B-C`, then `<<A2, B, C>>` joins `A2-B`, an unvetoed pair,
and `B` is back in `C`'s class. The re-review's trace (`{B, C}`, then the
record logs only `(A2, B)`, undo, re-observe joins via `(A2, C)`) is the
same defect. Option (b), logging skipped pairs against rows, fails both as
"rows this record created" (`_BugRec3SkipLog`) and as "every row on the
path" (`_BugRec3SkipPath`): a record observed for the first time after the
undo has no logged skips, yet it still asserts the undone equivalence.
Option (a), a class-level veto, passes (`_Rec3`): refuse any join, from
any record or group, that would put the two sides of an undone evidence
pair back into one class. This is the rule PR #70 implements (02cdd40)
and the model follows it exactly. Redo still works: `redo_merge(m)` is
exempt only from m's own pair, so when two undone pairs separate the same
classes each single redo is refused (witness `RedoRefused`) and
`redo_merges` of both succeeds (witness `RedoMany`); `_RedoCycle`,
`_Undo2` and `_Rec3` pass, and skipping the check fails
(`_BugRedoUnchecked`). The pair-level veto is sufficient while records
carry at most two IDs (`_PairVeto2`).

**Generation guard.** With `last_seen = MAX(last_seen, started_seq)`, the
superseded-generation guard is defence in depth: without it
(`_GenNoGuard`) freshness still holds; only `SupersededNoWrite` (rows and
IDs written by a stale commit) fails. Overwriting instead of MAX breaks
freshness (`_BugStampOverwrite`).

## Results

TLC 2.19 (tla2tools v1.7.4), Java 25, Apple M4 Max.

| Config | Result | Generated | Distinct | Depth | Wall (10 workers / 1 for bugs) |
|---|---|---|---|---|---|
| `(main)` | pass | 4,587,769 | 903,600 | 27 | 15 s |
| `_Gen` | pass | 4,803,737 | 1,029,960 | 31 | 16 s |
| `_GenNoGuard` | pass | 3,039,973 | 652,088 | 28 | 13 s |
| `_SplitVeto` | pass | 598,489 | 120,384 | 24 | 5 s |
| `_Rec3` | pass | 11,133,841 | 2,159,976 | 28 | 35 s |
| `_Split` | pass | 25,381,255 | 4,854,312 | 30 | 64 s |
| `_Undo2` | pass | 35,590,873 | 6,663,624 | 30 | 89 s |
| `_PairVeto2` | pass | 4,587,769 | 903,600 | 27 | 15 s |
| `_RedoCycle` | pass | 102,302,881 | 18,440,757 | 29 | 259 s |
| `_BugStaleSnapshotRoot` | `EdgeFaithful` | 5,850 | 2,685 | 8 | 2 s |
| `_BugStaleSnapshotNoReadBack` | `EdgeFaithful` | 1,065 | 522 | 7 | 3 s |
| `_BugStaleSnapshotOrphan` | `NoOrphanWork` | 1,065 | 522 | 7 | 2 s |
| `_BugMergeOverwrite` | `NoDecisionLost` | 2,743 | 1,176 | 8 | 2 s |
| `_BugMergeNoFind` | `Forest` | 27 | 17 | 3 | 2 s |
| `_BugStoreRoot` | `CursorImpliesEdges` | 1,150 | 520 | 7 | 2 s |
| `_BugStoreRootUndo` | `EdgeFaithful` | 3,957 | 1,608 | 8 | 2 s |
| `_BugSplitCommit` | `CursorImpliesEdges` | 29 | 19 | 4 | 2 s |
| `_BugCursorRegress` | `CursorMonotonic` | 7,311 | 2,634 | 10 | 2 s |
| `_BugCandUnconditional` | `NoDecisionLost` | 625 | 271 | 7 | 2 s |
| `_BugDecideRootOnly` | `UndoCarriesDecision` | 946 | 420 | 7 | 2 s |
| `_BugUndoRestoresLoser` | `UndoCarriesDecision` | 946 | 420 | 7 | 2 s |
| `_BugUndoNotLifo` | `WinnerIsRoot` | 4,091 | 1,679 | 8 | 2 s |
| `_BugUndoInner` | `NoDecisionLost` | 9,263 | 3,566 | 9 | 3 s |
| `_BugVetoRoots` | `NoSpuriousVeto` | 78 | 41 | 4 | 2 s |
| `_BugVetoIgnored` | `VetoHolds` | 83 | 42 | 4 | 1 s |
| `_BugAttachRoot` | `EvidenceHolds` | 564 | 250 | 6 | 2 s |
| `_BugRedoOldRow` | `WinnerIsRoot` | 217 | 99 | 5 | 2 s |
| `_BugStampOverwrite` | `FreshIsNewest` | 75,047 | 24,207 | 11 | 5 s |
| `_BugGenNoGuard` | `SupersededNoWrite` | 371,386 | 101,417 | 14 | 11 s |
| `_BugSplitBypass` | `NoDecisionLost` | 10,972 | 4,412 | 9 | 3 s |
| `_BugSplitMarkNd` | `UndoCarriesDecision` | 11,681 | 4,595 | 9 | 2 s |
| `_BugRec3PairVeto` | `VetoHolds` | 531 | 229 | 6 | 2 s |
| `_BugRec3SkipLog` | `VetoHolds` | 531 | 229 | 6 | 3 s |
| `_BugRec3SkipPath` | `VetoHolds` | 844 | 337 | 6 | 2 s |
| `_BugRedoUnchecked` | `VetoHolds` | 9,849 | 3,979 | 8 | 2 s |
| `_WitnessNeedsDecision` | `NeverNeedsDecision` | 854 | 388 | 7 | 2 s |
| `_WitnessResolvedSelfLoop` | `NeverResolvedSelfLoop` | 854 | 388 | 7 | 2 s |
| `_WitnessSeedCandidateConflict` | `NeverSeedCandidateConflict` | 854 | 388 | 7 | 2 s |
| `_WitnessCursorFull` | `NeverCursorFull` | 625 | 271 | 7 | 2 s |
| `_WitnessReplay` | `NeverReplay` | 560 | 234 | 7 | 2 s |
| `_WitnessUndoCarried` | `NeverUndoCarried` | 946 | 420 | 7 | 2 s |
| `_WitnessDependentMerge` | `NeverDependentMerge` | 854 | 388 | 7 | 2 s |
| `_WitnessAttachNonRoot` | `NeverAttachNonRoot` | 27 | 17 | 3 | 2 s |
| `_WitnessNoStateMerge` | `NeverNoStateMerge` | 6 | 6 | 2 | 2 s |
| `_WitnessRedo` | `NeverRedo` | 84 | 43 | 4 | 2 s |
| `_WitnessSuperseded` | `NeverSuperseded` | 17,390 | 6,688 | 10 | 2 s |
| `_WitnessStaleEdge` | `NeverStaleEdge` | 364,771 | 100,754 | 14 | 8 s |
| `_WitnessSplit` | `NeverSplit` | 202 | 94 | 5 | 2 s |
| `_WitnessSplitBlocked` | `NeverSplitBlocked` | 946 | 429 | 7 | 2 s |
| `_WitnessRec3Veto` | `NeverRec3Veto` | 531 | 229 | 6 | 2 s |
| `_WitnessRec3Skip` | `NeverRec3Skip` | 859 | 392 | 7 | 2 s |
| `_WitnessRedoRefused` | `NeverRedoRefused` | 9,187 | 3,979 | 8 | 3 s |
| `_WitnessRedoMany` | `NeverRedoMany` | 9,189 | 3,980 | 8 | 2 s |
| `_WitnessRec3Redo` | `NeverRedo` | 84 | 43 | 4 | 2 s |

Full suite (`TLA_FULL=1`, `TLC_WORKERS=10`): 10 min 24 s.

CI (`ubuntu-latest`, `TLC_WORKERS=auto`), pull-request set (main, `_Gen`,
`_GenNoGuard`, `_SplitVeto`, `_Rec3`, all bugs and witnesses): job 4 min
35 s at 0198a06. `_Split`, `_Undo2`, `_PairVeto2` and `_RedoCycle` (64 s,
89 s, 15 s, 259 s on the M4 Max) run only in the nightly workflow; it can
be dispatched only once `formal-full.yml` is on the default branch, so its
runner time is not yet measured.

## Obligations on the Rust store

1. Merge rows link two current classes, computed inside the merge
   transaction (`Forest`; `_BugMergeNoFind`).
2. `edge_sources` and `work_ids` store base works, never class roots;
   reads resolve through classes and drop resolved self-loops
   (`_BugStoreRoot*`).
3. `commit_page` re-resolves IDs inside its transaction: insert missing
   `work_ids` with `ON CONFLICT DO NOTHING`, read the mapping back, mint no
   work for an ID resolved before the await (`_BugStaleSnapshot*`).
4. `cursor` never moves back (`_BugCursorRegress`); page edges and cursor
   commit in one transaction (`_BugSplitCommit`).
5. A merge writes only the winner root's review row and logs both prior
   states under the merge id (`_BugMergeOverwrite`).
6. `candidate` is written only where the resolved root has no review row
   (`_BugCandUnconditional`).
7. `review decide` writes every row of the class and sets `decided_seq`
   (`_BugDecideRootOnly`).
8. On undo: if `decided_seq >=` the merge's `seq`, both works keep the
   current decision; otherwise the winner gets its logged prior state and
   the loser keeps its row (`_BugUndoRestoresLoser`). `decided_seq` and
   merge `seq` share one logical counter; compare sequence numbers, never
   timestamps.
9. Undo only the latest active row of the class (`_BugUndoNotLifo`,
   `_BugUndoInner`); this also covers a later row that depends on an
   earlier one (witness `DependentMerge`).
10. Merge rows record the evidence bases; a new ID joins the matched ID's
    base, not its root (`_BugAttachRoot`).
11. A user undo is sticky at class level. A join of classes X and Y is
    refused when some evidence pair {a, b} whose latest `merge_log` row
    (highest id for the unordered pair) is undone with `undo_reason =
    'user'` has a in X and b in Y, with classes taken inside the
    transaction just before that join; a refused join writes nothing and
    counts as an identity conflict, and the record's IDs still get their
    own base works. `version_split` undos create no separation.
    `redo_merge(m)` sets `undo_reason = 'redone'` and, if m's works are in
    different classes, inserts a new row; it is exempt only from m's own
    pair and writes nothing if refused. `redo_merges` redoes several in one
    transaction, each exempt from every pair in the set, all-or-nothing
    (`VetoHolds`, `NoReunionWithoutRedo`; `_BugRec3PairVeto`,
    `_BugRec3SkipLog`, `_BugRec3SkipPath`, `_BugRedoUnchecked`,
    `_BugVetoRoots`, `_BugVetoIgnored`, `_BugRedoOldRow`).
12. `version_split` undoes a row only if it is the outermost row of its
    class; otherwise it leaves the store unchanged and reports the split
    as pending (`_BugSplitBypass`, `_BugSplitMarkNd`).
13. Edge `last_seen` is `MAX(last_seen, started_seq)`
    (`_BugStampOverwrite`); a commit to a finished or superseded
    generation writes nothing (`_BugGenNoGuard`).
14. `commit_page`, `observe`, `undo_merge`, `redo_merge`, `split_versions`
    and `decide` each run in one `BEGIN IMMEDIATE` transaction; the model
    has no intermediate states inside them.

Not covered: more than one review, direction or source; strong-ID
contradiction vetoes; `work_versions` aggregation; truncated fetches; a
work split off by an undo that a committed page cites gets no `candidate`
row until the page is replayed (the model allows this; the design should
decide whether undo creates one).
