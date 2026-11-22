//! Read-through citation commands against mocked OpenAlex and Semantic
//! Scholar (no live calls).

use paperbridge::citations::{
    CallBudget, CitationConfig, CitationDirection, CitationService, CitationsRequest, RateLimit,
};
use paperbridge::models::CountKind;
use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FAST: RateLimit = RateLimit {
    per_second: 1000.0,
    burst: 100,
    daily: 10_000,
};
const S2_ID: &str = "00000000000000000000000000000000000000aa";

fn service(
    server: &MockServer,
    dir: &tempfile::TempDir,
    s2_key: bool,
    budget: CallBudget,
) -> CitationService {
    let mut cfg = CitationConfig::new(dir.path().join("citegraph.sqlite"));
    cfg.openalex_base = format!("{}/oa", server.uri());
    cfg.semantic_scholar_base = format!("{}/s2", server.uri());
    cfg.semantic_scholar_api_key = s2_key.then(|| "test-key".to_string());
    cfg.rates = (FAST, FAST);
    cfg.budget = budget;
    CitationService::new(cfg).unwrap()
}

fn req(id: &str, direction: CitationDirection, limit: u32, offset: u32) -> CitationsRequest {
    CitationsRequest {
        id: id.into(),
        direction,
        limit: Some(limit),
        offset: Some(offset),
        refresh: false,
        include_contexts: false,
    }
}

fn work(n: u32, doi: Option<&str>) -> Value {
    json!({
        "id": format!("https://openalex.org/W{n}"),
        "doi": doi.map(|d| format!("https://doi.org/{d}")),
        "display_name": format!("Work {n}"),
        "publication_year": 2020,
        "authorships": [{"author": {"display_name": "A. Author"}}],
    })
}

fn seed() -> Value {
    let mut w = work(1, Some("10.5555/seed"));
    w["referenced_works"] = json!(["https://openalex.org/W2", "https://openalex.org/W3"]);
    w["related_works"] = json!(["https://openalex.org/W9"]);
    w
}

async fn mount_seed(server: &MockServer, times: u64) {
    Mock::given(method("GET"))
        .and(path("/oa/works/doi:10.5555/seed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(seed()))
        .expect(times)
        .mount(server)
        .await;
}

#[tokio::test]
async fn refs_are_fetched_once_hydrated_and_served_from_cache() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    mount_seed(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/oa/works"))
        .and(query_param("filter", "openalex:W2|W3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [work(2, Some("10.5555/two")), work(3, None)]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let svc = service(&server, &dir, false, CallBudget::default());

    let out = svc
        .get_citations(req("doi:10.5555/seed", CitationDirection::Refs, 10, 0))
        .await
        .unwrap();
    assert_eq!(out.hit_id, "doi:10.5555/seed");
    assert_eq!(out.count_kind, CountKind::Exact);
    assert_eq!(out.total_count, Some(2));
    assert!(!out.truncated && !out.has_more);
    let ids: Vec<_> = out
        .hits
        .iter()
        .map(|h| h.hit.hit_id.clone().unwrap())
        .collect();
    assert_eq!(ids, vec!["doi:10.5555/two", "url:https://openalex.org/W3"]);
    assert_eq!(out.hits[0].hit.title, "Work 2");
    assert_eq!(out.hits[0].why, vec!["cited by subject".to_string()]);
    assert_eq!(out.hits[0].edge_sources[0].source, "openalex");
    let json = serde_json::to_value(&out).unwrap();
    assert!(
        json["hits"][0].get("doi").is_none(),
        "compact hit leaks doi"
    );
    assert_eq!(json["diagnostics"]["budget"][0]["used"], 2);

    // Cached: the mocks' expect(1) fails the test on a second fetch.
    let again = svc
        .get_citations(req("10.5555/seed", CitationDirection::Refs, 1, 1))
        .await
        .unwrap();
    assert_eq!(again.hits.len(), 1);
    assert!(again.diagnostics.budget.is_empty());

    // The subject's OpenAlex id round-trips as an input hit_id.
    let by_url = svc
        .get_citations(req(
            "url:https://openalex.org/W3",
            CitationDirection::CitedBy,
            1,
            0,
        ))
        .await;
    assert!(by_url.is_ok());
}

#[tokio::test]
async fn cited_by_resumes_from_committed_cursor() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    mount_seed(&server, 1).await;
    for (cursor, ids, next) in [
        ("*", vec![10, 11], Some("c2")),
        ("c2", vec![12, 13], Some("c3")),
        ("c3", vec![14], None),
    ] {
        Mock::given(method("GET"))
            .and(path("/oa/works"))
            .and(query_param("filter", "cites:W1"))
            .and(query_param("cursor", cursor))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "meta": {"count": 5, "next_cursor": next},
                "results": ids.iter().map(|n| work(*n, None)).collect::<Vec<_>>(),
            })))
            .expect(1)
            .mount(&server)
            .await;
    }
    let svc = service(&server, &dir, false, CallBudget::default());

    let first = svc
        .get_citations(req("doi:10.5555/seed", CitationDirection::CitedBy, 2, 0))
        .await
        .unwrap();
    assert_eq!(first.hits.len(), 2);
    assert!(first.has_more && first.truncated);
    assert_eq!(first.next_offset, Some(2));
    assert_eq!(first.count_kind, CountKind::Unknown);
    assert_eq!(first.coverage[0].total_reported, Some(5));
    assert_eq!(first.hits[0].why[0], "cites subject");

    // A new service (another process) resumes from the stored cursor.
    let svc2 = service(&server, &dir, false, CallBudget::default());
    let second = svc2
        .get_citations(req("doi:10.5555/seed", CitationDirection::CitedBy, 2, 2))
        .await
        .unwrap();
    assert_eq!(second.hits[0].hit.title, "Work 12");
    let third = svc2
        .get_citations(req("doi:10.5555/seed", CitationDirection::CitedBy, 2, 4))
        .await
        .unwrap();
    assert_eq!(third.hits.len(), 1);
    assert!(!third.has_more && !third.truncated);
    assert_eq!(third.count_kind, CountKind::Exact);
    assert_eq!(third.total_count, Some(5));
    assert!(third.coverage[0].complete);
}

#[tokio::test]
async fn budget_stops_cleanly_and_is_reported() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    mount_seed(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/oa/works"))
        .and(query_param("cursor", "*"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "meta": {"count": 50, "next_cursor": "c2"},
            "results": [work(10, None), work(11, None)],
        })))
        .expect(1)
        .mount(&server)
        .await;
    let svc = service(
        &server,
        &dir,
        false,
        CallBudget {
            openalex: 2,
            semantic_scholar: 0,
        },
    );
    let out = svc
        .get_citations(req("doi:10.5555/seed", CitationDirection::CitedBy, 5, 0))
        .await
        .unwrap();
    assert_eq!(out.hits.len(), 2);
    assert!(out.truncated && out.hits.iter().all(|h| h.truncated));
    let oa = &out.diagnostics.budget[0];
    assert_eq!(
        (oa.source.as_str(), oa.used, oa.exhausted),
        ("openalex", 2, true)
    );
    assert!(
        out.diagnostics
            .notes
            .contains(&"openalex_budget_exhausted".to_string())
    );
}

#[tokio::test]
async fn semantic_scholar_fallback_stops_at_offset_cap() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("GET"))
        .and(path("/oa/works/doi:10.5555/seed"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/s2/paper/DOI:10.5555/seed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "paperId": S2_ID, "externalIds": {"DOI": "10.5555/seed"}, "title": "Seed"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/s2/paper/{S2_ID}/citations")))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offset": 0,
            "next": 10000,
            "data": [
                {"intents": ["methodology"], "isInfluential": true, "contexts": ["we follow [4]"],
                 "citingPaper": {"paperId": "00000000000000000000000000000000000000bb",
                                 "externalIds": {"DOI": "10.5555/citer"}, "title": "Citer", "year": 2024}},
                {"citingPaper": {"paperId": null, "title": "Unindexed"}}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let svc = service(&server, &dir, true, CallBudget::default());
    let mut request = req("doi:10.5555/seed", CitationDirection::CitedBy, 10, 0);
    request.include_contexts = true;
    let out = svc.get_citations(request.clone()).await.unwrap();
    assert_eq!(out.diagnostics.sources_failed[0].source, "openalex");
    assert_eq!(out.diagnostics.sources_failed[0].reason, "not_found");
    assert_eq!(out.hits.len(), 1);
    let hit = &out.hits[0];
    assert_eq!(hit.hit.hit_id.as_deref(), Some("doi:10.5555/citer"));
    assert_eq!(hit.edge_sources[0].source, "semantic_scholar");
    assert_eq!(hit.edge_sources[0].intents, vec!["methodology".to_string()]);
    assert_eq!(hit.contexts[0].text, "we follow [4]");
    assert!(hit.why.iter().any(|w| w.contains("influential")));
    let cov = &out.coverage[0];
    assert!(cov.capped && !cov.complete);
    assert!(out.truncated && !out.has_more);
    assert!(
        out.diagnostics
            .notes
            .contains(&"semantic_scholar_offset_cap".to_string())
    );

    // Capped: a repeat call does not page past the cap.
    let again = svc.get_citations(request).await.unwrap();
    assert_eq!(again.hits.len(), 1);
}

#[tokio::test]
async fn missing_key_skips_semantic_scholar_and_related_uses_openalex() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    mount_seed(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/oa/works"))
        .and(query_param("filter", "openalex:W9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [work(9, None)]})))
        .mount(&server)
        .await;
    let svc = service(&server, &dir, false, CallBudget::default());
    let out = svc
        .get_citations(req("doi:10.5555/seed", CitationDirection::Related, 10, 0))
        .await
        .unwrap();
    assert_eq!(out.hits.len(), 1);
    assert_eq!(out.hits[0].why[0], "related to subject");

    let mut request = req("doi:10.5555/nowhere", CitationDirection::Refs, 10, 0);
    request.include_contexts = true;
    let out = svc.get_citations(request).await.unwrap();
    assert!(out.hits.is_empty());
    assert_eq!(
        out.diagnostics.sources_skipped[0].source,
        "semantic_scholar"
    );
    assert_eq!(out.diagnostics.sources_skipped[0].reason, "missing_api_key");
}

#[tokio::test]
async fn expired_cursor_restarts_in_a_new_generation() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    mount_seed(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/oa/works"))
        .and(query_param("cursor", "*"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "meta": {"count": 3, "next_cursor": "stale"},
            "results": [work(10, None)],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/oa/works"))
        .and(query_param("cursor", "stale"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    let svc = service(&server, &dir, false, CallBudget::default());
    svc.get_citations(req("doi:10.5555/seed", CitationDirection::CitedBy, 1, 0))
        .await
        .unwrap();
    let out = svc
        .get_citations(req("doi:10.5555/seed", CitationDirection::CitedBy, 1, 1))
        .await
        .unwrap();
    assert!(
        out.diagnostics
            .notes
            .contains(&"openalex_cursor_restarted".to_string())
    );
    assert_eq!(out.coverage[0].generation, 2);
}

#[tokio::test]
async fn unsupported_ids_fail_with_guidance() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let svc = service(&server, &dir, false, CallBudget::default());
    let err = svc
        .get_citations(req("zotero:ABCD1234", CitationDirection::Refs, 10, 0))
        .await
        .unwrap_err();
    let envelope = paperbridge::error::ErrorEnvelope::from_error(&err);
    assert_eq!(envelope.error, "invalid_input");
    assert!(
        envelope
            .suggestions
            .iter()
            .any(|s| s.contains("papers refs"))
    );
    // Nothing was opened or created for an invalid request.
    assert!(!dir.path().join("citegraph.sqlite").exists());
}

/// Semantic Scholar folds preprint and published versions into one paper;
/// the store keeps them as two linked works, and an arXiv subject still
/// reads the edges fetched under the published version.
#[tokio::test]
async fn arxiv_subject_reads_edges_across_versions() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("GET"))
        .and(path("/oa/works/doi:10.48550/arxiv.1706.03762"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/s2/paper/ARXIV:1706.03762"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "paperId": S2_ID,
            "externalIds": {"DOI": "10.5555/pub", "ArXiv": "1706.03762"},
            "title": "Attention"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/s2/paper/{S2_ID}/references")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offset": 0,
            "data": [{"contexts": ["as in [1]"], "citedPaper": {
                "paperId": "00000000000000000000000000000000000000cc",
                "externalIds": {"DOI": "10.5555/ref"}, "title": "Ref"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let svc = service(&server, &dir, true, CallBudget::default());
    for _ in 0..2 {
        let mut request = req("arxiv:1706.03762", CitationDirection::Refs, 10, 0);
        request.include_contexts = true;
        let out = svc.get_citations(request).await.unwrap();
        assert_eq!(out.hit_id, "arxiv:1706.03762");
        assert_eq!(out.hits.len(), 1, "{out:?}");
        assert_eq!(out.hits[0].contexts[0].text, "as in [1]");
        assert_eq!(out.coverage.len(), 1);
        assert!(out.coverage[0].complete);
    }
}

/// A 429 then 200: the retry pays for its own token and per-call budget.
#[tokio::test]
async fn throttled_retry_consumes_budget() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("GET"))
        .and(path("/oa/works/doi:10.5555/seed"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    mount_seed(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/oa/works"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .mount(&server)
        .await;
    let svc = service(&server, &dir, false, CallBudget::default());
    let out = svc
        .get_citations(req("doi:10.5555/seed", CitationDirection::Refs, 10, 0))
        .await
        .unwrap();
    assert_eq!(out.hits.len(), 2);
    // 429 + retry + hydration.
    assert_eq!(out.diagnostics.budget[0].used, 3);
    assert!(
        out.diagnostics
            .notes
            .contains(&"openalex_throttled_retry".to_string())
    );
}

/// The daily budget lives in the store, so a second process (service)
/// sees the first one's spend.
#[tokio::test]
async fn daily_budget_is_shared_across_processes() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    mount_seed(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/oa/works"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .mount(&server)
        .await;
    let tight = RateLimit { daily: 2, ..FAST };
    let mk = || {
        let mut cfg = CitationConfig::new(dir.path().join("citegraph.sqlite"));
        cfg.openalex_base = format!("{}/oa", server.uri());
        cfg.rates = (tight, FAST);
        CitationService::new(cfg).unwrap()
    };
    let first = mk()
        .get_citations(req("doi:10.5555/seed", CitationDirection::Refs, 10, 0))
        .await
        .unwrap();
    assert_eq!(first.hits.len(), 2);
    let second = mk()
        .get_citations(req("doi:10.5555/other", CitationDirection::Refs, 10, 0))
        .await
        .unwrap();
    assert!(second.hits.is_empty());
    assert!(
        second
            .diagnostics
            .notes
            .contains(&"openalex_daily_budget_exhausted".to_string())
    );
    assert!(
        second.diagnostics.sources_failed.is_empty(),
        "budget is not a failure"
    );
}

/// An empty OpenAlex reference list is not final: Semantic Scholar is
/// asked, and only its positive answer completes the list.
#[tokio::test]
async fn empty_openalex_refs_fall_back_to_semantic_scholar() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("GET"))
        .and(path("/oa/works/doi:10.5555/seed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(work(1, Some("10.5555/seed"))))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/s2/paper/DOI:10.5555/seed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "paperId": S2_ID, "externalIds": {"DOI": "10.5555/seed"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/s2/paper/{S2_ID}/references")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "offset": 0,
            "data": [{"citedPaper": {"paperId": "00000000000000000000000000000000000000dd",
                                     "externalIds": {"DOI": "10.5555/r"}, "title": "R"}}]
        })))
        .mount(&server)
        .await;
    let svc = service(&server, &dir, true, CallBudget::default());
    let out = svc
        .get_citations(req("doi:10.5555/seed", CitationDirection::Refs, 10, 0))
        .await
        .unwrap();
    assert!(
        out.diagnostics
            .notes
            .contains(&"openalex_refs_empty".to_string())
    );
    assert_eq!(out.hits.len(), 1);
    assert_eq!(out.coverage.len(), 1);
    assert_eq!(out.coverage[0].source, "semantic_scholar");
    assert!(out.coverage[0].complete);
}

/// A DOI the canonical form misses (it really ends in `.`) is retried in
/// its legacy form.
#[tokio::test]
async fn legacy_doi_form_is_retried_on_not_found() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("GET"))
        .and(path("/oa/works/doi:10.5555/seed"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let mut w = seed();
    w["doi"] = json!("https://doi.org/10.5555/seed.");
    Mock::given(method("GET"))
        .and(path("/oa/works/doi:10.5555/seed."))
        .respond_with(ResponseTemplate::new(200).set_body_json(w))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/oa/works"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .mount(&server)
        .await;
    let svc = service(&server, &dir, false, CallBudget::default());
    let out = svc
        .get_citations(req("doi:10.5555/seed.", CitationDirection::Refs, 10, 0))
        .await
        .unwrap();
    assert_eq!(out.hits.len(), 2);
}

/// Rows that collapse into one neighbor (shared DOI) do not short a page:
/// fetching continues until enough distinct neighbors are cached, and
/// next_offset counts the hits actually returned.
#[tokio::test]
async fn pagination_counts_distinct_neighbors() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    mount_seed(&server, 1).await;
    for (cursor, rows, next) in [
        (
            "*",
            vec![
                work(10, Some("10.5555/same")),
                work(11, Some("10.5555/same")),
            ],
            Some("c2"),
        ),
        ("c2", vec![work(12, None), work(13, None)], Some("c3")),
    ] {
        Mock::given(method("GET"))
            .and(path("/oa/works"))
            .and(query_param("filter", "cites:W1"))
            .and(query_param("cursor", cursor))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "meta": {"count": 9, "next_cursor": next}, "results": rows,
            })))
            .expect(1)
            .mount(&server)
            .await;
    }
    let svc = service(&server, &dir, false, CallBudget::default());
    let out = svc
        .get_citations(req("doi:10.5555/seed", CitationDirection::CitedBy, 2, 0))
        .await
        .unwrap();
    assert_eq!(out.hits.len(), 2);
    assert_eq!(out.next_offset, Some(2));
}

/// The configured OpenAlex key is sent on citation requests and never
/// appears in diagnostics.
#[tokio::test]
async fn openalex_api_key_is_sent_and_redacted() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(method("GET"))
        .and(path("/oa/works/doi:10.5555/seed"))
        .and(query_param("api_key", "oa-secret-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(seed()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/oa/works"))
        .and(query_param("api_key", "oa-secret-key"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let mut cfg = CitationConfig::new(dir.path().join("citegraph.sqlite"));
    cfg.openalex_base = format!("{}/oa", server.uri());
    cfg.openalex_api_key = Some("oa-secret-key".into());
    cfg.rates = (FAST, FAST);
    let svc = CitationService::new(cfg).unwrap();
    let out = svc
        .get_citations(req("doi:10.5555/seed", CitationDirection::Refs, 10, 0))
        .await
        .unwrap();
    assert_eq!(out.hits.len(), 2);
    let json = serde_json::to_string(&out).unwrap();
    assert!(!json.contains("oa-secret-key"));
}
