use paperbridge::config::{BackendModeConfig, Config, LibraryType};
use paperbridge::models::{ListCollectionsQuery, SearchItemsQuery};
use paperbridge::service::{
    PaperbridgeService, PrepareItemForVoxRequest, PrepareSearchResultForVoxRequest,
};
use paperbridge::zotero_api::build_backend;
use serde::Serialize;
use std::path::PathBuf;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture_path(name: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let direct = manifest_dir.join("tests").join("golden").join(name);
    if direct.exists() {
        return direct;
    }

    let nested = manifest_dir
        .join("paperbridge")
        .join("tests")
        .join("golden")
        .join(name);
    if nested.exists() {
        return nested;
    }

    direct
}

fn assert_golden<T: Serialize>(fixture_name: &str, value: &T) {
    let path = fixture_path(fixture_name);
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "failed to read fixture {fixture_name} at {}: {e}",
            path.display()
        )
    });
    let actual = serde_json::to_string_pretty(value)
        .unwrap_or_else(|e| panic!("failed to serialize for fixture {fixture_name}: {e}"));
    assert_eq!(
        expected.trim_end(),
        actual.trim_end(),
        "fixture mismatch for {fixture_name}"
    );
}

fn test_config(api_base: String) -> Config {
    Config {
        backend_mode: BackendModeConfig::Cloud,
        cloud_api_base: api_base,
        local_api_base: "http://127.0.0.1:23119/api".to_string(),
        user_id: Some(123),
        library_type: LibraryType::User,
        ..Config::default()
    }
}

async fn build_service_with_mocks() -> PaperbridgeService {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/users/123/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "key": "ITEMA",
                "data": {
                    "itemType": "journalArticle",
                    "title": "Graph Learning at Scale",
                    "date": "2024-08-01",
                    "creators": [{"firstName": "Grace", "lastName": "Hopper"}],
                    "url": "https://example.org/graph"
                }
            }
        ])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/users/123/collections/top"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "key": "COLL1",
                "data": {
                    "name": "Research",
                    "parentCollection": null
                },
                "meta": {
                    "numItems": 7
                }
            }
        ])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/users/123/items/ITEMA"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "key": "ITEMA",
            "data": {
                "itemType": "journalArticle",
                "title": "Graph Learning at Scale",
                "date": "2024-08-01",
                "abstractNote": "A practical systems paper.",
                "creators": [{"firstName": "Grace", "lastName": "Hopper"}],
                "url": "https://example.org/graph"
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/users/123/items/ITEMA/children"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "key": "NOTE1",
                "data": {
                    "itemType": "attachment",
                    "title": "Notes",
                    "contentType": "text/plain"
                }
            },
            {
                "key": "PDFA",
                "data": {
                    "itemType": "attachment",
                    "title": "Paper PDF",
                    "contentType": "application/pdf",
                    "path": "storage:paper.pdf"
                }
            }
        ])))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/users/123/items/PDFA/fulltext"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "content": "First sentence. Second sentence. Third sentence.",
            "indexedPages": 3,
            "totalPages": 3,
            "indexedChars": 42,
            "totalChars": 42
        })))
        .mount(&server)
        .await;

    let backend = build_backend(test_config(server.uri())).unwrap();
    PaperbridgeService::new(backend)
}

#[tokio::test]
async fn golden_outputs_match_expected_json() {
    let service = build_service_with_mocks().await;

    let search_results = service
        .search_items(SearchItemsQuery {
            q: Some("graph learning".to_string()),
            ..SearchItemsQuery::default()
        })
        .await
        .unwrap();
    assert_golden("search_items.json", &search_results);

    let collections = service
        .list_collections(ListCollectionsQuery {
            top_only: true,
            ..ListCollectionsQuery::default()
        })
        .await
        .unwrap();
    assert_golden("list_collections.json", &collections);

    let item = service.get_item("ITEMA").await.unwrap();
    assert_golden("get_item.json", &item);

    let fulltext = service.get_item_fulltext("PDFA").await.unwrap();
    assert_golden("get_item_fulltext.json", &fulltext);

    let prepared_item = service
        .prepare_item_for_vox(PrepareItemForVoxRequest {
            item_key: "ITEMA".to_string(),
            attachment_key: None,
            max_chars_per_chunk: Some(25),
        })
        .await
        .unwrap();
    assert_golden("prepare_item_for_vox.json", &prepared_item);

    let prepared_search = service
        .prepare_search_result_for_vox(PrepareSearchResultForVoxRequest {
            q: "graph learning".to_string(),
            qmode: None,
            item_type: None,
            tag: None,
            result_index: Some(0),
            search_limit: Some(5),
            max_chars_per_chunk: Some(25),
        })
        .await
        .unwrap();
    assert_golden("prepare_search_result_for_vox.json", &prepared_search);
}
