/// Normalize whitespace and blank lines for predictable TTS chunking.
pub fn normalize_text_for_tts(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_was_ws = false;

    for ch in input.chars() {
        if ch.is_whitespace() {
            if !prev_was_ws {
                out.push(' ');
                prev_was_ws = true;
            }
        } else {
            out.push(ch);
            prev_was_ws = false;
        }
    }

    out.trim().to_string()
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// Split long text into chunks that are suitable for speech synthesis.
///
/// Strategy:
/// 1) Split by sentence boundaries.
/// 2) Pack sentences into chunks up to `max_chars` characters.
/// 3) If one sentence exceeds max, hard-split it (words, then characters).
///
/// Every chunk is at most `max(max_chars, 1)` characters.
pub fn split_for_tts(input: &str, max_chars: usize) -> Vec<String> {
    let text = normalize_text_for_tts(input);
    if text.is_empty() {
        return Vec::new();
    }

    let safe_max = max_chars.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;

    for sentence in sentence_split(&text) {
        let sentence_len = char_len(sentence);
        if sentence_len > safe_max {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_len = 0;
            }
            chunks.extend(hard_split(sentence, safe_max));
            continue;
        }

        if current.is_empty() {
            current.push_str(sentence);
            current_len = sentence_len;
        } else if current_len + 1 + sentence_len <= safe_max {
            current.push(' ');
            current.push_str(sentence);
            current_len += 1 + sentence_len;
        } else {
            chunks.push(std::mem::replace(&mut current, sentence.to_string()));
            current_len = sentence_len;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

fn sentence_split(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;

    for (idx, ch) in text.char_indices() {
        if matches!(ch, '.' | '!' | '?' | ';') {
            let end = idx + ch.len_utf8();
            let sentence = text[start..end].trim();
            if !sentence.is_empty() {
                out.push(sentence);
            }
            start = end;
        }
    }

    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }

    out
}

/// Split `text` into pieces of at most `max_chars` characters, cutting only
/// on char boundaries. Whitespace is kept verbatim.
fn char_windows(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let mut out = Vec::new();
    let mut current = String::new();
    let mut len = 0;
    for ch in text.chars() {
        if len == max_chars {
            out.push(std::mem::take(&mut current));
            len = 0;
        }
        current.push(ch);
        len += 1;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Pack whitespace-separated words into pieces of at most `max_chars`
/// characters; words longer than the limit are split by character.
fn hard_split(sentence: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;

    for word in sentence.split_whitespace() {
        let word_len = char_len(word);
        if word_len > max_chars {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            let mut pieces = char_windows(word, max_chars);
            current = pieces.pop().unwrap_or_default();
            current_len = char_len(&current);
            out.extend(pieces);
            continue;
        }

        if current.is_empty() {
            current.push_str(word);
            current_len = word_len;
        } else if current_len + 1 + word_len <= max_chars {
            current.push(' ');
            current.push_str(word);
            current_len += 1 + word_len;
        } else {
            out.push(std::mem::replace(&mut current, word.to_string()));
            current_len = word_len;
        }
    }

    if !current.is_empty() {
        out.push(current);
    }

    out
}

fn is_fence_line(line: &str) -> bool {
    line.trim().starts_with("```")
}

/// Split technical text (such as book chapters or code-heavy documentation)
/// while preserving markdown fenced code blocks and optional breadcrumbs.
///
/// Every chunk is at most `max(max_chars, 1)` characters, breadcrumb
/// included. A breadcrumb is truncated so it uses at most half the budget,
/// and is omitted when the budget cannot hold it. Unterminated fences are
/// closed, and a code block whose fences alone exceed the budget is emitted
/// without fences.
pub(crate) fn split_technical_content(
    input: &str,
    max_chars: usize,
    breadcrumb: Option<&str>,
) -> Vec<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let max_chars = max_chars.max(1);
    let prefix = breadcrumb_prefix(breadcrumb, max_chars);
    let target_max = max_chars - char_len(&prefix);

    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;

    for block in segment_markdown_blocks(trimmed) {
        let block_len = char_len(&block);
        if block_len > target_max {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
                current_len = 0;
            }
            if block.lines().next().is_some_and(is_fence_line) {
                parts.extend(split_code_block(&block, target_max));
            } else {
                parts.extend(line_split_block(&block, target_max));
            }
            continue;
        }

        if current.is_empty() {
            current.push_str(&block);
            current_len = block_len;
        } else if current_len + 2 + block_len <= target_max {
            current.push_str("\n\n");
            current.push_str(&block);
            current_len += 2 + block_len;
        } else {
            parts.push(std::mem::replace(&mut current, block));
            current_len = block_len;
        }
    }

    if !current.is_empty() {
        parts.push(current);
    }

    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .map(|part| format!("{prefix}{part}"))
        .collect()
}

/// Breadcrumb plus blank line, truncated to at most half of `max_chars`.
/// Empty when there is no room for at least one breadcrumb character.
fn breadcrumb_prefix(breadcrumb: Option<&str>, max_chars: usize) -> String {
    let Some(breadcrumb) = breadcrumb.map(str::trim).filter(|b| !b.is_empty()) else {
        return String::new();
    };
    let budget = (max_chars / 2).saturating_sub(2);
    let crumb: String = breadcrumb.chars().take(budget).collect();
    let crumb = crumb.trim_end();
    if crumb.is_empty() {
        String::new()
    } else {
        format!("{crumb}\n\n")
    }
}

fn segment_markdown_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current_block = String::new();
    let mut in_code_block = false;

    for line in text.lines() {
        if is_fence_line(line) {
            if !in_code_block && !current_block.trim().is_empty() {
                // A fence directly after prose starts a new block.
                blocks.push(current_block.trim_end().to_string());
                current_block.clear();
            }
            in_code_block = !in_code_block;
            current_block.push_str(line);
            current_block.push('\n');
            if !in_code_block {
                blocks.push(current_block.trim_end().to_string());
                current_block.clear();
            }
            continue;
        }

        if in_code_block {
            current_block.push_str(line);
            current_block.push('\n');
            continue;
        }

        if line.trim().is_empty() {
            if !current_block.trim().is_empty() {
                blocks.push(current_block.trim_end().to_string());
            }
            current_block.clear();
        } else {
            if !current_block.is_empty() {
                current_block.push('\n');
            }
            current_block.push_str(line);
        }
    }

    if in_code_block {
        current_block.push_str("```");
    }
    if !current_block.trim().is_empty() {
        blocks.push(current_block.trim_end().to_string());
    }

    blocks
}

fn line_split_block(block: &str, max_chars: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut current_len = 0;

    for line in block.lines() {
        let line_len = char_len(line);
        if line_len > max_chars {
            if !current.is_empty() {
                parts.push(std::mem::take(&mut current));
                current_len = 0;
            }
            parts.extend(hard_split(line, max_chars));
            continue;
        }

        if current.is_empty() {
            current.push_str(line);
            current_len = line_len;
        } else if current_len + 1 + line_len <= max_chars {
            current.push('\n');
            current.push_str(line);
            current_len += 1 + line_len;
        } else {
            parts.push(std::mem::replace(&mut current, line.to_string()));
            current_len = line_len;
        }
    }

    if !current.is_empty() {
        parts.push(current);
    }

    parts
}

fn split_code_block(block: &str, target_max: usize) -> Vec<String> {
    let mut lines = block.lines();
    let Some(first_line) = lines.next() else {
        return Vec::new();
    };
    let fence_header = first_line.trim();
    let fence_close = "```";
    let body_lines: Vec<&str> = lines.collect();
    let body_lines = match body_lines.split_last() {
        Some((last, rest)) if is_fence_line(last) => rest,
        _ => &body_lines[..],
    };

    let overhead = char_len(fence_header) + 2 + fence_close.len();
    let Some(effective_max) = target_max.checked_sub(overhead).filter(|max| *max > 0) else {
        // The fences alone exceed the budget: emit the body unfenced.
        return line_split_block(&body_lines.join("\n"), target_max);
    };
    let fenced = |body: &str| format!("{fence_header}\n{body}\n{fence_close}");

    let mut parts = Vec::new();
    // `None` until the current part holds at least one (possibly empty) line.
    let mut current: Option<(String, usize)> = None;

    for &line in body_lines {
        let line_len = char_len(line);
        if line_len > effective_max {
            if let Some((body, _)) = current.take() {
                parts.push(fenced(&body));
            }
            for piece in char_windows(line, effective_max) {
                parts.push(fenced(&piece));
            }
            continue;
        }

        match current.as_mut() {
            Some((body, len)) if *len + 1 + line_len <= effective_max => {
                body.push('\n');
                body.push_str(line);
                *len += 1 + line_len;
            }
            Some(_) => {
                if let Some((body, _)) = current.replace((line.to_string(), line_len)) {
                    parts.push(fenced(&body));
                }
            }
            None => current = Some((line.to_string(), line_len)),
        }
    }

    if let Some((body, _)) = current
        && !body.trim().is_empty()
    {
        parts.push(fenced(&body));
    }

    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn normalize_collapses_whitespace() {
        let text = "a\n\n b\t\tc";
        assert_eq!(normalize_text_for_tts(text), "a b c");
    }

    #[test]
    fn split_prefers_sentence_boundaries() {
        let input = "One short sentence. Another short sentence. Last one.";
        let chunks = split_for_tts(input, 30);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], "One short sentence.");
    }

    #[test]
    fn split_hard_splits_very_long_sentence() {
        let input = "This sentence is intentionally very long and should be split into smaller pieces because it exceeds the maximum chunk size";
        let chunks = split_for_tts(input, 40);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= 40));
    }

    #[test]
    fn split_technical_content_preserves_code_blocks_and_breadcrumb() {
        let input = "\
Overview of data encoding.

```rust
struct Record {
    id: u64,
    name: String,
}
```

Summary of binary serialization.";

        let chunks = split_technical_content(input, 200, Some("[Designing Data > Chapter 4]"));
        assert!(!chunks.is_empty());
        assert!(chunks[0].starts_with("[Designing Data > Chapter 4]"));
        // Check code block is intact in one of the chunks
        let has_code = chunks.iter().any(|c| c.contains("```rust\nstruct Record"));
        assert!(has_code);
    }

    #[test]
    fn split_technical_content_splits_oversized_code_block_with_valid_fences() {
        let input = "\
```rust
let a = 1;
let b = 2;
let c = 3;
let d = 4;
let e = 5;
```";
        let chunks = split_technical_content(input, 35, None);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.starts_with("```rust"));
            assert!(chunk.ends_with("```"));
            assert!(chunk.chars().count() <= 35);
        }
    }

    #[test]
    fn regression_zero_max_chars_does_not_panic() {
        let chunks = split_technical_content("alpha beta", 0, Some("crumb"));
        assert!(chunks.iter().all(|c| c.chars().count() <= 1));
        assert!(!chunks.is_empty());
        let chunks = split_for_tts("alpha beta", 0);
        assert!(chunks.iter().all(|c| c.chars().count() <= 1));
    }

    #[test]
    fn regression_fence_after_prose_line_starts_new_block() {
        let input = "para\n```rust\nlet x = 1;\n```";
        let blocks = segment_markdown_blocks(input);
        assert_eq!(blocks, vec!["para", "```rust\nlet x = 1;\n```"]);
        let chunks = split_technical_content(input, 200, None);
        assert!(chunks.iter().all(|c| !c.contains("para```")));
    }

    #[test]
    fn regression_long_tokens_respect_max() {
        let url = format!("https://example.org/{}", "x".repeat(120));
        for chunk in split_for_tts(&url, 25) {
            assert!(chunk.chars().count() <= 25, "{chunk}");
        }
        let text = format!("see {url} now");
        for chunk in split_technical_content(&text, 25, None) {
            assert!(chunk.chars().count() <= 25, "{chunk}");
        }
    }

    #[test]
    fn regression_tts_measures_chars_not_bytes() {
        // 10 two-byte chars fit a 10-char budget.
        assert_eq!(split_for_tts("éééééééééé", 10), vec!["éééééééééé"]);
    }

    #[test]
    fn regression_unterminated_fence_is_closed() {
        let chunks = split_technical_content("```rust\nlet x = 1;", 200, None);
        assert_eq!(chunks, vec!["```rust\nlet x = 1;\n```"]);
    }

    #[test]
    fn regression_oversized_breadcrumb_is_truncated() {
        let crumb = "B".repeat(100);
        let chunks = split_technical_content("short body text", 40, Some(&crumb));
        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 40, "{chunk}");
            assert!(chunk.starts_with("BBBB"));
        }
    }

    #[test]
    fn regression_code_indentation_is_preserved_when_split() {
        let line = format!("    {}", "y".repeat(60));
        let input = format!("```\n{line}\n```");
        let chunks = split_technical_content(&input, 30, None);
        assert!(chunks[0].starts_with("```\n    y"));
    }

    fn non_ws(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    fn fence_count(chunk: &str) -> usize {
        chunk.lines().filter(|l| is_fence_line(l)).count()
    }

    fn strip_fence_lines(text: &str) -> String {
        text.lines()
            .filter(|l| !is_fence_line(l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Markdown-ish text whose only backticks are whole fence lines.
    fn technical_text() -> impl Strategy<Value = String> {
        let line = prop_oneof![
            6 => "[a-zA-Z0-9 ,.;:()é漢]{0,60}",
            1 => Just(String::new()),
            1 => Just("```".to_string()),
            1 => Just("```rust".to_string()),
            1 => "    [a-z{}();=]{0,80}",
        ];
        prop::collection::vec(line, 0..40).prop_map(|lines| lines.join("\n"))
    }

    proptest! {
        #[test]
        fn tts_chunks_are_bounded_and_lossless(input in "\\PC{0,400}", max in 0usize..120) {
            let chunks = split_for_tts(&input, max);
            for chunk in &chunks {
                prop_assert!(chunk.chars().count() <= max.max(1));
                prop_assert!(!chunk.is_empty());
                prop_assert_eq!(chunk.trim(), chunk.as_str());
            }
            prop_assert_eq!(non_ws(&chunks.concat()), non_ws(&input));
            prop_assert_eq!(&chunks, &split_for_tts(&input, max));
        }

        #[test]
        fn technical_chunks_never_panic_on_arbitrary_input(
            input in "\\PC{0,300}",
            max in 0usize..120,
            crumb in proptest::option::of("\\PC{0,40}"),
        ) {
            for chunk in split_technical_content(&input, max, crumb.as_deref()) {
                prop_assert!(chunk.chars().count() <= max.max(1));
            }
        }

        #[test]
        fn technical_chunks_hold_invariants(
            input in technical_text(),
            max in 1usize..300,
            crumb in proptest::option::of("[a-zA-Z >]{1,40}"),
        ) {
            let chunks = split_technical_content(&input, max, crumb.as_deref());
            let prefix = breadcrumb_prefix(crumb.as_deref(), max);
            let mut bodies = String::new();
            for chunk in &chunks {
                prop_assert!(chunk.chars().count() <= max, "oversize chunk {chunk:?}");
                prop_assert_eq!(chunk.trim(), chunk.as_str());
                prop_assert!(!chunk.is_empty());
                prop_assert!(chunk.starts_with(&prefix));
                prop_assert!(fence_count(chunk).is_multiple_of(2), "unbalanced fences {chunk:?}");
                bodies.push_str(&chunk[prefix.len()..]);
                bodies.push('\n');
            }
            prop_assert_eq!(
                non_ws(&strip_fence_lines(&bodies)),
                non_ws(&strip_fence_lines(&input))
            );
            prop_assert_eq!(&chunks, &split_technical_content(&input, max, crumb.as_deref()));
        }
    }
}
