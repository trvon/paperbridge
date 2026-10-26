//! BM25F index micro-benchmarks: `cargo bench -p paperseed-index`.

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use paperseed_index::{Index, IndexBuilder, paperseed_defaults, tokenize};
use std::hint::black_box;

const VOCAB: &[&str] = &[
    "graph",
    "neural",
    "network",
    "storage",
    "deduplication",
    "consensus",
    "protocol",
    "efficient",
    "learning",
    "transformer",
    "attention",
    "compiler",
    "verification",
    "distributed",
    "database",
    "index",
    "retrieval",
    "ranking",
    "kernel",
    "memory",
    "cache",
    "latency",
    "throughput",
    "privacy",
    "security",
    "fuzzing",
    "symbolic",
    "execution",
    "planar",
    "subgraph",
    "polynomial",
    "optimization",
    "quantum",
    "theorem",
];

/// Deterministic xorshift so runs are comparable without a `rand` dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn words(&mut self, n: usize) -> String {
        (0..n)
            .map(|_| VOCAB[(self.next() % VOCAB.len() as u64) as usize])
            .collect::<Vec<_>>()
            .join(" ")
    }
}

struct Doc {
    id: String,
    title: String,
    authors: String,
    abstract_text: String,
    full_text: String,
}

impl Doc {
    fn fields(&self) -> [(&str, &str); 4] {
        [
            ("title", self.title.as_str()),
            ("authors", self.authors.as_str()),
            ("abstract", self.abstract_text.as_str()),
            ("full_text", self.full_text.as_str()),
        ]
    }
}

fn corpus(n: usize) -> Vec<Doc> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    (0..n)
        .map(|i| Doc {
            id: format!("doc-{i:06}"),
            title: rng.words(8),
            authors: rng.words(3),
            abstract_text: rng.words(120),
            full_text: rng.words(1_500),
        })
        .collect()
}

fn build(docs: &[Doc]) -> Index {
    let mut builder = IndexBuilder::new(paperseed_defaults());
    for doc in docs {
        builder.add_document(doc.id.clone(), &doc.fields());
    }
    builder.build()
}

fn bench_tokenize(c: &mut Criterion) {
    let mut rng = Rng(7);
    let text = format!("Ef\u{fb01}cient Caf\u{e9} {}", rng.words(2_000));
    let mut group = c.benchmark_group("tokenize");
    group.throughput(Throughput::Bytes(text.len() as u64));
    group.bench_function("2k_words", |b| b.iter(|| tokenize(black_box(&text))));
    group.finish();
}

fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("build");
    group.sample_size(10);
    for n in [100, 1_000] {
        let docs = corpus(n);
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &docs, |b, docs| {
            b.iter(|| build(black_box(docs)))
        });
    }
    group.finish();
}

fn bench_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_top10");
    for n in [1_000, 10_000] {
        let index = build(&corpus(n));
        group.bench_with_input(BenchmarkId::new("3_terms", n), &index, |b, index| {
            b.iter(|| index.search(black_box("efficient graph retrieval"), 10))
        });
        group.bench_with_input(BenchmarkId::new("common_term", n), &index, |b, index| {
            b.iter(|| index.search(black_box("network"), 10))
        });
    }
    group.finish();
}

fn bench_upsert(c: &mut Criterion) {
    let docs = corpus(1_000);
    let index = build(&docs);
    let replacement = &corpus(1)[0];
    c.bench_function("upsert_existing/1000", |b| {
        b.iter_batched(
            || index.clone(),
            |mut index| index.upsert_document("doc-000500", &replacement.fields()),
            BatchSize::LargeInput,
        )
    });
}

fn bench_codec(c: &mut Criterion) {
    let index = build(&corpus(1_000));
    let bytes = index.to_bytes().expect("serialize");
    let mut group = c.benchmark_group("codec/1000");
    group.throughput(Throughput::Bytes(bytes.len() as u64));
    group.bench_function("to_bytes", |b| b.iter(|| black_box(&index).to_bytes()));
    group.bench_function("from_bytes", |b| {
        b.iter(|| Index::from_bytes(black_box(&bytes)))
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_tokenize,
    bench_build,
    bench_search,
    bench_upsert,
    bench_codec
);
criterion_main!(benches);
