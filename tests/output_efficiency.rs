use paperbridge::hit_enrich::{apply_detail, enrich_hit_identity, enrich_match};
use paperbridge::models::{
    PaperHit, PaperSource, SearchDetail, SearchDiagnostics, SearchPapersResult, SourceDiagnostic,
};

const TOKEN_ESTIMATOR: &str = "ceil(utf8_bytes/4)";

fn estimated_tokens(json: &str) -> usize {
    json.len().div_ceil(4)
}

fn representative_hit(index: usize) -> PaperHit {
    PaperHit::new(
        PaperSource::Arxiv,
        format!("Context-Efficient Paper Retrieval Study {index}"),
        vec![
            "Ada Lovelace".into(),
            "Grace Hopper".into(),
            "Edsger Dijkstra".into(),
            "Donald Knuth".into(),
        ],
        Some("2026".into()),
        Some(format!("10.5555/context.{index}")),
        Some(format!("2607.{index:05}v2")),
        None,
        Some(
            "A deliberately long abstract that is useful only after a paper is selected. "
                .repeat(8),
        ),
        Some(format!("https://arxiv.org/abs/2607.{index:05}")),
        Some(format!("https://arxiv.org/pdf/2607.{index:05}")),
        Some(format!("https://arxiv.org/pdf/2607.{index:05}")),
        Some("Proceedings of the Context Engineering Symposium".into()),
        Some(1_000 + index as u32),
    )
}

fn representative_result(detail: SearchDetail) -> SearchPapersResult {
    let mut hits = (1..=10).map(representative_hit).collect::<Vec<_>>();
    for hit in &mut hits {
        enrich_hit_identity(hit);
        enrich_match(hit, "context efficient paper retrieval");
        apply_detail(hit, detail, None);
    }

    SearchPapersResult {
        query: "context efficient paper retrieval".into(),
        total_count: 10,
        offset: 0,
        limit: 10,
        has_more: false,
        next_offset: None,
        detail: (detail != SearchDetail::Compact).then_some(detail),
        hits,
        diagnostics: Some(SearchDiagnostics {
            sources_ok: vec!["arxiv".into()],
            sources_skipped: vec![SourceDiagnostic {
                source: "semantic_scholar".into(),
                reason: "missing_api_key".into(),
            }],
            sources_failed: Vec::new(),
        }),
    }
}

#[test]
fn compact_search_payload_is_projected_and_bounded() {
    let unprojected = serde_json::to_string(&representative_result(SearchDetail::Compact)).unwrap();
    let compact =
        serde_json::to_string(&representative_result(SearchDetail::Compact).agent_output())
            .unwrap();
    let full = serde_json::to_string(&representative_result(SearchDetail::Full)).unwrap();
    let compact_value: serde_json::Value = serde_json::from_str(&compact).unwrap();

    let hit = &compact_value["hits"][0];
    for redundant_field in [
        "doi",
        "arxiv_id",
        "pmid",
        "abstract",
        "url",
        "pdf_url",
        "oa_pdf_url",
        "venue",
        "citation_count",
        "cache",
        "relevance_score",
    ] {
        assert!(
            hit.get(redundant_field).is_none(),
            "compact hit retained redundant field {redundant_field}"
        );
    }
    assert!(hit["hit_id"].is_string());
    assert!(hit["ids"]["doi"].is_string());
    assert!(hit["access"].is_object());
    assert!(hit["next"].is_array());
    assert!(compact_value.get("detail").is_none());

    let compact_tokens = estimated_tokens(&compact);
    let unprojected_tokens = estimated_tokens(&unprojected);
    let full_tokens = estimated_tokens(&full);
    println!(
        "search_papers unprojected compact: {} bytes, ~{} tokens; projected compact: {} bytes, ~{} tokens; full: {} bytes, ~{} tokens ({TOKEN_ESTIMATOR})",
        unprojected.len(),
        unprojected_tokens,
        compact.len(),
        compact_tokens,
        full.len(),
        full_tokens,
    );

    assert!(compact.len() < full.len());
    assert!(compact.len() < unprojected.len());
    assert!(
        compact_tokens <= 2_000,
        "default ten-hit compact search payload exceeded the 2k-token guardrail: {compact_tokens}"
    );
}
