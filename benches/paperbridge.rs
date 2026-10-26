//! Root-crate micro-benchmarks: `cargo bench -p paperbridge`.
//!
//! Text workloads use the real PDF fixtures under crates/paperseed/tests/fixtures
//! so chunking and section detection see realistic paper layout.

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use paperbridge::chunking::split_for_tts;
use paperbridge::hit_enrich::{apply_detail, enrich_hit_identity, enrich_match};
use paperbridge::internals;
use paperbridge::models::{PaperHit, PaperSource, SearchDetail};
use paperbridge::paper::tei::parse_tei;
use std::hint::black_box;
use std::path::Path;

const SOURCES: [PaperSource; 6] = [
    PaperSource::Arxiv,
    PaperSource::Crossref,
    PaperSource::OpenAlex,
    PaperSource::SemanticScholar,
    PaperSource::Dblp,
    PaperSource::EuropePmc,
];

fn fixture_text() -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/paperseed/tests/fixtures");
    [
        "arxiv_1408_5939_planar_subgraphs.pdf",
        "arxiv_1808_06100_polynomial_optimization.pdf",
    ]
    .iter()
    .filter_map(|name| std::fs::read(dir.join(name)).ok())
    .filter_map(|bytes| {
        paperseed::app::extract_pdf_text_from_bytes(&bytes)
            .ok()
            .flatten()
    })
    .collect::<Vec<_>>()
    .join("\n\n")
}

fn hit(index: usize) -> PaperHit {
    // Every third hit repeats an earlier paper from another source so dedupe
    // has real merges to do, mixing DOI, arXiv, and title-only identities.
    let paper = if index.is_multiple_of(3) {
        index / 3
    } else {
        index
    };
    let mut hit = PaperHit::new(
        SOURCES[index % SOURCES.len()],
        format!("Scalable Retrieval for Scientific Corpora, Part {paper}"),
        vec!["Ada Lovelace".into(), "Grace Hopper".into()],
        Some("2026".into()),
        (paper % 2 == 0).then(|| format!("10.5555/bench.{paper}")),
        (paper % 5 == 0).then(|| format!("2607.{paper:05}v{}", index % 3 + 1)),
        None,
        Some("An abstract long enough to exercise projection. ".repeat(6)),
        Some(format!("https://example.org/paper/{paper}")),
        None,
        None,
        Some("Benchmark Symposium".into()),
        Some(paper as u32),
    );
    enrich_hit_identity(&mut hit);
    hit
}

fn synthetic_tei(sections: usize) -> String {
    let mut body = String::new();
    for i in 1..=sections {
        body.push_str(&format!(
            "<div><head n=\"{i}\">Section {i}</head><p>{}</p><p>{}</p></div>",
            "Body text with <ref type=\"bibr\">[1]</ref> citations. ".repeat(20),
            "More discussion &amp; results. ".repeat(20),
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<TEI xmlns="http://www.tei-c.org/ns/1.0"><teiHeader><fileDesc><titleStmt>
<title level="a" type="main">Benchmark Paper</title></titleStmt>
<sourceDesc><biblStruct><analytic><author><persName><forename>Ada</forename>
<surname>Lovelace</surname></persName></author><idno type="DOI">10.5555/bench</idno>
</analytic></biblStruct></sourceDesc></fileDesc><profileDesc><abstract><p>Abstract.</p>
</abstract></profileDesc></teiHeader><text><body>{body}</body></text></TEI>"#
    )
}

fn bench_text(c: &mut Criterion) {
    let text = fixture_text();
    assert!(!text.is_empty(), "PDF fixtures produced no text");
    let mut group = c.benchmark_group("text");
    group.throughput(Throughput::Bytes(text.len() as u64));
    group.bench_function("split_technical_content/1200", |b| {
        b.iter(|| internals::split_technical_content(black_box(&text), 1_200, Some("Paper > Body")))
    });
    group.bench_function("split_for_tts/1200", |b| {
        b.iter(|| split_for_tts(black_box(&text), 1_200))
    });
    group.bench_function("fallback_sections", |b| {
        b.iter(|| internals::fallback_section_count(Some("Abstract."), black_box(&text)))
    });
    group.bench_function("book_toc", |b| {
        b.iter(|| internals::book_toc_chapter_count(black_box(&text)))
    });
    group.finish();
}

fn bench_tei(c: &mut Criterion) {
    let xml = synthetic_tei(40);
    let mut group = c.benchmark_group("tei");
    group.throughput(Throughput::Bytes(xml.len() as u64));
    group.bench_function("parse_40_sections", |b| {
        b.iter(|| parse_tei("ITEM", "ATTACH", black_box(&xml)))
    });
    group.finish();
}

fn bench_hits(c: &mut Criterion) {
    let hits: Vec<PaperHit> = (1..=200).map(hit).collect();
    c.bench_function("dedupe/200", |b| {
        b.iter_batched(
            || hits.clone(),
            internals::dedupe_hits,
            BatchSize::SmallInput,
        )
    });
    c.bench_function("enrich_and_project/200", |b| {
        b.iter_batched(
            || hits.clone(),
            |mut hits| {
                for hit in &mut hits {
                    enrich_match(hit, "scalable retrieval scientific corpora");
                    apply_detail(hit, SearchDetail::Compact, None);
                }
                hits
            },
            BatchSize::SmallInput,
        )
    });
    c.bench_function("serialize_hits/200", |b| {
        b.iter(|| serde_json::to_string(black_box(&hits)))
    });
}

fn bench_ids(c: &mut Criterion) {
    let inputs = [
        "https://dx.doi.org/10.1145/3290605.3300857",
        "doi:10.48550/ARXIV.1706.03762",
        "arXiv:1706.03762v7",
        "https://arxiv.org/pdf/1706.03762v5.pdf",
        "ISBN-13: 978-1-4919-0307-0",
        "attention is all you need",
    ];
    c.bench_function("normalize_ids/6", |b| {
        b.iter(|| {
            for raw in inputs {
                black_box(internals::normalize_doi(raw));
                black_box(internals::normalize_arxiv(raw));
                black_box(internals::normalize_isbn(raw));
            }
        })
    });
}

criterion_group!(benches, bench_text, bench_tei, bench_hits, bench_ids);
criterion_main!(benches);
