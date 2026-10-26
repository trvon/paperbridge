//! PDF text extraction on the repository fixtures: `cargo bench -p paperseed`.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use paperseed::app::extract_pdf_text_from_bytes;
use std::hint::black_box;
use std::path::Path;

fn bench_extract(c: &mut Criterion) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut group = c.benchmark_group("pdf_extract");
    group.sample_size(20);
    for name in [
        "arxiv_1408_5939_planar_subgraphs.pdf",
        "arxiv_1808_06100_polynomial_optimization.pdf",
    ] {
        let bytes = std::fs::read(dir.join(name)).expect("fixture PDF");
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_function(name.trim_end_matches(".pdf"), |b| {
            b.iter(|| extract_pdf_text_from_bytes(black_box(&bytes)))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_extract);
criterion_main!(benches);
