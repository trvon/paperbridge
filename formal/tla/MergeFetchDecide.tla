--------------------------- MODULE MergeFetchDecide ---------------------------
(***************************************************************************)
(* Identity merges interleaved with in-flight citation fetches and review  *)
(* decisions, for the paperbridge citation store (citegraph.sqlite).       *)
(* Design: docs/design/citation-graph.md, "Identity", "Concurrency",       *)
(* "Formal verification".                                                  *)
(*                                                                         *)
(* One expansion (one work, one direction, one source) is fetched in      *)
(* NPages pages by two workers (e.g. the CLI and the MCP server) sharing  *)
(* one store. Each store write is one atomic action (BEGIN IMMEDIATE).    *)
(*                                                                         *)
(* Edges are stored by the work an external ID maps to in work_ids (its  *)
(* base work), never by the union-find root, and are resolved through     *)
(* active merges at read time. Storing roots breaks under UndoMerge (see  *)
(* Bug = "store_root").                                                   *)
(*                                                                         *)
(* Bug switches (CONSTANT Bug), each must be rejected by TLC:             *)
(*   "none"            the design                                         *)
(*   "stale_snapshot"  CommitPage uses IDs resolved before the await      *)
(*   "merge_overwrite" Merge keeps one side's state, no needs_decision    *)
(*   "store_root"      CommitPage stores resolved roots, not base works   *)
(*   "split_commit"    cursor and edges written in two transactions       *)
(*   "undo_restores_loser" undo gives the loser its pre-merge state even  *)
(*                     after a post-merge decision                        *)
(*   "undo_not_lifo"   undo of a merge that has a later merge with the    *)
(*                     same winner                                        *)
(*                                                                         *)
(* Bound: 3 works (symmetric), 4 IDs, 2 workers, 1 review, 2 pages,     *)
(* 1 crash, 1 undo.                                                       *)
(* Measured with TLC 2.19 (tla2tools v1.7.4), Java 25, -workers 1, Apple  *)
(* M4 Max, via run.sh (all configs in README.md):                        *)
(*   MergeFetchDecide.cfg: 2,212,523 states generated, 488,124 distinct,  *)
(*   depth 22, 75 s wall (16 s with -workers auto).                       *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    Works,      \* work_id values (model values, symmetric)
    Workers,    \* concurrent writers sharing the store
    A, A2, B, C,\* external IDs; A and A2 are two IDs of the seed work
    NoWork,     \* "ID not yet in work_ids"
    MaxCrash,   \* crash budget
    MaxUndo,    \* UndoMerge budget
    Bug         \* bug switch, see header

IDs    == {A, A2, B, C}
NPages == 2
Pages  == 1..NPages

\* Page p of the seed's reference list, as raw (citing, cited) ID pairs.
PageEdges(p) == IF p = 1 THEN {<<A, B>>} ELSE {<<A, C>>}
PageIDs(p)   == UNION {{e[1], e[2]} : e \in PageEdges(p)}

\* Strong-ID matches a later fetch may report: B and C are the same paper;
\* A2 (a second ID of the seed) and C are the same paper, so the seed's
\* reference to C becomes a self-citation once merged.
EqualPairs == {{B, C}, {A2, C}}

States    == {"seed", "candidate", "included", "excluded", "needs_decision"}
NoState   == "none"
\* States that record a caller's decision; candidate is not a decision.
Decisions == {"seed", "included", "excluded"}

VARIABLES
    idMap,     \* work_ids: external ID -> base work (NoWork if absent)
    works,     \* allocated work_ids
    merges,    \* active merge_log rows (undone_at IS NULL):
               \* <<winner, loser, id, winner prior state, loser prior state>>
               \* (prior states live in review_log in Rust)
    edges,     \* edge_sources: <<citing base work, cited base work>>
    decision,  \* review_works.state per work row (one review)
    reviewLog, \* review_log entries <<work, prior state>> kept on conflict
    cursor,    \* fetch_log.cursor: last committed page
    pc,        \* per worker: idle | resolved | fetched | half | down
    page,      \* per worker: page in flight (0 = none)
    snapshot,  \* per worker: ID -> root resolved before the await
    crashes,   \* crashes so far
    undos,     \* undos so far
    \* ghosts (not stored by Rust)
    rawEdges,  \* raw ID pairs of every committed page
    known,     \* <<work, state>> decisions not superseded by a later Decide
    replayOK,  \* FALSE once a replayed page changed the store
    mseq,      \* merge_log.id of the last merge (logical clock)
    decidedAt, \* review_works.decided_at, as the mseq at decision time
    post,      \* ghost: <<merge id, state>> decided on the class after it
    undoCarryOK,   \* ghost: FALSE once an undo dropped a post-merge decision
    undoRestoreOK  \* ghost: FALSE once an undo failed to restore prior states

vars == <<idMap, works, merges, edges, decision, reviewLog, cursor,
          pc, page, snapshot, crashes, undos, rawEdges, known, replayOK,
                   mseq, decidedAt, post, undoCarryOK, undoRestoreOK>>

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

\* Resolution of an external ID, as read inside a transaction.
Resolve1(i) == IF idMap[i] = NoWork THEN NoWork ELSE Find(idMap[i])

\* Read path: resolve stored edges through active merges and drop
\* resolved self-loops.
Resolved == {<<Find(e[1]), Find(e[2])>> : e \in edges}
ReadEdges == {e \in Resolved : e[1] # e[2]}

\* What the read path should return: committed raw pairs, resolved now.
Faithful == {e \in {<<Resolve1(r[1]), Resolve1(r[2])>> : r \in rawEdges} :
                e[1] # e[2]}

\* Review state after a merge. Any two different states conflict,
\* including seed vs candidate and included vs candidate: the caller
\* resolves every conflict, paperbridge never picks a side.
Combine(sw, sl) ==
    IF sl = NoState THEN sw
    ELSE IF sw = NoState THEN sl
    ELSE IF sw = sl THEN sw
    ELSE IF Bug = "merge_overwrite" THEN sw
    ELSE "needs_decision"

\* Works resolving to root r.
Class(r) == {v \in works : Find(v) = r}

-----------------------------------------------------------------------------
Init ==
    \E s \in Works :
        /\ idMap = [i \in IDs |-> IF i \in {A, A2} THEN s ELSE NoWork]
        /\ works = {s}
        /\ decision = [w \in Works |-> IF w = s THEN "seed" ELSE NoState]
        /\ known = {<<s, "seed">>}
    /\ merges = {}
    /\ edges = {}
    /\ reviewLog = {}
    /\ cursor = 0
    /\ pc = [p \in Workers |-> "idle"]
    /\ page = [p \in Workers |-> 0]
    /\ snapshot = [p \in Workers |-> [i \in IDs |-> NoWork]]
    /\ crashes = 0
    /\ undos = 0
    /\ rawEdges = {}
    /\ replayOK = TRUE
    /\ mseq = 0
    /\ decidedAt = [w \in Works |-> 0]
    /\ post = {}
    /\ undoCarryOK = TRUE
    /\ undoRestoreOK = TRUE

\* Read the committed cursor and resolve the page's IDs before the await.
\* A worker may pick an already committed page (stale cursor read, retry).
Resolve(p) ==
    /\ pc[p] = "idle"
    /\ \E pg \in 1..(IF cursor < NPages THEN cursor + 1 ELSE NPages) :
        /\ page' = [page EXCEPT ![p] = pg]
        \* Only the stale-snapshot variant reads the snapshot; elsewhere it
        \* is not recorded, which keeps it out of the state space.
        /\ snapshot' = IF Bug = "stale_snapshot"
                         THEN [snapshot EXCEPT ![p] =
                                [i \in IDs |-> IF i \in PageIDs(pg)
                                                 THEN Resolve1(i) ELSE NoWork]]
                         ELSE snapshot
    /\ pc' = [pc EXCEPT ![p] = "resolved"]
    /\ UNCHANGED <<idMap, works, merges, edges, decision, reviewLog, cursor,
                   crashes, undos, rawEdges, known, replayOK,
                   mseq, decidedAt, post, undoCarryOK, undoRestoreOK>>

\* The HTTP await. No transaction is held; other actions interleave.
FetchPage(p) ==
    /\ pc[p] = "resolved"
    /\ pc' = [pc EXCEPT ![p] = "fetched"]
    /\ UNCHANGED <<idMap, works, merges, edges, decision, reviewLog, cursor,
                   page, snapshot, crashes, undos, rawEdges, known, replayOK,
                   mseq, decidedAt, post, undoCarryOK, undoRestoreOK>>

\* One transaction: resolve the page's IDs (minting works for unknown
\* IDs), insert edges by base work, mark new cited works candidate, and
\* advance the cursor. Under "split_commit" this is replaced by
\* CommitCursor then CommitEdges.
CommitPage(p) ==
    LET pg   == page[p]
        ids  == PageIDs(pg)
        snap == snapshot[p]
        \* IDs this transaction believes are absent from work_ids
        new  == IF Bug = "stale_snapshot"
                  THEN {i \in ids : snap[i] = NoWork}
                  ELSE {i \in ids : idMap[i] = NoWork}
    IN
    /\ pc[p] = "fetched"
    /\ Bug # "split_commit"
    /\ \E f \in [new -> Works \ works] :
      LET \* INSERT ... ON CONFLICT DO NOTHING into work_ids
          idMap2 == [i \in IDs |-> IF i \in new /\ idMap[i] = NoWork
                                     THEN f[i] ELSE idMap[i]]
          \* the work the transaction uses for each ID
          base(i) == IF i \in new THEN f[i]
                     ELSE IF Bug = "stale_snapshot" THEN snap[i]
                     ELSE idMap2[i]
          store(i) == IF Bug = "store_root" THEN Find(base(i)) ELSE base(i)
          newEdges == {<<store(e[1]), store(e[2])>> : e \in PageEdges(pg)}
          cited == {Find(base(e[2])) : e \in PageEdges(pg)}
          decision2 == [w \in Works |-> IF w \in cited /\ decision[w] = NoState
                                          THEN "candidate" ELSE decision[w]]
          works2 == works \cup {f[i] : i \in new}
          edges2 == edges \cup newEdges
          replay == pg <= cursor
      IN
      /\ \A i, j \in new : i # j => f[i] # f[j]
      /\ idMap' = idMap2
      /\ works' = works2
      /\ edges' = edges2
      /\ decision' = decision2
      /\ cursor' = IF pg > cursor THEN pg ELSE cursor
      /\ rawEdges' = rawEdges \cup PageEdges(pg)
      /\ replayOK' = (replayOK /\ (replay => /\ idMap2 = idMap
                                             /\ works2 = works
                                             /\ edges2 = edges
                                             /\ decision2 = decision))
      /\ pc' = [pc EXCEPT ![p] = "idle"]
      /\ page' = [page EXCEPT ![p] = 0]
      /\ snapshot' = [snapshot EXCEPT ![p] = [i \in IDs |-> NoWork]]
      /\ UNCHANGED <<merges, reviewLog, crashes, undos, known,
                     mseq, decidedAt, post, undoCarryOK, undoRestoreOK>>

\* Split-commit bug, first transaction: cursor only.
CommitCursor(p) ==
    /\ Bug = "split_commit"
    /\ pc[p] = "fetched"
    /\ cursor' = IF page[p] > cursor THEN page[p] ELSE cursor
    /\ pc' = [pc EXCEPT ![p] = "half"]
    /\ UNCHANGED <<idMap, works, merges, edges, decision, reviewLog,
                   page, snapshot, crashes, undos, rawEdges, known, replayOK,
                   mseq, decidedAt, post, undoCarryOK, undoRestoreOK>>

\* Split-commit bug, second transaction: edges (minting as needed).
CommitEdges(p) ==
    LET pg == page[p] IN
    /\ Bug = "split_commit"
    /\ pc[p] = "half"
    /\ \E f \in [{i \in PageIDs(pg) : idMap[i] = NoWork} -> Works \ works] :
        /\ \A i, j \in DOMAIN f : i # j => f[i] # f[j]
        /\ idMap' = [i \in IDs |-> IF i \in DOMAIN f THEN f[i] ELSE idMap[i]]
        /\ works' = works \cup {f[i] : i \in DOMAIN f}
        /\ edges' = edges \cup {<<idMap'[e[1]], idMap'[e[2]]>> : e \in PageEdges(pg)}
        /\ decision' = [w \in Works |->
                          IF w \in {Find(idMap'[e[2]]) : e \in PageEdges(pg)}
                             /\ decision[w] = NoState
                            THEN "candidate" ELSE decision[w]]
    /\ rawEdges' = rawEdges \cup PageEdges(pg)
    /\ pc' = [pc EXCEPT ![p] = "idle"]
    /\ page' = [page EXCEPT ![p] = 0]
    /\ snapshot' = [snapshot EXCEPT ![p] = [i \in IDs |-> NoWork]]
    /\ UNCHANGED <<merges, reviewLog, cursor, crashes, undos, known, replayOK,
                   mseq, decidedAt, post, undoCarryOK, undoRestoreOK>>

\* DiscoverEqual + Merge in one transaction: a fetch reports the same
\* strong ID for two works. Roots are computed inside the transaction and
\* the merge row links two distinct roots. Differing states make the
\* winner needs_decision and both prior states go to review_log. Only the
\* winner's row is written; the loser's row is kept.
Merge ==
    \E pair \in EqualPairs : \E x, y \in pair :
        /\ x # y
        /\ idMap[x] # NoWork /\ idMap[y] # NoWork
        /\ LET w  == Find(idMap[x])
               l  == Find(idMap[y])
               st == Combine(decision[w], decision[l])
           IN
           /\ w # l
           /\ merges' = merges \cup {<<w, l, mseq + 1, decision[w], decision[l]>>}
           /\ mseq' = mseq + 1
           /\ decision' = [decision EXCEPT ![w] = st]
           /\ reviewLog' = IF st = "needs_decision" /\ decision[w] # decision[l]
                             THEN reviewLog \cup {<<w, decision[w]>>,
                                                  <<l, decision[l]>>}
                             ELSE reviewLog
    /\ UNCHANGED <<idMap, works, edges, cursor, pc, page, snapshot, crashes, undos,
                   rawEdges, known, replayOK, decidedAt, post, undoCarryOK,
                   undoRestoreOK>>

\* Set undone_at on an active merge row. Only the latest active merge with
\* that winner may be undone (LIFO per winner), so the restored prior
\* state is not missing a later merge's contribution.
\* If the class was decided after the merge (decided_at >= merge id), both
\* works keep that decision: Decide wrote every row in the class, so both
\* rows already hold it. Otherwise each work gets its own prior state.
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
        /\ Bug = "undo_not_lifo" \/ ~\E m2 \in merges : m2[1] = w /\ m2[3] > m[3]
        /\ merges' = merges \ {m}
        /\ decision' = [decision EXCEPT ![w] = newW, ![l] = newL]
        /\ undoCarryOK' = (undoCarryOK /\
                              (postSt # NoState => newW = postSt /\ newL = postSt))
        /\ undoRestoreOK' = (undoRestoreOK /\
                                (postSt = NoState => newW = m[4] /\ newL = m[5]))
        /\ post' = {e \in post : e[1] # m[3]}
    /\ undos' = undos + 1
    /\ UNCHANGED <<idMap, works, edges, reviewLog, cursor, pc, page, snapshot,
                   crashes, rawEdges, known, replayOK, mseq, decidedAt>>

\* review decide on a resolved (root) row writes every row in its class
\* and stamps decided_at, so the decision survives a later undo.
\* Re-deciding an already included/excluded work is left out to bound the
\* state space; it is the same transition as deciding a needs_decision row.
Decide ==
    \E w \in works, s \in {"included", "excluded"} :
        LET cls  == Class(w)
            mids == {m[3] : m \in {m2 \in merges : m2[1] \in cls}}
        IN
        /\ IsRoot(w)
        /\ decision[w] \in {"candidate", "needs_decision"}
        /\ decision' = [v \in Works |-> IF v \in cls THEN s ELSE decision[v]]
        /\ decidedAt' = [v \in Works |-> IF v \in cls THEN mseq ELSE decidedAt[v]]
        /\ post' = {e \in post : e[1] \notin mids} \cup {<<i, s>> : i \in mids}
        /\ known' = {k \in known : Find(k[1]) # w} \cup {<<v, s>> : v \in cls}
        /\ UNCHANGED <<idMap, works, merges, edges, reviewLog, cursor, pc,
                       page, snapshot, crashes, undos, rawEdges, replayOK, mseq,
                       undoCarryOK, undoRestoreOK>>

\* A worker dies; uncommitted work is lost, committed rows stay.
Crash(p) ==
    /\ crashes < MaxCrash
    /\ pc[p] # "down"
    /\ pc' = [pc EXCEPT ![p] = "down"]
    /\ page' = [page EXCEPT ![p] = 0]
    /\ snapshot' = [snapshot EXCEPT ![p] = [i \in IDs |-> NoWork]]
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<idMap, works, merges, edges, decision, reviewLog, cursor,
                   undos, rawEdges, known, replayOK,
                   mseq, decidedAt, post, undoCarryOK, undoRestoreOK>>

\* Restart resumes from the committed cursor (Resolve reads it).
Restart(p) ==
    /\ pc[p] = "down"
    /\ pc' = [pc EXCEPT ![p] = "idle"]
    /\ UNCHANGED <<idMap, works, merges, edges, decision, reviewLog, cursor,
                   page, snapshot, crashes, undos, rawEdges, known, replayOK,
                   mseq, decidedAt, post, undoCarryOK, undoRestoreOK>>

Next ==
    \/ \E p \in Workers : Resolve(p) \/ FetchPage(p) \/ CommitPage(p)
                          \/ CommitCursor(p) \/ CommitEdges(p) \/ Crash(p) \/ Restart(p)
    \/ Merge
    \/ UndoMerge
    \/ Decide

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Invariants *)

TypeOK ==
    /\ idMap \in [IDs -> Works \cup {NoWork}]
    /\ works \subseteq Works
    /\ \A m \in merges : m[1] \in works /\ m[2] \in works
    /\ edges \subseteq works \X works
    /\ decision \in [Works -> States \cup {NoState}]
    /\ cursor \in 0..NPages
    /\ pc \in [Workers -> {"idle", "resolved", "fetched", "half", "down"}]
    /\ page \in [Workers -> 0..NPages]
    /\ crashes \in 0..MaxCrash
    /\ undos \in 0..MaxUndo

\* Union-find forest: one active parent per work, no cycles, no self-merge.
SingleParent == \A m1, m2 \in merges : m1[2] = m2[2] => m1 = m2
NoSelfMerge  == \A m \in merges : m[1] # m[2]
Acyclic ==
    \A w \in works : \A k \in 1..Cardinality(Works) : Up(w, k) = w => IsRoot(w)

\* Every ID and every work resolves to a root.
ResolvesToRoot ==
    /\ \A i \in IDs : idMap[i] # NoWork => IsRoot(Find(idMap[i]))
    /\ \A w \in works : IsRoot(Find(w))

\* Read view: endpoints are roots, no resolved self-loops.
ReadEdgesRootsNoSelfLoop ==
    \A e \in ReadEdges : IsRoot(e[1]) /\ IsRoot(e[2]) /\ e[1] # e[2]

\* The careful form of "no edge endpoint resolves to a non-root after
\* commit": the read view equals the committed raw pairs resolved through
\* work_ids and active merges, under any sequence of merges and undos.
EdgeFaithful == ReadEdges = Faithful

\* Every allocated work is reachable from some external ID (no orphan
\* work minted by a lost race).
NoOrphanWork == \A w \in works : \E i \in IDs : idMap[i] = w

\* No decision is lost by a merge: each live decision is either the
\* resolved state, or the resolved row is needs_decision and review_log
\* holds that state for some work now in the same class. (Merge logs the
\* two roots' states, so a decision carried by an earlier conflict-free
\* merge is logged under the root that carried it, not the original row.)
NoDecisionLost ==
    \A k \in known :
        LET r == Find(k[1]) IN
        \/ decision[r] = k[2]
        \/ /\ decision[r] = "needs_decision"
           /\ \E l \in reviewLog : l[2] = k[2] /\ Find(l[1]) = r

\* A committed cursor implies its pages' edges are present.
CursorImpliesEdges ==
    \A pg \in 1..cursor : \A e \in PageEdges(pg) :
        /\ idMap[e[1]] # NoWork /\ idMap[e[2]] # NoWork
        /\ <<idMap[e[1]], idMap[e[2]]>> \in edges

\* Replaying a committed page changes nothing.
ReplayIdempotent == replayOK

\* Undo after a post-merge decision: both works hold that decision.
UndoCarriesDecision == undoCarryOK

\* Undo with no post-merge decision: each work gets its own prior state.
UndoRestoresPrior == undoRestoreOK

-----------------------------------------------------------------------------
(* Witnesses: each must be violated, showing the scenario is reachable. *)

NeverNeedsDecision == \A w \in Works : decision[w] # "needs_decision"
NeverResolvedSelfLoop == \A e \in Resolved : e[1] # e[2]
\* A seed merged with a candidate becomes needs_decision.
NeverSeedCandidateConflict ==
    ~\E w \in works :
        /\ decision[w] = "needs_decision"
        /\ \E a, b \in reviewLog :
              /\ a[2] = "seed" /\ b[2] = "candidate"
              /\ Find(a[1]) = w /\ Find(b[1]) = w
Sym == Permutations(Works)
=============================================================================
