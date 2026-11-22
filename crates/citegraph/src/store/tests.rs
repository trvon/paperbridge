use super::*;
use proptest::prelude::*;
use std::collections::BTreeSet;

fn w(n: u32) -> ExtId {
    ExtId::openalex(&format!("W{n}")).unwrap()
}
fn d(n: u32) -> ExtId {
    ExtId::doi(&format!("10.5555/d{n}")).unwrap()
}
fn s2(n: u32) -> ExtId {
    ExtId::semantic_scholar(&format!("{n:040x}")).unwrap()
}
fn rec(primary: ExtId, extra: &[ExtId]) -> WorkRecord {
    let mut r = WorkRecord::new(primary);
    for id in extra {
        r = r.with_id(Some(id.clone()));
    }
    r
}
fn page(rows: &[WorkRecord], exhausted: bool) -> Page {
    Page {
        rows: rows.iter().cloned().map(PageRow::new).collect(),
        exhausted,
        next_cursor: (!exhausted).then(|| "next".to_string()),
        ..Page::default()
    }
}
const ALL: NeighborQuery = NeighborQuery {
    offset: 0,
    limit: 1000,
    include_stale: false,
    include_contexts: true,
};
fn neighbor_keys(
    store: &mut Store,
    subject: &ExtId,
    dir: Direction,
    stale: bool,
) -> Vec<Vec<ExtId>> {
    let q = NeighborQuery {
        include_stale: stale,
        ..ALL
    };
    store
        .neighbors(subject, dir, q)
        .unwrap()
        .items
        .into_iter()
        .map(|n| n.ids)
        .collect()
}

#[test]
fn file_store_uses_wal() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("nested/citegraph.sqlite")).unwrap();
    assert_eq!(store.journal_mode().unwrap(), "wal");
}

#[test]
fn strong_id_match_merges_and_undo_splits() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe(&[rec(w(1), &[])], "openalex").unwrap();
    store
        .observe(&[rec(s2(1), &[d(1)])], "semantic_scholar")
        .unwrap();
    assert!(store.merges().unwrap().is_empty());
    // OpenAlex reports W1 with DOI d1: the same id held by two works.
    store.observe(&[rec(w(1), &[d(1)])], "openalex").unwrap();
    let merges = store.merges().unwrap();
    assert_eq!(merges.len(), 1);
    assert_eq!(store.class_ids(&s2(1)).unwrap(), vec![d(1), w(1), s2(1)]);

    assert!(store.undo_merge(merges[0].id).unwrap());
    assert!(!store.undo_merge(merges[0].id).unwrap());
    let w1 = store.class_ids(&w(1)).unwrap();
    let s1 = store.class_ids(&s2(1)).unwrap();
    assert_ne!(w1, s1);
    // Undo is sticky: the same evidence does not redo the merge.
    store.observe(&[rec(w(1), &[d(1)])], "openalex").unwrap();
    assert_eq!(store.merges().unwrap().len(), 1);
}

#[test]
fn conflicting_strong_ids_veto_merges() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe(&[rec(w(1), &[d(1)])], "openalex").unwrap();
    store
        .observe(&[rec(s2(1), &[d(2)])], "semantic_scholar")
        .unwrap();
    // A record linking W1 and d2: W1's class holds d1, so d2 is withheld.
    let conflicts = store.observe(&[rec(w(1), &[d(2)])], "openalex").unwrap();
    assert_eq!(conflicts, 1);
    assert!(store.merges().unwrap().is_empty());
    // A record linking the two classes by source-native ids: vetoed by DOI.
    store.observe(&[rec(s2(1), &[w(1)])], "x").unwrap();
    assert!(store.merges().unwrap().is_empty());
}

#[test]
fn arxiv_and_publisher_doi_are_versions_not_merges() {
    let mut store = Store::open_in_memory().unwrap();
    let arxiv = ExtId::arxiv("1706.03762").unwrap();
    let arxiv_doi = ExtId::doi("10.48550/arXiv.1706.03762").unwrap();
    assert_eq!(arxiv_doi, arxiv);
    store
        .observe(&[rec(w(7), &[arxiv_doi])], "openalex")
        .unwrap();
    // Semantic Scholar folds both versions into one paper.
    store
        .observe(&[rec(s2(7), &[d(7), arxiv.clone()])], "semantic_scholar")
        .unwrap();
    assert!(store.merges().unwrap().is_empty());
    assert_eq!(store.class_ids(&s2(7)).unwrap(), vec![d(7), s2(7)]);
    assert_eq!(store.class_ids(&arxiv).unwrap(), vec![arxiv.clone(), w(7)]);

    let subject = rec(w(100), &[]);
    let state = store
        .begin_fetch(&subject, Direction::Refs, "openalex", false)
        .unwrap();
    store
        .commit_page(&state, &page(&[rec(s2(7), &[])], true))
        .unwrap();
    let items = store
        .neighbors(&w(100), Direction::Refs, ALL)
        .unwrap()
        .items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].versions, vec![arxiv, w(7)]);
}

#[test]
fn refs_fetch_records_coverage_and_dedupes_sources() {
    let mut store = Store::open_in_memory().unwrap();
    let subject = rec(w(1), &[d(1)]);
    let oa = store
        .begin_fetch(&subject, Direction::Refs, "openalex", false)
        .unwrap();
    assert_eq!((oa.generation, oa.pages, oa.complete), (1, 0, false));
    let out = store
        .commit_page(
            &oa,
            &Page {
                total_reported: Some(2),
                ..page(&[rec(w(2), &[]), rec(w(3), &[])], true)
            },
        )
        .unwrap();
    assert!(!out.replay);
    assert!(out.state.complete);
    assert_eq!(out.state.fetched, 2);

    // Semantic Scholar reports one of the same references with context.
    let s2_subject = rec(s2(1), &[d(1)]);
    let st = store
        .begin_fetch(&s2_subject, Direction::Refs, "semantic_scholar", false)
        .unwrap();
    let mut row = PageRow::new(rec(s2(2), &[d(2)]));
    row.contexts = vec!["as shown in [3]".into()];
    row.intents = vec!["background".into()];
    row.is_influential = Some(true);
    store
        .commit_page(
            &st,
            &Page {
                rows: vec![row],
                exhausted: true,
                ..Page::default()
            },
        )
        .unwrap();
    // d2 links to W2 only after OpenAlex reports W2's DOI.
    store.observe(&[rec(w(2), &[d(2)])], "openalex").unwrap();

    let items = store.neighbors(&d(1), Direction::Refs, ALL).unwrap().items;
    assert_eq!(items.len(), 2);
    let merged = &items[0];
    assert_eq!(merged.ids, vec![d(2), w(2), s2(2)]);
    assert_eq!(merged.sources.len(), 2);
    assert_eq!(
        merged.contexts,
        vec![("semantic_scholar".into(), "as shown in [3]".into())]
    );
    let s2_src = merged
        .sources
        .iter()
        .find(|s| s.source == "semantic_scholar")
        .unwrap();
    assert_eq!(s2_src.is_influential, Some(true));
    assert_eq!(s2_src.intents, vec!["background".to_string()]);
    let cov = store.coverage(&d(1), Direction::Refs).unwrap();
    assert_eq!(cov.len(), 2);
    assert!(cov.iter().all(|c| c.complete));
}

#[test]
fn staleness_needs_a_later_complete_untruncated_fetch() {
    let mut store = Store::open_in_memory().unwrap();
    let subject = rec(w(1), &[]);
    let g1 = store
        .begin_fetch(&subject, Direction::CitedBy, "openalex", false)
        .unwrap();
    store
        .commit_page(&g1, &page(&[rec(w(2), &[]), rec(w(3), &[])], true))
        .unwrap();

    // Truncated refresh without w3: nothing becomes stale.
    let g2 = store
        .begin_fetch(&subject, Direction::CitedBy, "openalex", true)
        .unwrap();
    assert_eq!(g2.generation, 2);
    let out = store
        .commit_page(
            &g2,
            &Page {
                hard_cap: true,
                ..page(&[rec(w(2), &[])], false)
            },
        )
        .unwrap();
    assert!(out.state.truncated && !out.state.complete);
    assert_eq!(
        neighbor_keys(&mut store, &w(1), Direction::CitedBy, false).len(),
        2
    );

    // Interrupted refresh (budget hit, cursor kept): still nothing stale.
    let g3 = store
        .begin_fetch(&subject, Direction::CitedBy, "openalex", true)
        .unwrap();
    let out = store
        .commit_page(&g3, &page(&[rec(w(2), &[])], false))
        .unwrap();
    assert!(out.state.resumable());
    assert_eq!(out.state.cursor.as_deref(), Some("next"));
    assert_eq!(
        neighbor_keys(&mut store, &w(1), Direction::CitedBy, false).len(),
        2
    );

    // Resume the same generation to completion without w3: w3 is stale.
    let resumed = store
        .begin_fetch(&subject, Direction::CitedBy, "openalex", false)
        .unwrap();
    assert_eq!((resumed.generation, resumed.pages), (3, 1));
    store
        .commit_page(&resumed, &page(&[rec(w(4), &[])], true))
        .unwrap();
    assert_eq!(
        neighbor_keys(&mut store, &w(1), Direction::CitedBy, false),
        vec![vec![w(2)], vec![w(4)]]
    );
    assert_eq!(
        neighbor_keys(&mut store, &w(1), Direction::CitedBy, true).len(),
        3
    );
}

#[test]
fn replayed_page_does_not_move_cursor() {
    let mut store = Store::open_in_memory().unwrap();
    let subject = rec(w(1), &[]);
    let st = store
        .begin_fetch(&subject, Direction::CitedBy, "openalex", false)
        .unwrap();
    let p = page(&[rec(w(2), &[])], false);
    let first = store.commit_page(&st, &p).unwrap();
    let again = store.commit_page(&st, &p).unwrap();
    assert!(!first.replay && again.replay);
    assert_eq!(first.state, again.state);
}

#[test]
fn unresolved_refs_are_kept_once() {
    let mut store = Store::open_in_memory().unwrap();
    let st = store
        .begin_fetch(
            &rec(s2(1), &[]),
            Direction::CitedBy,
            "semantic_scholar",
            false,
        )
        .unwrap();
    let p = Page {
        unresolved: vec![r#"{"paperId":null,"title":"x"}"#.into()],
        exhausted: true,
        ..Page::default()
    };
    store.commit_page(&st, &p).unwrap();
    store.commit_page(&st, &p).unwrap();
    let n: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM unresolved_refs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

/// `CommitPage` with a merge landing during the await, across two
/// connections on one file (the CLI and the MCP server).
#[test]
fn merge_during_await_is_resolved_inside_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite");
    let mut a = Store::open(&path).unwrap();
    let mut b = Store::open(&path).unwrap();
    let subject = rec(w(1), &[]);
    let st = a
        .begin_fetch(&subject, Direction::Refs, "openalex", false)
        .unwrap();
    let st = a
        .commit_page(&st, &page(&[rec(w(2), &[])], false))
        .unwrap()
        .state;

    // Page 2 is in flight on `a`. Meanwhile `b` records S2's view of the
    // same paper and then learns W2 and S1 share DOI d2.
    b.observe(&[rec(s2(1), &[d(2)])], "semantic_scholar")
        .unwrap();
    b.observe(&[rec(w(2), &[d(2)])], "openalex").unwrap();
    assert_eq!(b.merges().unwrap().len(), 1);

    let out = a.commit_page(&st, &page(&[rec(s2(1), &[])], true)).unwrap();
    assert!(!out.replay && out.state.complete && out.state.pages == 2);
    // S1 was not minted again: it resolves to the work `b` created.
    let works: i64 = a
        .conn()
        .query_row("SELECT COUNT(*) FROM works", [], |r| r.get(0))
        .unwrap();
    assert_eq!(works, 3);
    assert_eq!(
        neighbor_keys(&mut a, &w(1), Direction::Refs, false),
        vec![vec![d(2), w(2), s2(1)]]
    );
    // Undo on `b`; `a` sees both edges again, by base work.
    let id = b.merges().unwrap()[0].id;
    assert!(b.undo_merge(id).unwrap());
    assert_eq!(
        neighbor_keys(&mut a, &w(1), Direction::Refs, false),
        vec![vec![w(2)], vec![d(2), s2(1)]]
    );
}

// ---------------------------------------------------------------------------
// Property test mirroring MergeFetchDecide.tla's invariants.

#[derive(Debug, Clone)]
enum Op {
    /// A worker reads the committed fetch state (before its await).
    Snap(usize),
    /// The worker commits the page at its snapshot's position, as the
    /// source would return it (page content depends only on position).
    Commit(usize),
    /// Another writer observes a record (may merge, veto, attach ids).
    Observe(WorkRecord),
    Undo(usize),
    Refresh,
}

fn id_strategy() -> impl Strategy<Value = ExtId> {
    prop_oneof![
        (0u32..5).prop_map(w),
        (1u32..4).prop_map(d),
        (1u32..3).prop_map(s2),
    ]
}

fn record_strategy() -> impl Strategy<Value = WorkRecord> {
    (
        prop_oneof![(0u32..5).prop_map(w), (1u32..3).prop_map(s2)],
        prop::collection::vec(id_strategy(), 0..3),
    )
        .prop_map(|(p, extra)| rec(p, &extra))
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0usize..2).prop_map(Op::Snap),
        3 => (0usize..2).prop_map(Op::Commit),
        3 => record_strategy().prop_map(Op::Observe),
        1 => (0usize..4).prop_map(Op::Undo),
        1 => Just(Op::Refresh),
    ]
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    work_ids: Vec<(String, String, i64)>,
    works: i64,
    edges: BTreeSet<(i64, i64, String)>,
    fetch_log: Vec<(i64, i64, Option<String>, i64, bool)>,
}

fn snapshot(store: &Store) -> Snapshot {
    let c = store.conn();
    let work_ids = c
        .prepare("SELECT scheme, value, work_id FROM work_ids ORDER BY 1, 2")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let works = c
        .query_row("SELECT COUNT(*) FROM works", [], |r| r.get(0))
        .unwrap();
    let edges = c
        .prepare("SELECT citing, cited, source FROM edge_sources")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let fetch_log = c
        .prepare("SELECT work, generation, cursor, pages, complete FROM fetch_log ORDER BY 1, 2")
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    Snapshot {
        work_ids,
        works,
        edges,
        fetch_log,
    }
}

fn check_invariants(
    store: &mut Store,
    subject: &ExtId,
    committed: &[(ExtId, ExtId)],
) -> std::result::Result<(), TestCaseError> {
    let uf = UnionFind::load(store.conn()).unwrap();
    // Forest: no self-merge, single parent (load enforces), acyclic, and
    // every id and work resolves to a root.
    for (loser, winner) in uf.active() {
        prop_assert_ne!(loser, winner);
    }
    let bases: Vec<i64> = store
        .conn()
        .prepare("SELECT work_id FROM work_ids")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    for base in &bases {
        let root = uf.find(*base);
        prop_assert!(root.is_ok(), "cycle from {}", base);
        prop_assert!(uf.is_root(root.unwrap()));
    }
    // No orphan works.
    let orphans: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM works WHERE work_id NOT IN (SELECT work_id FROM work_ids)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    prop_assert_eq!(orphans, 0);

    // A committed cursor implies its page's edges are present, by base work.
    for (citing, cited) in committed {
        let (Some(a), Some(b)) = (
            base_of(store.conn(), citing).unwrap(),
            base_of(store.conn(), cited).unwrap(),
        ) else {
            return Err(TestCaseError::fail("committed id missing from work_ids"));
        };
        let present: bool = store
            .conn()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM edge_sources WHERE citing = ?1 AND cited = ?2)",
                params![a, b],
                |r| r.get(0),
            )
            .unwrap();
        prop_assert!(present, "edge {} -> {} missing", citing.key(), cited.key());
    }

    // Read view: endpoints are roots, no resolved self-loops, and it equals
    // the committed raw pairs resolved now (EdgeFaithful).
    let subject_class = store.class_ids(subject).unwrap();
    let read: BTreeSet<Vec<ExtId>> = neighbor_keys(store, subject, Direction::Refs, true)
        .into_iter()
        .collect();
    for ids in &read {
        prop_assert!(ids.iter().all(|i| !subject_class.contains(i)), "self-loop");
    }
    let mut expected = BTreeSet::new();
    for (_, cited) in committed {
        let class = store.class_ids(cited).unwrap();
        if class != subject_class {
            expected.insert(class);
        }
    }
    prop_assert_eq!(read, expected);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn store_matches_merge_fetch_decide_invariants(
        pages in prop::collection::vec(prop::collection::vec(record_strategy(), 1..3), 3),
        ops in prop::collection::vec(op_strategy(), 1..30),
    ) {
        let mut store = Store::open_in_memory().unwrap();
        let subject_rec = rec(w(0), &[]);
        let subject = w(0);
        let mut snaps: [Option<FetchState>; 2] = [None, None];
        let mut committed: Vec<(ExtId, ExtId)> = Vec::new();
        store.begin_fetch(&subject_rec, Direction::Refs, "openalex", false).unwrap();

        for op in ops {
            match op {
                Op::Snap(worker) => {
                    snaps[worker] = store
                        .begin_fetch(&subject_rec, Direction::Refs, "openalex", false)
                        .ok();
                }
                Op::Commit(worker) => {
                    let Some(state) = snaps[worker].take() else { continue };
                    let p = (state.pages as usize) % pages.len();
                    let pg = page(&pages[p], false);
                    let before = snapshot(&store);
                    let out = store.commit_page(&state, &pg).unwrap();
                    if out.replay {
                        let after = snapshot(&store);
                        prop_assert_eq!(&before.edges, &after.edges);
                        prop_assert_eq!(&before.fetch_log, &after.fetch_log);
                        if out.identity_conflicts == 0 {
                            prop_assert_eq!(&before.work_ids, &after.work_ids);
                            prop_assert_eq!(before.works, after.works);
                        }
                    } else {
                        prop_assert_eq!(out.state.pages, state.pages + 1);
                        // Replay of the same page right away is idempotent.
                        let mid = snapshot(&store);
                        let again = store.commit_page(&state, &pg).unwrap();
                        prop_assert!(again.replay);
                        let after = snapshot(&store);
                        prop_assert_eq!(&mid.edges, &after.edges);
                        prop_assert_eq!(&mid.fetch_log, &after.fetch_log);
                        prop_assert_eq!(&mid.work_ids, &after.work_ids);
                        prop_assert_eq!(mid.works, after.works);
                    }
                    for row in &pages[p] {
                        committed.push((subject.clone(), row.primary.clone()));
                    }
                }
                Op::Observe(r) => {
                    store.observe(&[r], "other").unwrap();
                }
                Op::Undo(k) => {
                    let merges = store.merges().unwrap();
                    if let Some(m) = merges.get(k) {
                        store.undo_merge(m.id).unwrap();
                    }
                }
                Op::Refresh => {
                    store.begin_fetch(&subject_rec, Direction::Refs, "openalex", true).unwrap();
                }
            }
            check_invariants(&mut store, &subject, &committed)?;
        }
    }
}
