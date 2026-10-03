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

/// Split long text into chunks that are suitable for speech synthesis.
///
/// Strategy:
/// 1) Split by sentence boundaries.
/// 2) Pack sentences into chunks up to `max_chars`.
/// 3) If one sentence exceeds max, hard-split it.
pub fn split_for_tts(input: &str, max_chars: usize) -> Vec<String> {
    let text = normalize_text_for_tts(input);
    if text.is_empty() {
        return Vec::new();
    }

    let safe_max = max_chars.max(1);
    let sentences = sentence_split(&text);
    let mut chunks = Vec::new();
    let mut current = String::new();

    for sentence in sentences {
        if sentence.len() > safe_max {
            if !current.is_empty() {
                chunks.push(current.trim().to_string());
                current.clear();
            }
            for part in hard_split(sentence, safe_max) {
                chunks.push(part);
            }
            continue;
        }

        if current.is_empty() {
            current.push_str(sentence);
            continue;
        }

        if current.len() + 1 + sentence.len() <= safe_max {
            current.push(' ');
            current.push_str(sentence);
        } else {
            chunks.push(current.trim().to_string());
            current.clear();
            current.push_str(sentence);
        }
    }

    if !current.is_empty() {
        chunks.push(current.trim().to_string());
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

    if start < text.len() {
        let tail = text[start..].trim();
        if !tail.is_empty() {
            out.push(tail);
        }
    }

    if out.is_empty() {
        out.push(text.trim());
    }

    out
}

fn hard_split(sentence: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();

    for word in sentence.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
            continue;
        }

        if current.len() + 1 + word.len() <= max_chars {
            current.push(' ');
            current.push_str(word);
        } else {
            out.push(current);
            current = word.to_string();
        }
    }

    if !current.is_empty() {
        out.push(current);
    }

    out
}

/// Split technical text (such as book chapters or code-heavy documentation)
/// while preserving markdown fenced code blocks and optional breadcrumbs.
pub(crate) fn split_technical_content(
    input: &str,
    max_chars: usize,
    breadcrumb: Option<&str>,
) -> Vec<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let prefix = breadcrumb.map(|b| format!("{b}\n\n")).unwrap_or_default();
    let prefix_len = prefix.chars().count();
    let target_max = max_chars.saturating_sub(prefix_len).clamp(1, max_chars);

    let blocks = segment_markdown_blocks(trimmed);
    let mut chunks = Vec::new();
    let mut current = String::new();

    for block in blocks {
        let block_len = block.chars().count();
        if block_len > target_max {
            if !current.is_empty() {
                chunks.push(format!("{prefix}{}", current.trim()));
                current.clear();
            }
            let is_fenced = block.trim_start().starts_with("```");
            let sub_parts = if is_fenced {
                split_code_block(&block, target_max)
            } else {
                line_split_block(&block, target_max)
            };
            for part in sub_parts {
                chunks.push(format!("{prefix}{}", part.trim()));
            }
            continue;
        }

        if current.is_empty() {
            current.push_str(&block);
            continue;
        }

        let needed = current.chars().count() + 2 + block_len;
        if needed <= target_max {
            current.push_str("\n\n");
            current.push_str(&block);
        } else {
            chunks.push(format!("{prefix}{}", current.trim()));
            current.clear();
            current.push_str(&block);
        }
    }

    if !current.is_empty() {
        chunks.push(format!("{prefix}{}", current.trim()));
    }

    chunks
}

fn segment_markdown_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current_block = String::new();
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.starts_with("```") {
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

        if trimmed_line.is_empty() {
            if !current_block.trim().is_empty() {
                blocks.push(current_block.trim_end().to_string());
                current_block.clear();
            }
        } else {
            if !current_block.is_empty() {
                current_block.push('\n');
            }
            current_block.push_str(line);
        }
    }

    if !current_block.trim().is_empty() {
        blocks.push(current_block.trim_end().to_string());
    }

    blocks
}

fn line_split_block(block: &str, max_chars: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();

    for line in block.lines() {
        if line.chars().count() > max_chars {
            if !current.is_empty() {
                parts.push(current.trim_end().to_string());
                current.clear();
            }
            for chunk in hard_split(line, max_chars) {
                parts.push(chunk);
            }
            continue;
        }

        let needed = if current.is_empty() {
            line.chars().count()
        } else {
            current.chars().count() + 1 + line.chars().count()
        };

        if needed <= max_chars {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        } else {
            parts.push(current.trim_end().to_string());
            current.clear();
            current.push_str(line);
        }
    }

    if !current.is_empty() {
        parts.push(current.trim_end().to_string());
    }

    parts
}

fn split_code_block(block: &str, target_max: usize) -> Vec<String> {
    let mut lines = block.lines();
    let Some(first_line) = lines.next() else {
        return vec![block.to_string()];
    };
    let fence_header = first_line.trim_end();
    let fence_close = "```";
    let body_lines: Vec<&str> = lines.collect();
    let body_lines = if body_lines
        .last()
        .map(|l| l.trim())
        .is_some_and(|l| l.starts_with("```"))
    {
        &body_lines[..body_lines.len() - 1]
    } else {
        &body_lines[..]
    };

    let overhead = fence_header.chars().count() + 1 + fence_close.chars().count() + 1;
    let effective_max = target_max.saturating_sub(overhead).max(20);

    let mut parts = Vec::new();
    let mut current_body = String::new();

    for &line in body_lines {
        let needed = if current_body.is_empty() {
            line.chars().count()
        } else {
            current_body.chars().count() + 1 + line.chars().count()
        };

        if needed <= effective_max {
            if !current_body.is_empty() {
                current_body.push('\n');
            }
            current_body.push_str(line);
        } else {
            if !current_body.is_empty() {
                parts.push(format!("{fence_header}\n{current_body}\n{fence_close}"));
                current_body.clear();
            }
            if line.chars().count() > effective_max {
                for part in hard_split(line, effective_max) {
                    parts.push(format!("{fence_header}\n{part}\n{fence_close}"));
                }
            } else {
                current_body.push_str(line);
            }
        }
    }

    if !current_body.is_empty() {
        parts.push(format!("{fence_header}\n{current_body}\n{fence_close}"));
    }

    if parts.is_empty() {
        vec![block.to_string()]
    } else {
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }
}
