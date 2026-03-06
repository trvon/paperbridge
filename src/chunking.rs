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
}
