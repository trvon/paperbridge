//! Join cost vs store size: `cargo bench -p citegraph`.
//!
//! Each iteration observes one 200-record page whose records attach a new
//! OpenAlex id to an existing class (one mint + one logged merge each).
//! Per-join cost should stay flat as the store grows from 1k to 50k
//! classes: class lookups use the in-transaction member lists, not a scan.

use citegraph::{ExtId, Store, WorkRecord};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::cell::Cell;

fn w(n: u64) -> ExtId {
    ExtId::openalex(&format!("W{n}")).expect("openalex id")
}
fn d(n: u64) -> ExtId {
    ExtId::doi(&format!("10.5555/d{n}")).expect("doi")
}

/// A store holding `classes` two-id classes (`W{n}` + DOI `d{n}`).
fn seeded(classes: u64) -> Store {
    let mut store = Store::open_in_memory().expect("store");
    let records: Vec<WorkRecord> = (0..classes)
        .map(|n| WorkRecord::new(w(n)).with_id(Some(d(n))))
        .collect();
    for chunk in records.chunks(5_000) {
        store.observe(chunk, "openalex").expect("seed");
    }
    store
}

fn joins(c: &mut Criterion) {
    let mut group = c.benchmark_group("observe_200_joins");
    group.sample_size(20);
    for classes in [1_000u64, 50_000] {
        let mut store = seeded(classes);
        let next = Cell::new(10_000_000u64);
        group.bench_with_input(
            BenchmarkId::from_parameter(classes),
            &classes,
            |b, &classes| {
                b.iter(|| {
                    let page: Vec<WorkRecord> = (0..200)
                        .map(|i| {
                            let fresh = next.get() + i;
                            WorkRecord::new(w(fresh)).with_id(Some(d(fresh % classes)))
                        })
                        .collect();
                    next.set(next.get() + 200);
                    store.observe(&page, "openalex").expect("observe")
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, joins);
criterion_main!(benches);
