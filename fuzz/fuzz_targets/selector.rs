#![no_main]
use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

fn root() -> &'static serde_json::Value {
    static ROOT: OnceLock<serde_json::Value> = OnceLock::new();
    ROOT.get_or_init(|| {
        serde_json::json!({
            "title": "Paper",
            "sections": [
                {"id": "s0", "heading": "Intro", "paragraphs": ["a", "b"]},
                {"id": "s1", "heading": "Method", "children": [{"id": "s1.1"}]}
            ],
            "references": [{"doi": "10.1/x"}],
        })
    })
}

fuzz_target!(|selector: &str| {
    let _ = paperbridge::paper::selector::evaluate(root(), selector);
});
