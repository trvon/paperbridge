--------------------------- MODULE MergeFetchDecide ---------------------------
(***************************************************************************)
(* Identity merges interleaved with in-flight citation fetches and review  *)
(* decisions, for the paperbridge citation store (citegraph.sqlite).       *)
(* Design: docs/design/citation-graph.md, "Identity", "Concurrency",       *)
(* "Formal verification". Rules the Rust store must follow are listed in  *)
(* formal/tla/README.md, "Obligations".                                   *)
(*                                                                         *)
(* One expansion of the seed is fetched in NPages pages by two workers    *)
(* (CLI and MCP server) sharing one store. Every store write (CommitPage, *)
(* Merge, Attach, UndoMerge, Redo, Decide) is one atomic action, i.e. one *)
(* BEGIN IMMEDIATE transaction.                                           *)
(*                                                                         *)
(* Edges and external IDs point at base works (the work_id an ID was      *)
(* first evidenced with), never at union-find roots; reads resolve        *)
(* through active merges. Merge rows record the evidence base pair.       *)
(*                                                                         *)
(* Bug switches (CONSTANT Bug); each config with Bug # "none" must be     *)
(* rejected by TLC on the invariant named in run.sh. See README.md.       *)
(*                                                                         *)
(* Bound: 3 works (symmetric), 4 IDs, 2 workers, 1 review, 2 pages,       *)
(* 1 crash, MaxUndo 1 (MaxUndo 2 in MergeFetchDecide_Undo2.cfg).          *)
(* Measured with TLC 2.19 (tla2tools v1.7.4), Java 25, Apple M4 Max:      *)
(*   MergeFetchDecide.cfg:       STATS_MAIN *)
(*   MergeFetchDecide_Undo2.cfg: STATS_UNDO2 *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Works,      \* work_id values (model values, symmetric)
    Workers,    \* concurrent writers sharing the store
    A, A2, B, C,\* external IDs; A is the seed's ID
    NoWork,     \* "ID not yet in work_ids"
    MaxCrash,   \* crash budget
    MaxUndo,    \* UndoMerge budget
    Bug         \* bug switch, see README.md

IDs    == {A, A2, B, C}
NPages == 2

\* Raw (citing, cited) ID pairs per page. Page 2 is a citing work with no
\* review row: only cited works are marked candidate, so C enters the store
\* without review state and later merges exercise the NoState arms.
PageEdges(p) == IF p = 1 THEN {<<A, B>>} ELSE {<<C, A>>}
PageIDs(p)   == UNION {{e[1], e[2]} : e \in PageEdges(p)}

\* Records a later fetch may return: the IDs one source reports for one
\* paper. Two known IDs in different classes give a merge; a known and a
\* new ID give an attach. {A, A2} then {A2, C} makes C the seed, so C's
\* citation of A becomes a resolved self-loop.
Records == {{B, C}, {A2, C}, {A, A2}}

States    == {"seed", "candidate", "included", "excluded", "needs_decision"}
NoState   == "none"
SOpt      == States \cup {NoState}
MaxSeq    == 2 + MaxUndo   \* 3 works: <= 2 active merges, +1 per undo

VARIABLES
    \* store
    idMap,     \* work_ids: external ID -> base work (NoWork if absent)
    works,     \* allocated work_ids
    merges,    \* active merge_log rows (undone_at IS NULL):
               \* <<winner, loser, id, winner prior, loser prior, evidence>>
               \* evidence = {base of x, base of y}; priors live in review_log
    edges,     \* edge_sources: <<citing base work, cited base work>>
    decision,  \* review_works.state per work row (one review)
    reviewLog, \* review_log <<merge id, work, prior state>> on conflict
    cursor,    \* fetch_log.cursor: last committed page
    mseq,      \* last merge_log.id (logical clock)
    decidedAt, \* review_works.decided_at as the merge_log.id at decide time
    vetoed,    \* evidence pairs of undone merges not yet redone
    \* workers
    pc,        \* idle | resolved | fetched | half | down
    page,      \* page in flight (0 = none)
    snapshot,  \* ID -> root resolved before the await (snapshot bugs only)
    crashes, undos,
    \* ghosts (not stored by Rust)
    rawEdges,  \* raw ID pairs of every committed page
    evid,      \* <<ID, base>> the evidence each ID mapping came from
    known,     \* <<work, state>> decisions not superseded by a later Decide
    vetoG,     \* evidence pairs undone and not redone (bug-independent)
    everUndone,\* evidence pairs ever undone
    post,      \* <<merge id, state>> decided on the class after that merge
    ok,        \* flags for transition checks
    seen       \* reachability markers for witnesses

store  == <<idMap, works, merges, edges, decision, reviewLog, cursor, mseq,
            decidedAt, vetoed>>
local  == <<pc, page, snapshot, crashes, undos>>
ghost  == <<rawEdges, evid, known, vetoG, everUndone, post, ok, seen>>
vars   == <<store, local, ghost>>

-----------------------------------------------------------------------------
(* Union-find over active merges. *)

IsRoot(w) == ~\E m \in merges : m[2] = w
Parent(w) == IF IsRoot(w) THEN w ELSE (CHOOSE m \in merges : m[2] = w)[1]

\* Bounded by |Works| so that a cyclic merge set cannot hang TLC.
RECURSIVE FindN(_, _)
FindN(w, n) == IF n = 0 \/ IsRoot(w) THEN w ELSE FindN(Parent(w), n - 1)
Find(w) == FindN(w, Cardinality(Works))

RECURSIVE Up(_, _)
Up(w, k) == IF k = 0 THEN w ELSE Up(Parent(w), k - 1)

Resolve1(i) == IF idMap[i] = NoWork THEN NoWork ELSE Find(idMap[i])
Class(r)    == {v \in works : Find(v) = r}
ActiveIds   == {m[3] : m \in merges}

\* Read path: resolve stored edges and drop resolved self-loops.
Resolved  == {<<Find(e[1]), Find(e[2])>> : e \in edges}
ReadEdges == {e \in Resolved : e[1] # e[2]}
Faithful  == {e \in {<<Resolve1(r[1]), Resolve1(r[2])>> : r \in rawEdges} :
                 e[1] # e[2]}

\* Review state after a merge. Any two different states conflict,
\* including seed vs candidate and included vs candidate: the caller
\* resolves every conflict; paperbridge never picks a side.
Combine(sw, sl) ==
    IF sl = NoState THEN sw
    ELSE IF sw = NoState THEN sl
    ELSE IF sw = sl THEN sw
    ELSE IF Bug = "merge_overwrite" THEN sw
    ELSE "needs_decision"

SnapBugs == {"snap_root", "snap_no_readback", "snap_orphan"}

-----------------------------------------------------------------------------
Init ==
    \E s \in Works :
        /\ idMap = [i \in IDs |-> IF i = A THEN s ELSE NoWork]
        /\ works = {s}
        /\ decision = [w \in Works |-> IF w = s THEN "seed" ELSE NoState]
        /\ known = {<<s, "seed">>}
        /\ evid = {<<A, s>>}
    /\ merges = {} /\ edges = {} /\ reviewLog = {} /\ cursor = 0
    /\ mseq = 0 /\ decidedAt = [w \in Works |-> 0] /\ vetoed = {}
    /\ pc = [p \in Workers |-> "idle"]
    /\ page = [p \in Workers |-> 0]
    /\ snapshot = [p \in Workers |-> [i \in IDs |-> NoWork]]
    /\ crashes = 0 /\ undos = 0
    /\ rawEdges = {} /\ vetoG = {} /\ everUndone = {} /\ post = {}
    /\ ok = [replay |-> TRUE, cursor |-> TRUE, carry |-> TRUE,
             restore |-> TRUE, veto |-> TRUE]
    /\ seen = {}

\* Read the committed cursor and pick a page before the await. A worker may
\* pick an already committed page (stale cursor read, retry).
Resolve(p) ==
    /\ pc[p] = "idle"
    /\ \E pg \in 1..(IF cursor < NPages THEN cursor + 1 ELSE NPages) :
        /\ page' = [page EXCEPT ![p] = pg]
        \* Only snapshot bugs read the snapshot; elsewhere it is not
        \* recorded, which keeps it out of the state space.
        /\ snapshot' = IF Bug \in SnapBugs
                         THEN [snapshot EXCEPT ![p] =
                                [i \in IDs |-> IF i \in PageIDs(pg)
                                                 THEN Resolve1(i) ELSE NoWork]]
                         ELSE snapshot
    /\ pc' = [pc EXCEPT ![p] = "resolved"]
    /\ UNCHANGED <<store, crashes, undos, ghost>>

\* The HTTP await. No transaction is held; other actions interleave.
FetchPage(p) ==
    /\ pc[p] = "resolved"
    /\ pc' = [pc EXCEPT ![p] = "fetched"]
    /\ UNCHANGED <<store, page, snapshot, crashes, undos, ghost>>

\* One transaction: re-resolve the page's IDs, insert missing work_ids
\* (ON CONFLICT DO NOTHING) and read them back, insert edges by base work,
\* mark cited works with no review row candidate, cursor = max(cursor, pg).
CommitPage(p) ==
    LET pg   == page[p]
        ids  == PageIDs(pg)
        snap == snapshot[p]
        \* IDs this transaction mints a work for
        new  == IF Bug \in {"snap_no_readback", "snap_orphan"}
                  THEN {i \in ids : snap[i] = NoWork}
                  ELSE {i \in ids : idMap[i] = NoWork}
    IN
    /\ pc[p] = "fetched"
    /\ Bug # "split_commit"
    /\ \E f \in [new -> Works \ works] :
      LET inserted == {i \in new : idMap[i] = NoWork}
          idMap2 == [i \in IDs |-> IF i \in inserted THEN f[i] ELSE idMap[i]]
          base(i) ==
              CASE Bug = "snap_no_readback" /\ i \in new -> f[i]
                \* uses the pre-await resolution only where a merge or undo
                \* changed it during the await, isolating that race from
                \* the separate store-root bug
                [] Bug = "snap_root" /\ snap[i] # NoWork
                     /\ snap[i] # Find(idMap2[i]) -> snap[i]
                [] OTHER -> idMap2[i]
          store1(i) == IF Bug = "store_root" THEN Find(base(i)) ELSE base(i)
          newEdges == {<<store1(e[1]), store1(e[2])>> : e \in PageEdges(pg)}
          cited == {Find(base(e[2])) : e \in PageEdges(pg)}
          decision2 == [w \in Works |->
                          IF w \in cited /\ (decision[w] = NoState
                                             \/ Bug = "cand_unconditional")
                            THEN "candidate" ELSE decision[w]]
          works2  == works \cup {f[i] : i \in new}
          edges2  == edges \cup newEdges
          cursor2 == IF Bug = "cursor_regress" THEN pg
                     ELSE IF pg > cursor THEN pg ELSE cursor
          replay  == pg <= cursor
      IN
      /\ \A i, j \in new : i # j => f[i] # f[j]
      /\ idMap' = idMap2
      /\ works' = works2
      /\ edges' = edges2
      /\ decision' = decision2
      /\ cursor' = cursor2
      /\ rawEdges' = rawEdges \cup PageEdges(pg)
      /\ evid' = evid \cup {<<i, f[i]>> : i \in inserted}
      /\ ok' = [ok EXCEPT
                  !.replay = @ /\ (replay => /\ idMap2 = idMap
                                             /\ works2 = works
                                             /\ edges2 = edges
                                             /\ decision2 = decision),
                  !.cursor = @ /\ cursor2 >= cursor]
      /\ seen' = IF replay THEN seen \cup {"replay"} ELSE seen
      /\ pc' = [pc EXCEPT ![p] = "idle"]
      /\ page' = [page EXCEPT ![p] = 0]
      /\ snapshot' = [snapshot EXCEPT ![p] = [i \in IDs |-> NoWork]]
      /\ UNCHANGED <<merges, reviewLog, mseq, decidedAt, vetoed, crashes,
                     undos, known, vetoG, everUndone, post>>

\* split_commit bug, first transaction: cursor only.
CommitCursor(p) ==
    /\ Bug = "split_commit"
    /\ pc[p] = "fetched"
    /\ cursor' = IF page[p] > cursor THEN page[p] ELSE cursor
    /\ pc' = [pc EXCEPT ![p] = "half"]
    /\ UNCHANGED <<idMap, works, merges, edges, decision, reviewLog, mseq,
                   decidedAt, vetoed, page, snapshot, crashes, undos, ghost>>

\* split_commit bug, second transaction: edges.
CommitEdges(p) ==
    LET pg == page[p] IN
    /\ Bug = "split_commit"
    /\ pc[p] = "half"
    /\ \E f \in [{i \in PageIDs(pg) : idMap[i] = NoWork} -> Works \ works] :
        /\ \A i, j \in DOMAIN f : i # j => f[i] # f[j]
        /\ idMap' = [i \in IDs |-> IF i \in DOMAIN f THEN f[i] ELSE idMap[i]]
        /\ works' = works \cup {f[i] : i \in DOMAIN f}
        /\ evid' = evid \cup {<<i, f[i]>> : i \in DOMAIN f}
        /\ edges' = edges \cup {<<idMap'[e[1]], idMap'[e[2]]>> : e \in PageEdges(pg)}
        /\ decision' = [w \in Works |->
                          IF w \in {Find(idMap'[e[2]]) : e \in PageEdges(pg)}
                             /\ decision[w] = NoState
                            THEN "candidate" ELSE decision[w]]
    /\ rawEdges' = rawEdges \cup PageEdges(pg)
    /\ pc' = [pc EXCEPT ![p] = "idle"]
    /\ page' = [page EXCEPT ![p] = 0]
    /\ snapshot' = [snapshot EXCEPT ![p] = [i \in IDs |-> NoWork]]
    /\ UNCHANGED <<merges, reviewLog, cursor, mseq, decidedAt, vetoed, crashes,
                   undos, known, vetoG, everUndone, post, ok, seen>>

\* DiscoverEqual + Merge, one transaction: a record reports two known IDs
\* in different classes. Roots are computed inside the transaction; the
\* row links the two roots and records the evidence base pair. A pair
\* whose merge was undone is vetoed until Redo. Different states make the
\* winner needs_decision, logging both priors under the merge id; only the
\* winner's row is written.
Merge ==
    \E r \in Records : \E x, y \in r :
        /\ x # y
        /\ idMap[x] # NoWork /\ idMap[y] # NoWork
        /\ LET bx == idMap[x]
               by == idMap[y]
               w  == IF Bug = "merge_no_find" THEN bx ELSE Find(bx)
               l  == IF Bug = "merge_no_find" THEN by ELSE Find(by)
               ev == {bx, by}
               id == mseq + 1
               st == Combine(decision[w], decision[l])
           IN
           /\ IF Bug = "merge_no_find" THEN bx # by ELSE w # l
           /\ Bug = "veto_ignored" \/ ev \notin vetoed
           /\ merges' = merges \cup {<<w, l, id, decision[w], decision[l], ev>>}
           /\ mseq' = id
           /\ decision' = [decision EXCEPT ![w] = st]
           /\ reviewLog' = IF st = "needs_decision" /\ decision[w] # decision[l]
                             THEN reviewLog \cup {<<id, w, decision[w]>>,
                                                  <<id, l, decision[l]>>}
                             ELSE reviewLog
           /\ seen' = IF ev # {w, l} THEN seen \cup {"dependent_merge"} ELSE seen
    /\ UNCHANGED <<idMap, works, edges, cursor, decidedAt, vetoed, local,
                   rawEdges, evid, known, vetoG, everUndone, post, ok>>

\* A record reports a known ID x with a new ID y: y joins x's base work,
\* not x's class root.
Attach ==
    \E r \in Records : \E x, y \in r :
        /\ x # y
        /\ idMap[x] # NoWork /\ idMap[y] = NoWork
        /\ idMap' = [idMap EXCEPT ![y] = IF Bug = "attach_root"
                                           THEN Find(idMap[x]) ELSE idMap[x]]
        /\ evid' = evid \cup {<<y, idMap[x]>>}
        /\ seen' = IF IsRoot(idMap[x]) THEN seen
                   ELSE seen \cup {"attach_nonroot"}
    /\ UNCHANGED <<works, merges, edges, decision, reviewLog, cursor, mseq,
                   decidedAt, vetoed, local, rawEdges, known, vetoG,
                   everUndone, post, ok>>

\* Set undone_at, one transaction. Only an outermost merge may be undone:
\* its winner is a current root and it is the latest active merge with that
\* winner. If the class was decided after the merge (decided_at >= merge
\* id) both works keep that decision (Decide wrote every class row);
\* otherwise each work gets its own prior state. The evidence pair is
\* vetoed so the same record does not re-apply the merge.
UndoMerge ==
    /\ undos < MaxUndo
    /\ \E m \in merges :
        LET w == m[1]
            l == m[2]
            decided == decidedAt[w] >= m[3]
            newW == IF decided THEN decision[w] ELSE m[4]
            newL == IF Bug = "undo_restores_loser" THEN m[5] ELSE decision[l]
            postSt == IF \E e \in post : e[1] = m[3]
                        THEN (CHOOSE e \in post : e[1] = m[3])[2]
                        ELSE NoState
        IN
        /\ Bug = "undo_inner" \/ IsRoot(w)
        /\ Bug = "undo_not_lifo" \/ ~\E m2 \in merges : m2[1] = w /\ m2[3] > m[3]
        /\ merges' = merges \ {m}
        /\ decision' = [decision EXCEPT ![w] = newW, ![l] = newL]
        /\ vetoed' = vetoed \cup {IF Bug = "veto_roots" THEN {w, l} ELSE m[6]}
        /\ vetoG' = vetoG \cup {m[6]}
        /\ everUndone' = everUndone \cup {m[6]}
        /\ post' = {e \in post : e[1] # m[3]}
        /\ ok' = [ok EXCEPT
                    !.carry = @ /\ (postSt # NoState =>
                                      newW = postSt /\ newL = postSt),
                    !.restore = @ /\ (postSt = NoState =>
                                        newW = m[4] /\ newL = m[5])]
        /\ seen' = IF postSt # NoState THEN seen \cup {"undo_carried"} ELSE seen
    /\ undos' = undos + 1
    /\ UNCHANGED <<idMap, works, edges, reviewLog, cursor, mseq, decidedAt,
                   pc, page, snapshot, crashes, rawEdges, evid, known>>

\* Explicit redo lifts the veto on an undone evidence pair; the next
\* matching record may merge again.
Redo ==
    \E v \in vetoed :
        /\ vetoed' = vetoed \ {v}
        /\ vetoG' = vetoG \ {v}
        /\ UNCHANGED <<idMap, works, merges, edges, decision, reviewLog, cursor,
                       mseq, decidedAt, local, rawEdges, evid, known,
                       everUndone, post, ok, seen>>

\* A record whose evidence pair is vetoed is skipped. The check is on the
\* evidence base pair, never on current roots.
SkipVetoed ==
    \E r \in Records : \E x, y \in r :
        /\ x # y
        /\ idMap[x] # NoWork /\ idMap[y] # NoWork
        /\ Find(idMap[x]) # Find(idMap[y])
        /\ {idMap[x], idMap[y]} \in vetoed
        /\ ok' = [ok EXCEPT !.veto = @ /\ {idMap[x], idMap[y]} \in everUndone]
        /\ UNCHANGED <<store, local, rawEdges, evid, known, vetoG, everUndone,
                       post, seen>>

\* review decide on a resolved (root) row: writes every row in its class
\* and stamps decided_at with the current merge_log id. Re-deciding an
\* included/excluded work is left out to bound the state space.
Decide ==
    \E w \in works, s \in {"included", "excluded"} :
        LET cls  == Class(w)
            mids == {m[3] : m \in {m2 \in merges : m2[1] \in cls}}
            rows == IF Bug = "decide_root_only" THEN {w} ELSE cls
        IN
        /\ IsRoot(w)
        /\ decision[w] \in {"candidate", "needs_decision"}
        /\ decision' = [v \in Works |-> IF v \in rows THEN s ELSE decision[v]]
        /\ decidedAt' = [v \in Works |-> IF v \in cls THEN mseq ELSE decidedAt[v]]
        /\ post' = {e \in post : e[1] \notin mids} \cup {<<i, s>> : i \in mids}
        /\ known' = {k \in known : Find(k[1]) # w} \cup {<<v, s>> : v \in cls}
        /\ UNCHANGED <<idMap, works, merges, edges, reviewLog, cursor, mseq,
                       vetoed, local, rawEdges, evid, vetoG, everUndone, ok,
                       seen>>

\* A worker dies between transactions; committed rows stay.
Crash(p) ==
    /\ crashes < MaxCrash
    /\ pc[p] # "down"
    /\ pc' = [pc EXCEPT ![p] = "down"]
    /\ page' = [page EXCEPT ![p] = 0]
    /\ snapshot' = [snapshot EXCEPT ![p] = [i \in IDs |-> NoWork]]
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<store, undos, ghost>>

\* Restart resumes from the committed cursor (Resolve reads it).
Restart(p) ==
    /\ pc[p] = "down"
    /\ pc' = [pc EXCEPT ![p] = "idle"]
    /\ UNCHANGED <<store, page, snapshot, crashes, undos, ghost>>

Next ==
    \/ \E p \in Workers : \/ Resolve(p) \/ FetchPage(p) \/ CommitPage(p)
                          \/ CommitCursor(p) \/ CommitEdges(p)
                          \/ Crash(p) \/ Restart(p)
    \/ Merge \/ Attach \/ UndoMerge \/ Redo \/ SkipVetoed \/ Decide

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Invariants *)

TypeOK ==
    /\ idMap \in [IDs -> Works \cup {NoWork}]
    /\ works \subseteq Works
    /\ \A m \in merges :
         /\ m \in works \X works \X (1..MaxSeq) \X SOpt \X SOpt \X SUBSET works
         /\ Cardinality(m[6]) = 2
    /\ edges \subseteq works \X works
    /\ decision \in [Works -> SOpt]
    /\ reviewLog \subseteq (1..MaxSeq) \X works \X SOpt
    /\ cursor \in 0..NPages
    /\ mseq \in 0..MaxSeq
    /\ decidedAt \in [Works -> 0..MaxSeq]
    /\ vetoed \subseteq SUBSET works
    /\ pc \in [Workers -> {"idle", "resolved", "fetched", "half", "down"}]
    /\ page \in [Workers -> 0..NPages]
    /\ crashes \in 0..MaxCrash
    /\ undos \in 0..MaxUndo
    /\ post \subseteq (1..MaxSeq) \X {"included", "excluded"}

\* Union-find forest: one active parent per work, no cycles, no self-merge.
SingleParent == \A m1, m2 \in merges : m1[2] = m2[2] => m1 = m2
NoSelfMerge  == \A m \in merges : m[1] # m[2]
Acyclic ==
    \A w \in works : \A k \in 1..Cardinality(Works) : Up(w, k) = w => IsRoot(w)

ResolvesToRoot ==
    /\ \A i \in IDs : idMap[i] # NoWork => IsRoot(Find(idMap[i]))
    /\ \A w \in works : IsRoot(Find(w))

\* Every active merge still joins its own evidence: undoing an earlier
\* merge never leaves a later one linking the wrong side.
EvidenceHolds == \A m \in merges : \A b1, b2 \in m[6] : Find(b1) = Find(b2)

\* Every external ID resolves to a class containing a base it was
\* evidenced with, whatever merges were undone.
IDsFollowEvidence ==
    \A i \in IDs : idMap[i] # NoWork =>
        \E e \in evid : e[1] = i /\ Find(e[2]) = Find(idMap[i])

ReadEdgesRootsNoSelfLoop ==
    \A e \in ReadEdges : IsRoot(e[1]) /\ IsRoot(e[2]) /\ e[1] # e[2]

\* The read view equals the committed raw pairs resolved through work_ids
\* and active merges, after any merges and undos.
EdgeFaithful == ReadEdges = Faithful

NoOrphanWork == \A w \in works : \E i \in IDs : idMap[i] = w

\* Each live decision is the resolved state, or the resolved row is
\* needs_decision and an active merge in the class logged that state.
NoDecisionLost ==
    \A k \in known :
        LET r == Find(k[1]) IN
        \/ decision[r] = k[2]
        \/ /\ decision[r] = "needs_decision"
           /\ \E l \in reviewLog : /\ l[1] \in ActiveIds
                                   /\ l[3] = k[2] /\ Find(l[2]) = r

CursorImpliesEdges ==
    \A pg \in 1..cursor : \A e \in PageEdges(pg) :
        /\ idMap[e[1]] # NoWork /\ idMap[e[2]] # NoWork
        /\ <<idMap[e[1]], idMap[e[2]]>> \in edges

CursorMonotonic     == ok.cursor
ReplayIdempotent    == ok.replay
UndoCarriesDecision == ok.carry
UndoRestoresPrior   == ok.restore

\* An undone merge is not re-applied from the same evidence until Redo.
VetoHolds == \A m \in merges : m[6] \notin vetoG
\* A pair that was never undone is never vetoed (veto keyed on evidence).
NoSpuriousVeto == /\ vetoed \subseteq everUndone
                  /\ ok.veto

-----------------------------------------------------------------------------
(* Witnesses: each must be violated, showing the scenario is reachable. *)

NeverNeedsDecision    == \A w \in Works : decision[w] # "needs_decision"
NeverResolvedSelfLoop == \A e \in Resolved : e[1] # e[2]
NeverSeedCandidateConflict ==
    ~\E w \in works :
        /\ decision[w] = "needs_decision"
        /\ \E a, b \in reviewLog :
              /\ a[3] = "seed" /\ b[3] = "candidate"
              /\ Find(a[2]) = w /\ Find(b[2]) = w
NeverCursorFull       == cursor < NPages
NeverReplay           == "replay" \notin seen
NeverUndoCarried      == "undo_carried" \notin seen
NeverDependentMerge   == "dependent_merge" \notin seen
NeverAttachNonRoot    == "attach_nonroot" \notin seen
NeverNoStateMerge     ==
    ~\E m \in merges : m[4] = NoState \/ m[5] = NoState

Sym == Permutations(Works)
=============================================================================
