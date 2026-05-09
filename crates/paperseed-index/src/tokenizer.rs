//! Deterministic UTF-8 tokenizer used by both index build and query.
//!
//! Lowercases ASCII, splits on any non-alphanumeric Unicode character, and
//! drops empty tokens. Does not stem, fold diacritics, or remove stopwords —
//! BM25F's IDF term handles common-token down-weighting on its own.

pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
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
    fn splits_on_whitespace_and_punctuation() {
        assert_eq!(
            tokenize("Hello, World! Foo-bar."),
            vec!["hello", "world", "foo", "bar"],
        );
    }

    #[test]
    fn lowercases_ascii() {
        assert_eq!(tokenize("BM25F Index"), vec!["bm25f", "index"]);
    }

    #[test]
    fn keeps_alphanumerics_inside_token() {
        assert_eq!(tokenize("v2 test123"), vec!["v2", "test123"]);
    }

    #[test]
    fn empty_text_yields_no_tokens() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   ").is_empty());
        assert!(tokenize("---").is_empty());
    }

    #[test]
    fn handles_unicode_letters() {
        // is_alphanumeric() is true for letters in any script.
        let toks = tokenize("café résumé");
        assert_eq!(toks.len(), 2);
        // ASCII lowercase only — NFC/NFD equivalence is out of scope.
        assert!(toks[0].starts_with("café") || toks[0] == "café");
    }

    #[test]
    fn deterministic_across_calls() {
        let a = tokenize("Reproducible Tokenizer");
        let b = tokenize("Reproducible Tokenizer");
        assert_eq!(a, b);
    }
}
