--------------------------- MODULE MergeFetchDecide ---------------------------
(***************************************************************************)
(* Identity merges interleaved with in-flight citation fetches, fetch     *)
(* generations and review decisions, for the paperbridge citation store   *)
(* (crates/citegraph). Design: docs/design/citation-graph.md. Obligations *)
(* on the Rust store: formal/tla/README.md.                               *)
(*                                                                         *)
(* Store semantics follow crates/citegraph (PR #70):                      *)
(*  - every external ID gets its own base work; work ids are allocated    *)
(*    in order (1, 2, ...);                                               *)
(*  - IDs reported together are joined by logged merge rows that record   *)
(*    the evidence base pair; a class is a connected component over the   *)
(*    evidence pairs of active rows; its root is the smallest work id;    *)
(*  - every store write is one atomic action (one BEGIN IMMEDIATE tx).    *)
(*                                                                         *)
(* Budgets (CONSTANTS) pick the mechanisms a config explores; Bug selects *)
(* a seeded defect that TLC must reject (see README.md and run.sh).       *)
(*                                                                         *)
(* Measured with TLC 2.19 (tla2tools v1.7.4), Java 25, M4 Max, 10 workers *)
(*   MergeFetchDecide.cfg  903,600 distinct, depth 27, 14 s               *)
(*   _Gen 1,029,960 / 31 / 16 s; _Split 4,854,312 / 30 / 94 s;            *)
(*   _Undo2 6,663,624 / 30 / 99 s. All configs: README.md, "Results".    *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets, TLC

CONSTANTS
    NW,          \* work ids 1..NW
    Workers,     \* concurrent writers sharing the store
    A, A2, B, C, \* external IDs; A is the seed
    Arxiv,       \* IDs that are arXiv IDs (the rest are DOIs or other IDs)
    NPages,      \* pages per generation
    MaxCrash,    \* crash budget
    MaxUndo,     \* user undo budget
    MaxSplit,    \* version_split budget
    NGen,        \* fetch generations (1 = no refresh)
    VersionVeto, \* join refuses a class mixing arXiv IDs and DOIs
    Bug          \* seeded defect, "none" for the design

Works   == 1..NW
NoWork  == 0
IDs     == {A, A2, B, C}
Doi     == IDs \ Arxiv

\* Raw (citing, cited) pairs per generation and page. Page 2 of generation
\* 1 is a citing work: only cited works get a candidate row, so C enters
\* without review state. Generation 2 no longer returns C -> A. Pages
\* past 2 are empty.
PageEdges(g, p) ==
    IF p = 1 THEN {<<A, B>>}
    ELSE IF p = 2 /\ g = 1 THEN {<<C, A>>} ELSE {}
PageIDs(g, p) == UNION {{e[1], e[2]} : e \in PageEdges(g, p)}

\* Records a fetch may return: IDs one source reports for one paper.
\* {A, A2} then {A2, C} makes C the seed, so C -> A becomes a resolved
\* self-loop. {B, C} joins an arXiv ID with a DOI (a version pair).
Records == {{B, C}, {A2, C}, {A, A2}}

States  == {"seed", "candidate", "included", "excluded", "needs_decision"}
NoState == "none"
SOpt    == States \cup {NoState}
\* <= NW-1 active rows; each undo or split frees one slot for a new row.
MaxSeq  == (NW - 1) + MaxUndo + MaxSplit

VARIABLES
    \* store
    idMap,     \* work_ids: external ID -> base work (NoWork if absent)
    nworks,    \* works allocated: 1..nworks
    merges,    \* active merge_log rows (undone_at IS NULL), records
               \* [id, w, l, ev, pw, pl, why]: winner/loser roots, evidence
               \* bases, winner/loser prior states (review_log), and the
               \* ghost true evidence pair why
    root,      \* class root per work, derived from merges (cached)
    vetoed,    \* evidence pairs whose latest row was undone by a user
    undone,    \* user-undone rows (for redo)
    edgeSeen,  \* edge_sources: <<citing base, cited base>> -> last_seen gen
    decision,  \* review_works.state per work row (one review)
    reviewLog, \* review_log <<merge id, work, prior state>>
    cursor,    \* fetch_log: generation -> pages committed
    curGen,    \* newest generation started
    mseq,      \* merge_log.seq of the last row (logical clock)
    decidedAt, \* review_works.decided_seq per row
    \* workers
    pc,        \* idle | resolved | fetched | half | down
    page,      \* page in flight (0 = none)
    pgen,      \* generation of the page in flight
    snapshot,  \* ID -> root resolved before the await (snapshot bugs only)
    budget,    \* [crash, undo, split] used
    \* ghosts
    rawEdges,  \* <<gen, raw pair>> committed
    evid,      \* <<ID, base>> each ID mapping came from
    known,     \* <<work, state>> decisions not superseded by a later Decide
    everUndone,\* evidence pairs ever user-undone
    post,      \* <<merge id, state>> decided on the class after that merge
    ok,        \* transition-check flags
    seen       \* reachability markers for witnesses

store == <<idMap, nworks, merges, root, vetoed, undone, edgeSeen, decision,
           reviewLog, cursor, curGen, mseq, decidedAt>>
local == <<pc, page, pgen, snapshot, budget>>
ghost == <<rawEdges, evid, known, everUndone, post, ok, seen>>
vars  == <<store, local, ghost>>

-----------------------------------------------------------------------------
(* Classes: components over active evidence pairs; root = smallest id. *)

works  == 1..nworks
Min(S) == CHOOSE x \in S : \A y \in S : x <= y
Max(S) == CHOOSE x \in S : \A y \in S : x >= y
RECURSIVE Grow(_, _, _)
Grow(ms, S, k) ==
    LET S2 == S \cup UNION {m.ev : m \in {m2 \in ms : m2.ev \cap S # {}}}
    IN IF k = 0 \/ S2 = S THEN S ELSE Grow(ms, S2, k - 1)
\* The store recomputes classes from the active rows in each transaction;
\* the model caches the result in root.
RootsOf(ms) == [w \in Works |-> Min(Grow(ms, {w}, NW))]
Find(w)     == root[w]
IsRoot(w)   == Find(w) = w
Class(r)    == {v \in works : Find(v) = r}
Resolve1(i) == IF idMap[i] = NoWork THEN NoWork ELSE Find(idMap[i])
ActiveIds   == {m.id : m \in merges}

\* Undo guards. Design (PR #70): the latest active row of its class.
\* IsRoot guard: winner is a current root and no later row has the same
\* winner.
Outermost(m) == \A m2 \in merges : Find(m2.w) = Find(m.w) => m2.id <= m.id
RootGuard(m) == IsRoot(m.w) /\ ~\E m2 \in merges : m2.w = m.w /\ m2.id > m.id

ClassIDs(r) == {i \in IDs : idMap[i] # NoWork /\ Find(idMap[i]) = r}
Mixed(ids)  == ids \cap Arxiv # {} /\ ids \cap Doi # {}

\* Read path: resolve stored edges and drop resolved self-loops.
edges     == DOMAIN edgeSeen
Resolved  == {<<Find(e[1]), Find(e[2])>> : e \in edges}
ReadEdges == {e \in Resolved : e[1] # e[2]}
Faithful  == {e \in {<<Resolve1(r[2][1]), Resolve1(r[2][2])>> : r \in rawEdges} :
                 e[1] # e[2]}

Complete(g)    == cursor[g] = NPages
NewestComplete == IF \E g \in 1..curGen : Complete(g)
                    THEN Max({g \in 1..curGen : Complete(g)}) ELSE 0

\* Review state after a merge. Any two different states conflict,
\* including seed vs candidate: the caller resolves every conflict.
Combine(sw, sl) ==
    IF sl = NoState THEN sw
    ELSE IF sw = NoState THEN sl
    ELSE IF sw = sl THEN sw
    ELSE IF Bug = "merge_overwrite" THEN sw
    ELSE "needs_decision"

SnapBugs == {"snap_root", "snap_no_readback", "snap_orphan"}

\* A new row joining evidence bases a and b (true evidence t).
NewRow(a, b, t) ==
    LET ra == Find(a)
        rb == Find(b)
        w  == IF ra < rb THEN ra ELSE rb
        l  == IF ra < rb THEN rb ELSE ra
    IN [id |-> mseq + 1, w |-> w, l |-> l, ev |-> {a, b},
        pw |-> decision[w], pl |-> decision[l], why |-> t]

\* Store effects of inserting row r.
AddRow(r) ==
    LET st == Combine(r.pw, r.pl) IN
    /\ merges' = merges \cup {r}
    /\ root' = RootsOf(merges')
    /\ mseq' = r.id
    /\ decision' = [decision EXCEPT ![r.w] = st]
    /\ reviewLog' = IF st = "needs_decision" /\ r.pw # r.pl
                      THEN reviewLog \cup {<<r.id, r.w, r.pw>>, <<r.id, r.l, r.pl>>}
                      ELSE reviewLog

\* Review rows after removing active row m. If the class was decided
\* after m (decided_seq >= m.id), both works keep the decision (Decide
\* wrote every class row); otherwise each gets its prior state.
Restore(m) ==
    LET decided == decidedAt[m.w] >= m.id
        newW == IF decided THEN decision[m.w] ELSE m.pw
        newL == IF Bug = "undo_restores_loser" THEN m.pl ELSE decision[m.l]
        postSt == IF \E e \in post : e[1] = m.id
                    THEN (CHOOSE e \in post : e[1] = m.id)[2] ELSE NoState
    IN
    /\ decision' = [decision EXCEPT ![m.w] = newW, ![m.l] = newL]
    /\ post' = {e \in post : e[1] # m.id}
    /\ ok' = [ok EXCEPT
                !.carry = @ /\ (postSt # NoState => newW = postSt /\ newL = postSt),
                !.restore = @ /\ (postSt = NoState => newW = m.pw /\ newL = m.pl)]
    /\ seen' = seen \cup (IF postSt # NoState THEN {"undo_carried"} ELSE {})
                    \cup (IF Outermost(m) THEN {} ELSE {"inner_removed"})

-----------------------------------------------------------------------------
Init ==
    /\ idMap = [i \in IDs |-> IF i = A THEN 1 ELSE NoWork]
    /\ nworks = 1
    /\ merges = {} /\ vetoed = {} /\ undone = {}
    /\ root = [w \in Works |-> w]
    /\ edgeSeen = [e \in {} |-> 0]
    /\ decision = [w \in Works |-> IF w = 1 THEN "seed" ELSE NoState]
    /\ reviewLog = {}
    /\ cursor = [g \in 1..NGen |-> 0]
    /\ curGen = 1
    /\ mseq = 0
    /\ decidedAt = [w \in Works |-> 0]
    /\ pc = [p \in Workers |-> "idle"]
    /\ page = [p \in Workers |-> 0]
    /\ pgen = [p \in Workers |-> 1]
    /\ snapshot = [p \in Workers |-> [i \in IDs |-> NoWork]]
    /\ budget = [crash |-> 0, undo |-> 0, split |-> 0]
    /\ rawEdges = {}
    /\ evid = {<<A, 1>>}
    /\ known = {<<1, "seed">>}
    /\ everUndone = {} /\ post = {}
    /\ ok = [replay |-> TRUE, cursor |-> TRUE, carry |-> TRUE,
             restore |-> TRUE, veto |-> TRUE, gen |-> TRUE]
    /\ seen = {}

\* begin_fetch --refresh: start a new generation.
Refresh ==
    /\ curGen < NGen
    /\ curGen' = curGen + 1
    /\ UNCHANGED <<idMap, nworks, merges, root, vetoed, undone, edgeSeen, decision,
                   reviewLog, cursor, mseq, decidedAt, local, ghost>>

\* Read the latest generation and its cursor and pick a page; a worker may
\* pick an already committed page (stale cursor read, retry).
Resolve(p) ==
    /\ pc[p] = "idle"
    /\ \E pg \in 1..(IF cursor[curGen] < NPages THEN cursor[curGen] + 1 ELSE NPages) :
        /\ page' = [page EXCEPT ![p] = pg]
        /\ snapshot' = IF Bug \in SnapBugs
                         THEN [snapshot EXCEPT ![p] =
                                [i \in IDs |-> IF i \in PageIDs(curGen, pg)
                                                 THEN Resolve1(i) ELSE NoWork]]
                         ELSE snapshot
    /\ pgen' = [pgen EXCEPT ![p] = curGen]
    /\ pc' = [pc EXCEPT ![p] = "resolved"]
    /\ UNCHANGED <<store, budget, ghost>>

\* The HTTP await; no transaction is held.
FetchPage(p) ==
    /\ pc[p] = "resolved"
    /\ pc' = [pc EXCEPT ![p] = "fetched"]
    /\ UNCHANGED <<store, page, pgen, snapshot, budget, ghost>>

\* commit_page, one transaction. A finished generation, or one older than
\* the newest complete generation, writes nothing. Otherwise: mint a base
\* work per unknown ID (ON CONFLICT DO NOTHING, read back), upsert edges by
\* base work with last_seen = MAX(last_seen, generation), mark cited works
\* with no review row candidate, cursor = max(cursor, page).
CommitPage(p) ==
    LET g    == pgen[p]
        pg   == page[p]
        ids  == PageIDs(g, pg)
        snap == snapshot[p]
        superseded == Complete(g) \/ NewestComplete > g
        new  == IF Bug \in {"snap_no_readback", "snap_orphan"}
                  THEN {i \in ids : snap[i] = NoWork}
                  ELSE {i \in ids : idMap[i] = NoWork}
    IN
    /\ pc[p] = "fetched"
    /\ Bug # "split_commit"
    /\ pc' = [pc EXCEPT ![p] = "idle"]
    /\ page' = [page EXCEPT ![p] = 0]
    /\ snapshot' = [snapshot EXCEPT ![p] = [i \in IDs |-> NoWork]]
    /\ UNCHANGED <<pgen, budget>>
    /\ IF superseded /\ Bug # "gen_no_guard"
       THEN /\ seen' = seen \cup {"superseded"}
            /\ UNCHANGED <<store, rawEdges, evid, known, everUndone, post, ok>>
       ELSE
         \E f \in [new -> Works] :
         LET inserted == {i \in new : idMap[i] = NoWork}
             idMap2 == [i \in IDs |-> IF i \in inserted THEN f[i] ELSE idMap[i]]
             base(i) ==
                 CASE Bug = "snap_no_readback" /\ i \in new -> f[i]
                   \* the pre-await resolution, only where it went stale
                   [] Bug = "snap_root" /\ snap[i] # NoWork
                        /\ snap[i] # Find(idMap2[i]) -> snap[i]
                   [] OTHER -> idMap2[i]
             store1(i) == IF Bug = "store_root" THEN Find(base(i)) ELSE base(i)
             newE == {<<store1(e[1]), store1(e[2])>> : e \in PageEdges(g, pg)}
             stamp(e) == IF e \in edges
                           THEN IF Bug = "stamp_overwrite" THEN g
                                ELSE IF edgeSeen[e] > g THEN edgeSeen[e] ELSE g
                           ELSE g
             seen2 == [e \in edges \cup newE |->
                         IF e \in newE THEN stamp(e) ELSE edgeSeen[e]]
             cited == {Find(base(e[2])) : e \in PageEdges(g, pg)}
             decision2 == [w \in Works |->
                             IF w \in cited /\ (decision[w] = NoState
                                                \/ Bug = "cand_unconditional")
                               THEN "candidate" ELSE decision[w]]
             cursor2 == IF Bug = "cursor_regress" THEN pg
                        ELSE IF pg > cursor[g] THEN pg ELSE cursor[g]
             replay == pg <= cursor[g]
         IN
         /\ \A i, j \in new : i # j => f[i] # f[j]
         /\ {f[i] : i \in new} = nworks + 1 .. nworks + Cardinality(new)
         /\ idMap' = idMap2
         /\ nworks' = nworks + Cardinality(new)
         /\ edgeSeen' = seen2
         /\ decision' = decision2
         /\ cursor' = [cursor EXCEPT ![g] = cursor2]
         /\ rawEdges' = rawEdges \cup {<<g, e>> : e \in PageEdges(g, pg)}
         /\ evid' = evid \cup {<<i, f[i]>> : i \in inserted}
         /\ ok' = [ok EXCEPT
                     !.replay = @ /\ (replay => /\ idMap2 = idMap
                                                /\ new = {}
                                                /\ seen2 = edgeSeen
                                                \* a replay may only add a
                                                \* candidate row to a root
                                                \* split off by an undo
                                                /\ \A w \in Works :
                                                     decision2[w] # decision[w]
                                                     => decision[w] = NoState),
                     !.cursor = @ /\ cursor2 >= cursor[g],
                     !.gen = @ /\ (superseded => seen2 = edgeSeen /\ new = {})]
         /\ seen' = seen \cup (IF replay THEN {"replay"} ELSE {})
         /\ UNCHANGED <<merges, root, vetoed, undone, reviewLog, curGen, mseq,
                        decidedAt, known, everUndone, post>>

\* split_commit bug: cursor in one transaction ...
CommitCursor(p) ==
    /\ Bug = "split_commit"
    /\ pc[p] = "fetched"
    /\ cursor' = [cursor EXCEPT ![pgen[p]] = IF page[p] > @ THEN page[p] ELSE @]
    /\ pc' = [pc EXCEPT ![p] = "half"]
    /\ UNCHANGED <<idMap, nworks, merges, root, vetoed, undone, edgeSeen, decision,
                   reviewLog, curGen, mseq, decidedAt, page, pgen, snapshot,
                   budget, ghost>>

\* ... and edges in another.
CommitEdges(p) ==
    LET g   == pgen[p]
        pg  == page[p]
        new == {i \in PageIDs(g, pg) : idMap[i] = NoWork}
    IN
    /\ Bug = "split_commit"
    /\ pc[p] = "half"
    /\ \E f \in [new -> Works] :
        /\ \A i, j \in new : i # j => f[i] # f[j]
        /\ {f[i] : i \in new} = nworks + 1 .. nworks + Cardinality(new)
        /\ idMap' = [i \in IDs |-> IF i \in new THEN f[i] ELSE idMap[i]]
        /\ nworks' = nworks + Cardinality(new)
        /\ evid' = evid \cup {<<i, f[i]>> : i \in new}
        /\ LET newE == {<<idMap'[e[1]], idMap'[e[2]]>> : e \in PageEdges(g, pg)}
           IN edgeSeen' = [e \in edges \cup newE |->
                             IF e \in newE THEN g ELSE edgeSeen[e]]
        /\ decision' = [w \in Works |->
                          IF w \in {Find(idMap'[e[2]]) : e \in PageEdges(g, pg)}
                             /\ decision[w] = NoState
                            THEN "candidate" ELSE decision[w]]
    /\ rawEdges' = rawEdges \cup {<<g, e>> : e \in PageEdges(g, pg)}
    /\ pc' = [pc EXCEPT ![p] = "idle"]
    /\ page' = [page EXCEPT ![p] = 0]
    /\ snapshot' = [snapshot EXCEPT ![p] = [i \in IDs |-> NoWork]]
    /\ UNCHANGED <<merges, root, vetoed, undone, reviewLog, cursor, curGen, mseq,
                   decidedAt, pgen, budget, known, everUndone, post, ok, seen>>

\* observe, one transaction: a record reports two known IDs in different
\* classes; join them with a row recording the evidence bases unless a
\* veto applies (sticky user undo of this pair; mixed versions).
Merge ==
    \E r \in Records : \E x, y \in r :
        /\ x # y
        /\ idMap[x] # NoWork /\ idMap[y] # NoWork
        /\ LET bx == idMap[x]
               by == idMap[y]
               ev == {bx, by}
               lo == IF bx < by THEN bx ELSE by
               hi == IF bx < by THEN by ELSE bx
               \* merge_no_find: winner, loser and class check use bases
               row == IF Bug = "merge_no_find"
                        THEN [NewRow(bx, by, ev) EXCEPT !.w = lo, !.l = hi,
                                 !.pw = decision[lo], !.pl = decision[hi]]
                        ELSE NewRow(bx, by, ev)
           IN
           /\ IF Bug = "merge_no_find" THEN bx # by ELSE Find(bx) # Find(by)
           /\ Bug = "veto_ignored" \/ ev \notin vetoed
           /\ ~(VersionVeto /\ Mixed(ClassIDs(Find(bx)) \cup ClassIDs(Find(by))))
           /\ AddRow(row)
           /\ seen' = seen \cup (IF ev # {row.w, row.l} THEN {"dependent_merge"} ELSE {})
    /\ UNCHANGED <<idMap, nworks, vetoed, undone, edgeSeen, cursor, curGen,
                   decidedAt, local, rawEdges, evid, known, everUndone, post, ok>>

\* observe: a record reports a known ID x with a new ID y. y gets its own
\* base work, joined to x's base by a logged row (unless the version veto
\* refuses the join; y keeps its own work then).
Attach ==
    \E r \in Records : \E x, y \in r :
        /\ x # y
        /\ idMap[x] # NoWork /\ idMap[y] = NoWork
        /\ nworks < NW
        /\ LET n  == nworks + 1
               bx == idMap[x]
               rx == Find(bx)
               ea == IF Bug = "attach_root" THEN rx ELSE bx
               row == [id |-> mseq + 1, w |-> rx, l |-> n, ev |-> {ea, n},
                       pw |-> decision[rx], pl |-> NoState, why |-> {bx, n}]
               vetoedJoin == VersionVeto /\ Mixed(ClassIDs(rx) \cup {y})
           IN
           /\ idMap' = [idMap EXCEPT ![y] = n]
           /\ nworks' = n
           /\ evid' = evid \cup {<<y, n>>}
           /\ IF vetoedJoin
                THEN UNCHANGED <<merges, root, mseq, decision, reviewLog, seen>>
                ELSE /\ AddRow(row)
                     /\ seen' = seen \cup (IF IsRoot(bx) THEN {} ELSE {"attach_nonroot"})
    /\ UNCHANGED <<vetoed, undone, edgeSeen, cursor, curGen, decidedAt, local,
                   rawEdges, known, everUndone, post, ok>>

\* undo_merge, one transaction: only the outermost row of a class (the
\* latest active row in it). Sticky: the evidence pair is vetoed.
UndoMerge ==
    /\ budget.undo < MaxUndo
    /\ \E m \in merges :
        /\ CASE Bug = "undo_inner"    -> ~\E m2 \in merges : m2.w = m.w /\ m2.id > m.id
             [] Bug = "undo_not_lifo" -> IsRoot(m.w)
             [] OTHER                 -> Outermost(m)
        /\ merges' = merges \ {m}
        /\ root' = RootsOf(merges')
        /\ Restore(m)
        /\ vetoed' = vetoed \cup {IF Bug = "veto_roots" THEN {m.w, m.l} ELSE m.ev}
        /\ undone' = undone \cup {m}
        /\ everUndone' = everUndone \cup {m.ev}
    /\ budget' = [budget EXCEPT !.undo = @ + 1]
    /\ UNCHANGED <<idMap, nworks, edgeSeen, reviewLog, cursor, curGen, mseq,
                   decidedAt, pc, page, pgen, snapshot, rawEdges, evid, known>>

\* redo_merge: re-apply an undone row as a NEW row; this lifts the veto.
Redo ==
    \E m \in undone :
        /\ m.ev \in vetoed
        /\ \E a, b \in m.ev : Find(a) # Find(b)
        /\ ~(VersionVeto /\ Mixed(UNION {ClassIDs(Find(a)) : a \in m.ev}))
        /\ LET a == CHOOSE a \in m.ev : TRUE
               b == CHOOSE b \in m.ev : b # a
               fresh == NewRow(a, b, m.why)
               \* redo_old_row: reactivate the old row (old id and priors)
               row == IF Bug = "redo_old_row" THEN [fresh EXCEPT !.id = m.id] ELSE fresh
               st == Combine(row.pw, row.pl)
           IN
           /\ merges' = merges \cup {row}
           /\ root' = RootsOf(merges')
           /\ mseq' = IF Bug = "redo_old_row" THEN mseq ELSE row.id
           /\ decision' = [decision EXCEPT ![row.w] = st]
           /\ reviewLog' = IF st = "needs_decision" /\ row.pw # row.pl
                             THEN reviewLog \cup {<<row.id, row.w, row.pw>>,
                                                  <<row.id, row.l, row.pl>>}
                             ELSE reviewLog
        /\ vetoed' = vetoed \ {m.ev}
        /\ undone' = undone \ {m}
        /\ seen' = seen \cup {"redo"}
    /\ UNCHANGED <<idMap, nworks, edgeSeen, cursor, curGen, decidedAt, local,
                   rawEdges, evid, known, everUndone, post, ok>>

\* A record whose evidence pair is vetoed is skipped; the check is on the
\* evidence base pair, never on roots.
SkipVetoed ==
    \E r \in Records : \E x, y \in r :
        /\ x # y
        /\ idMap[x] # NoWork /\ idMap[y] # NoWork
        /\ Find(idMap[x]) # Find(idMap[y])
        /\ {idMap[x], idMap[y]} \in vetoed
        /\ ok' = [ok EXCEPT !.veto = @ /\ {idMap[x], idMap[y]} \in everUndone]
        /\ UNCHANGED <<store, local, rawEdges, evid, known, everUndone, post, seen>>

\* version_split: a folded record shows the class holds both versions
\* (arXiv ID B, DOI C) and an active row joins their bases directly.
\* Design (safe rule): undo it (not sticky) only if it is the outermost
\* row of the class; otherwise change nothing and report the split as
\* pending (a later record retries once the outer rows are undone).
\* Bug "split_bypass" (PR #70 as written) undoes it regardless.
\* Bug "split_mark_nd" marks the class root needs_decision instead.
VersionSplit ==
    /\ budget.split < MaxSplit
    /\ idMap[B] # NoWork /\ idMap[C] # NoWork
    /\ \E m \in merges :
        /\ m.ev = {idMap[B], idMap[C]}
        /\ budget' = [budget EXCEPT !.split = @ + 1]
        /\ IF Outermost(m) \/ Bug = "split_bypass"
           THEN /\ merges' = merges \ {m}
                /\ root' = RootsOf(merges')
                /\ Restore(m)
                /\ UNCHANGED reviewLog
           ELSE LET r == Find(m.w) IN
                /\ IF Bug = "split_mark_nd"
                     THEN /\ decision' = [decision EXCEPT ![r] = "needs_decision"]
                          /\ reviewLog' = IF decision[r] = "needs_decision"
                                            THEN reviewLog
                                            ELSE reviewLog \cup {<<m.id, r, decision[r]>>}
                     ELSE UNCHANGED <<decision, reviewLog>>
                /\ seen' = seen \cup {"split_blocked"}
                /\ UNCHANGED <<merges, root, post, ok>>
    /\ UNCHANGED <<idMap, nworks, vetoed, undone, edgeSeen, cursor, curGen, mseq,
                   decidedAt, pc, page, pgen, snapshot, rawEdges, evid, known,
                   everUndone>>

\* review decide on a root row: writes every row in its class and stamps
\* decided_seq. Re-deciding included/excluded is left out (bound).
Decide ==
    \E w \in works, s \in {"included", "excluded"} :
        LET cls  == Class(w)
            mids == {m.id : m \in {m2 \in merges : Find(m2.w) = w}}
            rows == IF Bug = "decide_root_only" THEN {w} ELSE cls
        IN
        /\ IsRoot(w)
        /\ decision[w] \in {"candidate", "needs_decision"}
        /\ decision' = [v \in Works |-> IF v \in rows THEN s ELSE decision[v]]
        /\ decidedAt' = [v \in Works |-> IF v \in cls THEN mseq ELSE decidedAt[v]]
        /\ post' = {e \in post : e[1] \notin mids} \cup {<<i, s>> : i \in mids}
        /\ known' = {k \in known : Find(k[1]) # w} \cup {<<v, s>> : v \in cls}
        /\ UNCHANGED <<idMap, nworks, merges, root, vetoed, undone, edgeSeen, reviewLog,
                       cursor, curGen, mseq, local, rawEdges, evid, everUndone,
                       ok, seen>>

Crash(p) ==
    /\ budget.crash < MaxCrash
    /\ pc[p] # "down"
    /\ pc' = [pc EXCEPT ![p] = "down"]
    /\ page' = [page EXCEPT ![p] = 0]
    /\ snapshot' = [snapshot EXCEPT ![p] = [i \in IDs |-> NoWork]]
    /\ budget' = [budget EXCEPT !.crash = @ + 1]
    /\ UNCHANGED <<store, pgen, ghost>>

Restart(p) ==
    /\ pc[p] = "down"
    /\ pc' = [pc EXCEPT ![p] = "idle"]
    /\ UNCHANGED <<store, page, pgen, snapshot, budget, ghost>>

Next ==
    \/ \E p \in Workers : \/ Resolve(p) \/ FetchPage(p) \/ CommitPage(p)
                          \/ CommitCursor(p) \/ CommitEdges(p)
                          \/ Crash(p) \/ Restart(p)
    \/ Merge \/ Attach \/ UndoMerge \/ Redo \/ SkipVetoed \/ VersionSplit
    \/ Decide \/ Refresh

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Invariants *)

RowOK(m) ==
    /\ m.id \in 1..MaxSeq
    /\ m.w \in works /\ m.l \in works
    /\ m.ev \subseteq works /\ Cardinality(m.ev) = 2
    /\ m.why \subseteq works /\ Cardinality(m.why) = 2
    /\ m.pw \in SOpt /\ m.pl \in SOpt

TypeOK ==
    /\ idMap \in [IDs -> 0..NW]
    /\ nworks \in 1..NW
    /\ \A m \in merges \cup undone : RowOK(m)
    /\ vetoed \subseteq SUBSET works
    /\ edges \subseteq works \X works
    /\ \A e \in edges : edgeSeen[e] \in 1..NGen
    /\ decision \in [Works -> SOpt]
    /\ reviewLog \subseteq (1..MaxSeq) \X works \X SOpt
    /\ cursor \in [1..NGen -> 0..NPages]
    /\ curGen \in 1..NGen
    /\ mseq \in 0..MaxSeq
    /\ decidedAt \in [Works -> 0..MaxSeq]
    /\ pc \in [Workers -> {"idle", "resolved", "fetched", "half", "down"}]
    /\ page \in [Workers -> 0..NPages]
    /\ post \subseteq (1..MaxSeq) \X {"included", "excluded"}

\* Active rows form a forest (UnionFind::load never sees a cycle).
Forest      == Cardinality(merges) = nworks - Cardinality({Find(w) : w \in works})
NoSelfMerge == \A m \in merges : m.w # m.l
\* The outermost row's winner is its class root (store.rs Corrupt check).
WinnerIsRoot == \A m \in merges : Outermost(m) => IsRoot(m.w)
\* Both guards allow the same rows, and every class with an active row has
\* a row each guard allows (no merge left un-undoable).
GuardsAgree    == \A m \in merges : Outermost(m) <=> RootGuard(m)
AlwaysUndoable ==
    \A m \in merges : \E m2 \in merges :
        Find(m2.w) = Find(m.w) /\ Outermost(m2) /\ RootGuard(m2)

\* Each active row still joins its true evidence.
EvidenceHolds == \A m \in merges : \A a, b \in m.why : Find(a) = Find(b)

\* Every external ID resolves to a class containing a base it was
\* evidenced with.
IDsFollowEvidence ==
    \A i \in IDs : idMap[i] # NoWork =>
        \E e \in evid : e[1] = i /\ Find(e[2]) = Find(idMap[i])

ReadEdgesRootsNoSelfLoop ==
    \A e \in ReadEdges : IsRoot(e[1]) /\ IsRoot(e[2]) /\ e[1] # e[2]

EdgeFaithful == ReadEdges = Faithful

NoOrphanWork == \A w \in works : \E i \in IDs : idMap[i] = w

\* Each live decision is the resolved state, or the resolved row is
\* needs_decision and an active row of the same class logged that state.
NoDecisionLost ==
    \A k \in known :
        LET r == Find(k[1]) IN
        \/ decision[r] = k[2]
        \/ /\ decision[r] = "needs_decision"
           /\ \E l \in reviewLog :
                 /\ \E m \in merges : m.id = l[1] /\ Find(m.w) = r
                 /\ l[3] = k[2] /\ Find(l[2]) = r

CursorImpliesEdges ==
    \A g \in 1..curGen : \A pg \in 1..cursor[g] : \A e \in PageEdges(g, pg) :
        /\ idMap[e[1]] # NoWork /\ idMap[e[2]] # NoWork
        /\ <<idMap[e[1]], idMap[e[2]]>> \in edges

\* Fresh edges (last_seen >= newest complete generation) are exactly the
\* edges that generation returned.
FreshIsNewest ==
    LET G == NewestComplete IN
    G > 0 =>
        {e \in edges : edgeSeen[e] >= G} =
            {<<idMap[r[1]], idMap[r[2]]>> : r \in UNION {PageEdges(G, q) : q \in 1..NPages}}

CursorMonotonic     == ok.cursor
ReplayIdempotent    == ok.replay
UndoCarriesDecision == ok.carry
UndoRestoresPrior   == ok.restore
SupersededNoWrite   == ok.gen

VetoHolds      == \A m \in merges : m.ev \notin vetoed
NoSpuriousVeto == vetoed \subseteq everUndone /\ ok.veto

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
NeverCursorFull     == cursor[1] < NPages
NeverReplay         == "replay" \notin seen
NeverUndoCarried    == "undo_carried" \notin seen
NeverDependentMerge == "dependent_merge" \notin seen
NeverAttachNonRoot  == "attach_nonroot" \notin seen
NeverNoStateMerge   == ~\E m \in merges : m.pl = NoState /\ m.pw # NoState
NeverRedo           == "redo" \notin seen
NeverSuperseded     == "superseded" \notin seen
NeverStaleEdge      == \A e \in edges : edgeSeen[e] >= NewestComplete
NeverSplit          == budget.split = 0
NeverSplitBlocked   == "split_blocked" \notin seen
NeverInnerRemoved   == "inner_removed" \notin seen
=============================================================================
