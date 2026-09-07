//! Exercise the advertised MCP surface, not only the underlying service DTOs.
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Client {
    child: Child,
    input: ChildStdin,
    output: Receiver<String>,
    id: u64,
}

impl Client {
    fn start(config: &std::path::Path, profile: &str) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_paperbridge"));
        for (key, _) in std::env::vars() {
            if key.starts_with("PAPERBRIDGE_") || key.starts_with("ZOTERO_MCP_") {
                command.env_remove(key);
            }
        }
        let mut child = command
            .args(["serve", "--profile", profile])
            .env("PAPERBRIDGE_CONFIG", config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, output) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            input,
            output,
            id: 0,
        };
        let init = client.rpc("initialize", json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"contract-test","version":"1"}}));
        assert_eq!(init["result"]["serverInfo"]["name"], "paperbridge");
        assert_eq!(
            init["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        writeln!(
            client.input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        client.input.flush().unwrap();
        client
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(
            self.input,
            "{}",
            json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params})
        )
        .unwrap();
        self.input.flush().unwrap();
        let response = self
            .output
            .recv_timeout(Duration::from_secs(20))
            .expect("MCP response within deadline");
        serde_json::from_str(&response).unwrap()
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.rpc("tools/call", json!({"name":name,"arguments":arguments}))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_contract_metadata_errors_pagination_and_profiles() {
    let upstream = MockServer::start().await;
    Mock::given(method("GET")).and(path("/users/123/items/ITEM1234"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key":"ITEM1234","version":7,"data":{"itemType":"journalArticle","title":"Fixture","DOI":"10.5555/fixture","publicationTitle":"Fixture Journal","creators":[]}})))
        .mount(&upstream).await;
    Mock::given(method("GET")).and(path("/users/123/items/ITEM1234/children"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"key":"ATTACH12","data":{"itemType":"attachment","title":"PDF","contentType":"application/pdf"}}])))
        .mount(&upstream).await;
    Mock::given(method("GET"))
        .and(path("/users/123/items/ATTACH12/fulltext"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"content":format!("éabc{}", "long body ".repeat(12_000))})),
        )
        .mount(&upstream)
        .await;
    Mock::given(method("GET"))
        .and(path("/users/123/items/MISSING1"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_string(format!("fixture-secret {}", "x".repeat(100_000))),
        )
        .mount(&upstream)
        .await;
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.toml");
    std::fs::write(&config, format!("backend_mode='cloud'\ncloud_api_base='{}'\nuser_id=123\napi_key='fixture-secret'\npaperseed_enabled=false\npaperseed_yams_enabled=false\ngrobid_auto_spawn=false\n", upstream.uri())).unwrap();
    let mut client = Client::start(&config, "full");
    let manifest = client.rpc("tools/list", json!({}));
    let tools = manifest["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 23);
    for tool in tools {
        assert!(tool["outputSchema"].is_object(), "{}", tool["name"]);
        assert!(tool["annotations"].is_object());
    }
    let response = client.call("get_item", json!({"key":"ITEM1234"}));
    let result = &response["result"];
    let data = &result["structuredContent"];
    assert_eq!(data["doi"], "10.5555/fixture");
    assert_eq!(data["venue"], "Fixture Journal");
    assert_eq!(data["version"], 7);
    assert_eq!(
        serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
        *data
    );
    let schema = &tools
        .iter()
        .find(|tool| tool["name"] == "get_item")
        .unwrap()["outputSchema"];
    assert!(jsonschema::validator_for(schema).unwrap().is_valid(data));

    let response = client.call("get_item", json!({"key":"MISSING1"}));
    assert_eq!(response["result"]["isError"], true);
    let encoded = response.to_string();
    assert!(encoded.len() < 16_384);
    assert!(!encoded.contains("fixture-secret"));
    assert!(response["result"]["structuredContent"]["recovery"].is_array());

    let response = client.call(
        "open_paper",
        json!({"item_key":"ITEM1234","want":["chunks"],"max_chars":4}),
    );
    let data = &response["result"]["structuredContent"];
    assert_eq!(data["chunks_page"]["next_offset"], 5);
    assert_eq!(data["chunks_page"]["truncated"], true);
    let response = client.call(
        "open_paper",
        json!({"item_key":"ITEM1234","want":["structure"],"max_chars":100}),
    );
    assert!(response["result"]["structuredContent"]["structure_page"].is_object());
    assert!(response.to_string().len() < 16_384);
    let structure_only = client.call(
        "open_paper",
        json!({"item_key":"ITEM1234","want":["structure"],"selector":"metadata"}),
    );
    let combined = client.call(
        "open_paper",
        json!({"item_key":"ITEM1234","want":["fulltext","structure"],"selector":"metadata"}),
    );
    assert_eq!(
        structure_only["result"]["structuredContent"]["structure"],
        combined["result"]["structuredContent"]["structure"],
        "adding fulltext must preserve Zotero citation metadata"
    );
    let invalid = client.call("open_paper", json!({"item_key":"ITEM1234","want":["typo"]}));
    assert_eq!(invalid["error"]["code"], -32602);
    drop(client);

    let mut core = Client::start(&config, "core");
    let manifest = core.rpc("tools/list", json!({}));
    assert_eq!(manifest["result"]["tools"].as_array().unwrap().len(), 6);
    let forbidden = core.call(
        "delete_item",
        json!({"item":{"key":"ITEM1234","version":7}}),
    );
    assert!(forbidden["error"].is_object());
}
