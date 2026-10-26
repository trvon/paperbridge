#![no_main]
use libfuzzer_sys::fuzz_target;
use paperbridge::chunking::split_for_tts;
use paperbridge::internals::split_technical_content;

fuzz_target!(|input: (u16, &str)| {
    let (max, text) = input;
    let max = usize::from(max % 4_096);
    let tts = split_for_tts(text, max);
    let technical = split_technical_content(text, max, Some("Paper > Section"));
    for chunk in tts.iter().chain(&technical) {
        assert!(!chunk.trim().is_empty());
        assert!(chunk.chars().count() <= max.max(1), "chunk exceeds {max}");
    }
});
