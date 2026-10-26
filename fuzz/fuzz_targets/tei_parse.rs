#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|xml: &str| {
    if let Ok(structure) = paperbridge::paper::tei::parse_tei("ITEM", "ATTACH", xml) {
        let _ = serde_json::to_string(&structure);
    }
});
