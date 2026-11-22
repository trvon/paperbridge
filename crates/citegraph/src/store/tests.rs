use super::*;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

fn w(n: u32) -> ExtId {
    ExtId::openalex(&format!("W{n}")).unwrap()
}
fn d(n: u32) -> ExtId {
    ExtId::doi(&format!("10.5555/d{n}")).unwrap()
}
fn s2(n: u32) -> ExtId {
    ExtId::semantic_scholar(&format!("{n:040x}")).unwrap()
}
fn ax(n: u32) -> ExtId {
    ExtId::arxiv(&format!("2401.{n:05}")).unwrap()
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
fn last_merge(store: &mut Store) -> i64 {
    store.merges().unwrap().last().unwrap().id
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
    assert_eq!(store.merges().unwrap().len(), 1, "s2(1) joined to d1");
    // OpenAlex reports W1 with DOI d1: two works share evidence.
    store.observe(&[rec(w(1), &[d(1)])], "openalex").unwrap();
    assert_eq!(store.merges().unwrap().len(), 2);
    assert_eq!(store.class_ids(&s2(1)).unwrap(), vec![d(1), w(1), s2(1)]);

    let id = last_merge(&mut store);
    assert!(store.undo_merge(id).unwrap());
    assert!(!store.undo_merge(id).unwrap());
    let undone = store.merges().unwrap().pop().unwrap();
    assert!(undone.undone_seq.unwrap() > undone.seq);
    assert_eq!(undone.undo_reason.as_deref(), Some("user"));
    assert_eq!(store.class_ids(&w(1)).unwrap(), vec![w(1)]);
    assert_eq!(store.class_ids(&s2(1)).unwrap(), vec![d(1), s2(1)]);
    // Sticky: the same evidence does not redo the merge ...
    assert_eq!(store.observe(&[rec(w(1), &[d(1)])], "openalex").unwrap(), 1);
    assert_eq!(store.class_ids(&w(1)).unwrap(), vec![w(1)]);
    // ... but redo does, as a new row.
    let redone = store.redo_merge(id).unwrap();
    assert!(redone.is_some_and(|r| r > id));
    assert_eq!(store.class_ids(&w(1)).unwrap(), vec![d(1), w(1), s2(1)]);
    assert_eq!(store.observe(&[rec(w(1), &[d(1)])], "openalex").unwrap(), 0);
}

/// Review probe t8: a new id arriving after a merge gets its own work and a
/// logged merge, so undoing the earlier merge cannot strand it.
#[test]
fn new_ids_join_by_logged_merge_and_survive_undo() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe(&[rec(w(1), &[])], "openalex").unwrap();
    store
        .observe(&[rec(s2(1), &[d(1)])], "semantic_scholar")
        .unwrap();
    store.observe(&[rec(w(1), &[d(1)])], "openalex").unwrap();
    let w1_d1 = last_merge(&mut store);
    store.observe(&[rec(w(9), &[d(1)])], "openalex").unwrap();
    let w9_d1 = last_merge(&mut store);
    assert_ne!(w1_d1, w9_d1, "W9 joined by its own merge row");

    assert!(store.undo_merge(w1_d1).unwrap());
    assert_eq!(store.class_ids(&w(1)).unwrap(), vec![w(1)]);
    assert_eq!(store.class_ids(&w(9)).unwrap(), vec![d(1), w(9), s2(1)]);
    assert!(store.undo_merge(w9_d1).unwrap());
    assert_eq!(store.class_ids(&w(9)).unwrap(), vec![w(9)]);
    assert_eq!(store.class_ids(&d(1)).unwrap(), vec![d(1), s2(1)]);
}

/// Review probe t1: undoing an earlier merge leaves a later merge with its
/// own evidence intact (classes are recomputed, nothing cascades wrongly),
/// and the veto only covers the undone evidence pair.
#[test]
fn undo_with_dependent_merge_keeps_later_evidence() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe(&[rec(w(1), &[])], "openalex").unwrap();
    store
        .observe(&[rec(s2(1), &[d(1)])], "semantic_scholar")
        .unwrap();
    store.observe(&[rec(w(1), &[d(1)])], "openalex").unwrap();
    let first = last_merge(&mut store);
    store.observe(&[rec(w(2), &[])], "openalex").unwrap();
    store.observe(&[rec(w(2), &[s2(1)])], "x").unwrap();
    assert_eq!(
        store.class_ids(&w(2)).unwrap(),
        vec![d(1), w(1), w(2), s2(1)]
    );

    assert!(store.undo_merge(first).unwrap());
    assert_eq!(store.class_ids(&w(1)).unwrap(), vec![w(1)]);
    assert_eq!(store.class_ids(&w(2)).unwrap(), vec![d(1), w(2), s2(1)]);
    // Re-observing the never-undone evidence is a no-op, not a veto.
    assert_eq!(store.observe(&[rec(w(2), &[s2(1)])], "x").unwrap(), 0);
    assert_eq!(store.class_ids(&w(2)).unwrap(), vec![d(1), w(2), s2(1)]);
    // Other evidence linking W1 to the class is not blocked by the undo.
    store.observe(&[rec(w(1), &[s2(1)])], "x").unwrap();
    assert_eq!(
        store.class_ids(&w(1)).unwrap(),
        vec![d(1), w(1), w(2), s2(1)]
    );
}

#[test]
fn conflicting_strong_ids_veto_merges() {
    let mut store = Store::open_in_memory().unwrap();
    store.observe(&[rec(w(1), &[d(1)])], "openalex").unwrap();
    store
        .observe(&[rec(s2(1), &[d(2)])], "semantic_scholar")
        .unwrap();
    // A record linking W1 and d2: W1's class holds d1, so no merge. d2 is
    // still recorded under its own work.
    let conflicts = store.observe(&[rec(w(1), &[d(2)])], "openalex").unwrap();
    assert_eq!(conflicts, 1);
    assert_eq!(store.class_ids(&w(1)).unwrap(), vec![d(1), w(1)]);
    assert_eq!(store.class_ids(&d(2)).unwrap(), vec![d(2), s2(1)]);
    assert_eq!(store.observe(&[rec(s2(1), &[w(1)])], "x").unwrap(), 1);
}

/// Review probe t5: preprint/published versions never merge and the result
/// does not depend on arrival order.
#[test]
fn versions_are_order_independent_and_never_merged() {
    let arxiv = ExtId::arxiv("1706.03762").unwrap();
    assert_eq!(ExtId::doi("10.48550/arXiv.1706.03762").unwrap(), arxiv);
    let folded = rec(s2(7), &[d(7), arxiv.clone()]);

    let mut first = Store::open_in_memory().unwrap();
    first
        .observe(
            &[rec(s2(7), std::slice::from_ref(&arxiv))],
            "semantic_scholar",
        )
        .unwrap();
    first
        .observe(std::slice::from_ref(&folded), "semantic_scholar")
        .unwrap();
    let mut second = Store::open_in_memory().unwrap();
    second
        .observe(std::slice::from_ref(&folded), "semantic_scholar")
        .unwrap();

    for store in [&mut first, &mut second] {
        assert_eq!(store.class_ids(&arxiv).unwrap(), vec![arxiv.clone()]);
        assert_eq!(store.class_ids(&s2(7)).unwrap(), vec![d(7), s2(7)]);
        assert_eq!(
            store.linked_ids(&arxiv).unwrap(),
            vec![arxiv.clone(), d(7), s2(7)]
        );
        // The preprint-only view arriving later does not re-merge.
        store
            .observe(
                &[rec(s2(7), std::slice::from_ref(&arxiv))],
                "semantic_scholar",
            )
            .unwrap();
        assert_eq!(store.class_ids(&arxiv).unwrap(), vec![arxiv.clone()]);
    }
    let split = first.merges().unwrap();
    assert!(
        split
            .iter()
            .any(|m| m.undo_reason.as_deref() == Some("version_split"))
    );

    let subject = rec(w(100), &[]);
    let state = second
        .begin_fetch(&subject, Direction::Refs, "openalex", false)
        .unwrap();
    second
        .commit_page(&state, &page(&[rec(s2(7), &[])], true))
        .unwrap();
    let items = second
        .neighbors(&w(100), Direction::Refs, ALL)
        .unwrap()
        .items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].versions, vec![arxiv]);
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
    assert!(!out.replay && out.state.complete);
    assert_eq!(out.state.fetched, 2);

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
    assert_eq!(store.neighbor_total(&d(1), Direction::Refs).unwrap(), 2);
    let paged = store
        .neighbors(
            &d(1),
            Direction::Refs,
            NeighborQuery {
                offset: 1,
                limit: 1,
                ..ALL
            },
        )
        .unwrap();
    assert_eq!((paged.total, paged.items[0].ids.clone()), (2, vec![w(3)]));
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

    let g3 = store
        .begin_fetch(&subject, Direction::CitedBy, "openalex", true)
        .unwrap();
    let out = store
        .commit_page(&g3, &page(&[rec(w(2), &[])], false))
        .unwrap();
    assert!(out.state.resumable());
    assert_eq!(
        neighbor_keys(&mut store, &w(1), Direction::CitedBy, false).len(),
        2
    );

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

/// Review probe t3: a slow worker's replay of an old generation does not
/// re-stamp a row that a later complete generation left out.
#[test]
fn late_replay_does_not_unstale() {
    let mut s = Store::open_in_memory().unwrap();
    let subj = rec(w(1), &[]);
    let g1 = s
        .begin_fetch(&subj, Direction::Refs, "openalex", false)
        .unwrap();
    let both = page(&[rec(w(2), &[]), rec(w(3), &[])], true);
    s.commit_page(&g1, &both).unwrap();
    let g2 = s
        .begin_fetch(&subj, Direction::Refs, "openalex", true)
        .unwrap();
    s.commit_page(&g2, &page(&[rec(w(2), &[])], true)).unwrap();
    assert_eq!(s.neighbor_total(&w(1), Direction::Refs).unwrap(), 1);
    let out = s.commit_page(&g1, &both).unwrap();
    assert!(out.replay && out.superseded);
    assert_eq!(s.neighbor_total(&w(1), Direction::Refs).unwrap(), 1);
}

/// Review probe t9: a page from a superseded generation writes nothing.
#[test]
fn superseded_generation_commit_is_a_no_op() {
    let mut s = Store::open_in_memory().unwrap();
    let subj = rec(w(1), &[]);
    let g1 = s
        .begin_fetch(&subj, Direction::CitedBy, "openalex", false)
        .unwrap();
    let g1b = s
        .commit_page(&g1, &page(&[rec(w(2), &[])], false))
        .unwrap()
        .state;
    let g2 = s
        .begin_fetch(&subj, Direction::CitedBy, "openalex", true)
        .unwrap();
    s.commit_page(&g2, &page(&[rec(w(4), &[])], true)).unwrap();
    assert_eq!(s.neighbor_total(&w(1), Direction::CitedBy).unwrap(), 1);
    let out = s
        .commit_page(&g1b, &page(&[rec(w(2), &[]), rec(w(3), &[])], false))
        .unwrap();
    assert!(out.superseded);
    assert_eq!((out.state.generation, out.state.pages), (1, 1));
    assert_eq!(s.neighbor_total(&w(1), Direction::CitedBy).unwrap(), 1);
    assert!(s.class_ids(&w(3)).unwrap().is_empty(), "nothing minted");
}

/// Review probe t6: staleness is scoped to (work, direction, source); a
/// complete refs fetch of the citing work does not hide a cited-by edge.
#[test]
fn staleness_does_not_cross_directions() {
    let mut s = Store::open_in_memory().unwrap();
    let g = s
        .begin_fetch(&rec(w(1), &[]), Direction::CitedBy, "openalex", false)
        .unwrap();
    s.commit_page(&g, &page(&[rec(w(9), &[])], true)).unwrap();
    let g = s
        .begin_fetch(&rec(w(9), &[]), Direction::Refs, "openalex", false)
        .unwrap();
    s.commit_page(&g, &page(&[rec(w(5), &[])], true)).unwrap();
    assert_eq!(s.neighbor_total(&w(1), Direction::CitedBy).unwrap(), 1);
    // In refs(W9)'s own view, W1 was first stored by an earlier cited-by
    // fetch and the later complete refs fetch left it out: stale there.
    assert_eq!(
        neighbor_keys(&mut s, &w(9), Direction::Refs, false),
        vec![vec![w(5)]]
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
    assert!(!first.replay && again.replay && !again.superseded);
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

#[test]
fn rate_and_daily_budget_are_shared_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite");
    let mut a = Store::open(&path).unwrap();
    let mut b = Store::open(&path).unwrap();
    let limit = RateLimit {
        per_second: 1.0,
        burst: 2,
        daily: 3,
    };
    let day = 1_795_305_600_000; // 2026-11-22T00:00Z
    assert_eq!(
        a.take_request("openalex", limit, day).unwrap(),
        RateGrant::Granted
    );
    assert_eq!(
        b.take_request("openalex", limit, day).unwrap(),
        RateGrant::Granted
    );
    assert_eq!(
        a.take_request("openalex", limit, day + 250).unwrap(),
        RateGrant::Wait(750)
    );
    assert_eq!(
        b.take_request("openalex", limit, day + 1000).unwrap(),
        RateGrant::Granted
    );
    assert_eq!(a.requests_today("openalex", day).unwrap(), 3);
    assert_eq!(
        a.take_request("openalex", limit, day + 5000).unwrap(),
        RateGrant::DailyExhausted
    );
    // Other sources and the next UTC day have their own budgets.
    assert_eq!(
        a.take_request("semantic_scholar", limit, day + 5000)
            .unwrap(),
        RateGrant::Granted
    );
    assert_eq!(
        b.take_request("openalex", limit, day + 86_400_000).unwrap(),
        RateGrant::Granted
    );
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

    // Page 2 is in flight on `a`. Meanwhile `b` mints S1's work and learns
    // W2 and S1 share DOI d2.
    b.observe(&[rec(s2(1), &[d(2)])], "semantic_scholar")
        .unwrap();
    b.observe(&[rec(w(2), &[d(2)])], "openalex").unwrap();

    let out = a.commit_page(&st, &page(&[rec(s2(1), &[])], true)).unwrap();
    assert!(!out.replay && out.state.complete && out.state.pages == 2);
    // One work per id: S1 was not minted again.
    let works: i64 = a
        .conn()
        .query_row("SELECT COUNT(*) FROM works", [], |r| r.get(0))
        .unwrap();
    assert_eq!(works, 4);
    assert_eq!(
        neighbor_keys(&mut a, &w(1), Direction::Refs, false),
        vec![vec![d(2), w(2), s2(1)]]
    );
    let id = last_merge(&mut b);
    assert!(b.undo_merge(id).unwrap());
    assert_eq!(
        neighbor_keys(&mut a, &w(1), Direction::Refs, false),
        vec![vec![w(2)], vec![d(2), s2(1)]]
    );
}

/// An undo landing during the await: the commit resolves through the
/// merges active at commit time, and the edge stays on its base work.
#[test]
fn undo_during_await_keeps_edges_on_base_works() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite");
    let mut a = Store::open(&path).unwrap();
    let mut b = Store::open(&path).unwrap();
    b.observe(&[rec(w(2), &[])], "openalex").unwrap();
    b.observe(&[rec(s2(1), &[d(2)])], "semantic_scholar")
        .unwrap();
    b.observe(&[rec(w(2), &[d(2)])], "openalex").unwrap();
    let st = a
        .begin_fetch(&rec(w(1), &[]), Direction::Refs, "openalex", false)
        .unwrap();
    assert_eq!(a.class_ids(&w(2)).unwrap(), vec![d(2), w(2), s2(1)]);
    let id = last_merge(&mut b);
    assert!(b.undo_merge(id).unwrap());
    a.commit_page(&st, &page(&[rec(w(2), &[]), rec(s2(1), &[])], true))
        .unwrap();
    assert_eq!(
        neighbor_keys(&mut a, &w(1), Direction::Refs, false),
        vec![vec![w(2)], vec![d(2), s2(1)]]
    );
}

/// Two threads with their own connections: one pages a fetch while the
/// other merges and undoes. BEGIN IMMEDIATE + busy_timeout serialize them;
/// invariants hold afterwards.
#[test]
fn two_connections_in_threads_keep_invariants() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite");
    Store::open(&path).unwrap();
    let fetcher = {
        let path = path.clone();
        std::thread::spawn(move || {
            let mut s = Store::open(&path).unwrap();
            let subject = rec(w(0), &[]);
            for round in 0..20u32 {
                let st = s
                    .begin_fetch(&subject, Direction::Refs, "openalex", round % 7 == 6)
                    .unwrap();
                let rows = [rec(w(1 + round % 4), &[]), rec(s2(1 + round % 2), &[])];
                s.commit_page(&st, &page(&rows, round % 5 == 4)).unwrap();
            }
        })
    };
    let merger = {
        let path = path.clone();
        std::thread::spawn(move || {
            let mut s = Store::open(&path).unwrap();
            for round in 0..20u32 {
                s.observe(&[rec(w(1 + round % 4), &[d(1 + round % 3)])], "x")
                    .unwrap();
                s.observe(&[rec(s2(1 + round % 2), &[d(1 + round % 3)])], "x")
                    .unwrap();
                if round % 3 == 2
                    && let Some(m) = s.merges().unwrap().iter().find(|m| m.undone_at.is_none())
                {
                    s.undo_merge(m.id).unwrap();
                }
            }
        })
    };
    fetcher.join().unwrap();
    merger.join().unwrap();
    let mut s = Store::open(&path).unwrap();
    check_forest(&s).unwrap();
    let read: BTreeSet<Vec<ExtId>> = neighbor_keys(&mut s, &w(0), Direction::Refs, true)
        .into_iter()
        .collect();
    let mut expected = BTreeSet::new();
    let scope = s.linked_ids(&w(0)).unwrap();
    for id in (1..5).map(w).chain((1..3).map(s2)) {
        let class = s.class_ids(&id).unwrap();
        if !class.is_empty() && !class.iter().any(|i| scope.contains(i)) {
            expected.insert(class);
        }
    }
    assert_eq!(read, expected);
}

fn check_forest(store: &Store) -> std::result::Result<(), TestCaseError> {
    let uf = UnionFind::load(store.conn());
    prop_assert!(uf.is_ok(), "active merges form a cycle");
    let uf = uf.unwrap();
    let active: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM merge_log WHERE undone_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    prop_assert_eq!(
        active as usize,
        uf.edge_count(),
        "every active row joins two classes"
    );
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
        prop_assert!(uf.is_root(root));
    }
    let orphans: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM works WHERE work_id NOT IN (SELECT work_id FROM work_ids)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    prop_assert_eq!(orphans, 0);
    // Versions are never merged.
    let mut classes: BTreeMap<i64, (bool, bool)> = BTreeMap::new();
    let mut stmt = store
        .conn()
        .prepare("SELECT scheme, work_id FROM work_ids")
        .unwrap();
    for row in stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .unwrap()
    {
        let (scheme, base) = row.unwrap();
        let e = classes.entry(uf.find(base)).or_default();
        e.0 |= scheme == "arxiv";
        e.1 |= scheme == "doi";
    }
    for (root, (arxiv, doi)) in classes {
        prop_assert!(
            !(arxiv && doi),
            "class {} holds an arXiv id and a DOI",
            root
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Property test mirroring MergeFetchDecide.tla's invariants, over all three
// directions, several subjects and arXiv/version ids.

const KEYS: [(u32, Direction); 6] = [
    (0, Direction::Refs),
    (0, Direction::CitedBy),
    (0, Direction::Related),
    (1, Direction::Refs),
    (1, Direction::CitedBy),
    (2, Direction::Related),
];
const PAGES_PER_KEY: usize = 4;
const PAGES_PER_GEN: i64 = 2;

fn subject_id(n: u32) -> ExtId {
    match n {
        0 => w(0),
        1 => s2(9),
        _ => ax(9),
    }
}

#[derive(Debug, Clone)]
enum Op {
    /// A worker reads the committed fetch state of a key (before its await).
    Snap(usize, usize),
    /// The worker commits the page at its snapshot's position. Content
    /// depends only on (key, generation, position), as from a source.
    Commit(usize),
    /// Another writer observes a record. Between a `Snap` and its `Commit`
    /// this is another worker creating the work for a page id, or a merge,
    /// during the await.
    Observe(WorkRecord),
    /// Between a `Snap` and its `Commit`: an undo during the await.
    Undo(usize),
    Redo(usize),
    Refresh(usize),
}

fn id_strategy() -> impl Strategy<Value = ExtId> {
    prop_oneof![
        (0u32..5).prop_map(w),
        (1u32..4).prop_map(d),
        (1u32..3).prop_map(s2),
        (1u32..3).prop_map(ax),
        Just(s2(9)),
        Just(ax(9)),
    ]
}

fn record_strategy() -> impl Strategy<Value = WorkRecord> {
    (
        prop_oneof![(0u32..5).prop_map(w), (1u32..3).prop_map(s2), Just(s2(9))],
        prop::collection::vec(id_strategy(), 0..3),
    )
        .prop_map(|(p, extra)| rec(p, &extra))
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0usize..2, 0usize..KEYS.len()).prop_map(|(w, k)| Op::Snap(w, k)),
        4 => (0usize..2).prop_map(Op::Commit),
        3 => record_strategy().prop_map(Op::Observe),
        1 => (0usize..6).prop_map(Op::Undo),
        1 => (0usize..6).prop_map(Op::Redo),
        1 => (0usize..KEYS.len()).prop_map(Op::Refresh),
    ]
}

/// `(citing, cited, source, first_seen_seq, refs_seen_seq, cited_by_seen_seq)`
type EdgeRow = (i64, i64, String, i64, Option<i64>, Option<i64>);

#[derive(Debug, PartialEq)]
struct Snapshot {
    work_ids: Vec<(String, String, i64)>,
    works: i64,
    edges: BTreeSet<EdgeRow>,
    related: BTreeSet<(i64, i64, String, i64, i64)>,
    fetch_log: Vec<(i64, String, i64, Option<String>, i64, bool)>,
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
        .prepare(
            "SELECT citing, cited, source, first_seen_seq, refs_seen_seq, cited_by_seen_seq
             FROM edge_sources",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let related = c
        .prepare("SELECT work, related, source, first_seen_seq, last_seen_seq FROM related_works")
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let fetch_log = c
        .prepare(
            "SELECT work, direction, generation, cursor, pages, complete FROM fetch_log ORDER BY 1, 2, 3",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    Snapshot {
        work_ids,
        works,
        edges,
        related,
        fetch_log,
    }
}

/// Committed raw pairs, per key: `(generation, row primary)`.
type Ghost = Vec<Vec<(i64, ExtId)>>;

fn check_invariants(store: &mut Store, ghost: &Ghost) -> std::result::Result<(), TestCaseError> {
    check_forest(store)?;
    for (k, &(subject_n, dir)) in KEYS.iter().enumerate() {
        let subject = subject_id(subject_n);
        // CursorImpliesEdges: every committed pair is present by base work.
        for (_, other) in &ghost[k] {
            let (Some(sb), Some(ob)) = (
                base_of(store.conn(), &subject).unwrap(),
                base_of(store.conn(), other).unwrap(),
            ) else {
                return Err(TestCaseError::fail("committed id missing from work_ids"));
            };
            let (sql, a, b) = match dir {
                Direction::Refs => (
                    "SELECT EXISTS(SELECT 1 FROM edge_sources WHERE citing = ?1 AND cited = ?2)",
                    sb,
                    ob,
                ),
                Direction::CitedBy => (
                    "SELECT EXISTS(SELECT 1 FROM edge_sources WHERE citing = ?1 AND cited = ?2)",
                    ob,
                    sb,
                ),
                Direction::Related => (
                    "SELECT EXISTS(SELECT 1 FROM related_works WHERE work = ?1 AND related = ?2)",
                    sb,
                    ob,
                ),
            };
            let present: bool = store
                .conn()
                .query_row(sql, params![a, b], |r| r.get(0))
                .unwrap();
            prop_assert!(
                present,
                "{:?} edge {} / {} missing",
                dir,
                subject.key(),
                other.key()
            );
        }
        // EdgeFaithful: the read view equals the committed pairs resolved
        // now, minus resolved self-loops (subject class and its versions).
        let scope = store.linked_ids(&subject).unwrap();
        let read: BTreeSet<Vec<ExtId>> = neighbor_keys(store, &subject, dir, true)
            .into_iter()
            .collect();
        for ids in &read {
            prop_assert!(ids.iter().all(|i| !scope.contains(i)), "self-loop in view");
        }
        // Citation edges are shared: refs(A) holding B is cited_by(B)
        // holding A. Build the expected view from every committed raw pair.
        let mut expected = BTreeSet::new();
        for (k2, &(n2, dir2)) in KEYS.iter().enumerate() {
            let s2_id = subject_id(n2);
            for (_, other) in &ghost[k2] {
                let (near, far) = match (dir, dir2) {
                    (Direction::Related, Direction::Related) => (&s2_id, other),
                    (Direction::Refs, Direction::Refs)
                    | (Direction::CitedBy, Direction::CitedBy) => (&s2_id, other),
                    (Direction::Refs, Direction::CitedBy)
                    | (Direction::CitedBy, Direction::Refs) => (other, &s2_id),
                    _ => continue,
                };
                let near_class = store.class_ids(near).unwrap();
                if !near_class.iter().any(|i| scope.contains(i)) {
                    continue;
                }
                let class = store.class_ids(far).unwrap();
                if !class.iter().any(|i| scope.contains(i)) {
                    expected.insert(class);
                }
            }
        }
        prop_assert_eq!(&read, &expected, "{:?} view of {}", dir, subject.key());

        // Staleness: everything the latest complete generation returned is
        // fresh.
        let base = base_of(store.conn(), &subject).unwrap();
        let latest_complete: Option<i64> = base.and_then(|b| {
            store
                .conn()
                .query_row(
                    "SELECT MAX(generation) FROM fetch_log WHERE work = ?1 AND direction = ?2 AND complete = 1",
                    params![b, dir.as_str()],
                    |r| r.get(0),
                )
                .unwrap()
        });
        if let Some(g) = latest_complete {
            let fresh: BTreeSet<Vec<ExtId>> = neighbor_keys(store, &subject, dir, false)
                .into_iter()
                .collect();
            for (gen_no, other) in &ghost[k] {
                let class = store.class_ids(other).unwrap();
                if *gen_no == g && !class.iter().any(|i| scope.contains(i)) {
                    prop_assert!(
                        fresh.contains(&class),
                        "returned by latest complete gen but stale"
                    );
                }
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn store_matches_merge_fetch_decide_invariants(
        pages in prop::collection::vec(
            prop::collection::vec(prop::collection::vec(record_strategy(), 1..3), PAGES_PER_KEY),
            KEYS.len(),
        ),
        ops in prop::collection::vec(op_strategy(), 1..40),
    ) {
        let mut store = Store::open_in_memory().unwrap();
        let mut snaps: [Option<(usize, FetchState)>; 2] = [None, None];
        let mut ghost: Ghost = vec![Vec::new(); KEYS.len()];
        let begin = |store: &mut Store, k: usize, refresh: bool| {
            let (n, dir) = KEYS[k];
            store.begin_fetch(&rec(subject_id(n), &[]), dir, "openalex", refresh).unwrap()
        };

        for op in ops {
            match op {
                Op::Snap(worker, k) => snaps[worker] = Some((k, begin(&mut store, k, false))),
                Op::Commit(worker) => {
                    let Some((k, state)) = snaps[worker].take() else { continue };
                    let idx = (state.pages + state.generation) as usize % PAGES_PER_KEY;
                    let pg = page(&pages[k][idx], state.pages + 1 >= PAGES_PER_GEN);
                    let before = snapshot(&store);
                    let out = store.commit_page(&state, &pg).unwrap();
                    if out.replay {
                        prop_assert_eq!(&before, &snapshot(&store), "replay changed the store");
                    } else {
                        prop_assert_eq!(out.state.pages, state.pages + 1);
                        let mid = snapshot(&store);
                        let again = store.commit_page(&state, &pg).unwrap();
                        prop_assert!(again.replay);
                        prop_assert_eq!(&mid, &snapshot(&store), "immediate replay changed the store");
                    }
                    if !out.superseded {
                        for row in &pages[k][idx] {
                            ghost[k].push((state.generation, row.primary.clone()));
                        }
                    }
                }
                Op::Observe(r) => {
                    store.observe(&[r], "other").unwrap();
                }
                Op::Undo(i) => {
                    if let Some(m) = store.merges().unwrap().get(i) {
                        store.undo_merge(m.id).unwrap();
                    }
                }
                Op::Redo(i) => {
                    if let Some(m) = store.merges().unwrap().get(i) {
                        store.redo_merge(m.id).unwrap();
                    }
                }
                Op::Refresh(k) => {
                    begin(&mut store, k, true);
                }
            }
            check_invariants(&mut store, &ghost)?;
        }
    }
}
