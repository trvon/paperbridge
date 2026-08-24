//! Deterministic UTF-8 tokenizer used by both index build and query.
//!
//! Applies Unicode decomposition and diacritic folding, lowercases, splits on
//! non-alphanumeric characters, and applies a small deterministic English
//! suffix stemmer. Stopwords remain because BM25F's IDF down-weights them.

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.nfd().filter(|ch| !is_combining_mark(*ch)) {
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        } else if !current.is_empty() {
            out.push(stem(std::mem::take(&mut current)));
        }
    }
    if !current.is_empty() {
        out.push(stem(current));
    }
    out
}

fn stem(mut token: String) -> String {
    let len = token.chars().count();
    if len > 5 && token.ends_with("ies") {
        token.truncate(token.len() - 3);
        token.push('y');
    } else if len > 5 && token.ends_with("ing") {
        token.truncate(token.len() - 3);
    } else if len > 4 && token.ends_with("ed") {
        token.truncate(token.len() - 2);
    } else if len > 4
        && token.ends_with('s')
        && !token.ends_with("ss")
        && !token.ends_with("us")
        && !token.ends_with("is")
    {
        token.pop();
    }
    token
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
        assert_eq!(
            tokenize("café résumé Müller"),
            vec!["cafe", "resume", "muller"]
        );
    }

    #[test]
    fn canonical_and_decomposed_diacritics_match() {
        assert_eq!(tokenize("Müller"), tokenize("Mu\u{308}ller"));
    }

    #[test]
    fn lightly_stems_common_english_suffixes() {
        assert_eq!(
            tokenize("networks studies learning learned"),
            vec!["network", "study", "learn", "learn"]
        );
    }

    #[test]
    fn deterministic_across_calls() {
        let a = tokenize("Reproducible Tokenizer");
        let b = tokenize("Reproducible Tokenizer");
        assert_eq!(a, b);
    }
}
