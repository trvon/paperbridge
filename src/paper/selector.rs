use crate::error::{Result, ZoteroMcpError};
use serde_json::Value;

pub fn evaluate(root: &Value, selector: &str) -> Result<Value> {
    let trimmed = selector.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Ok(root.clone());
    }

    let mut current = root;
    let mut buf = String::new();
    let mut i = 0;
    let bytes = trimmed.as_bytes();

    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '.' {
            if !buf.is_empty() {
                current = descend_key(current, &buf, trimmed)?;
                buf.clear();
            }
            i += 1;
        } else if c == '[' {
            if !buf.is_empty() {
                current = descend_key(current, &buf, trimmed)?;
                buf.clear();
            }
            let end = trimmed[i..]
                .find(']')
                .ok_or_else(|| invalid(trimmed, "unclosed '['"))?
                + i;
            let idx_str = &trimmed[i + 1..end];
            let idx: usize = idx_str
                .trim()
                .parse()
                .map_err(|_| invalid(trimmed, &format!("bad index '{idx_str}'")))?;
            current = descend_index(current, idx, trimmed)?;
            i = end + 1;
        } else {
            buf.push(c);
            i += 1;
        }
    }

    if !buf.is_empty() {
        current = descend_key(current, &buf, trimmed)?;
    }

    Ok(current.clone())
}

fn descend_key<'a>(value: &'a Value, key: &str, selector: &str) -> Result<&'a Value> {
    match value {
        Value::Object(map) => map.get(key).ok_or_else(|| {
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort();
            invalid(
                selector,
                &format!("missing key '{key}' (available: {})", keys.join(", ")),
            )
        }),
        Value::Array(_) => Err(invalid(
            selector,
            &format!("expected object, found array when resolving '{key}'"),
        )),
        _ => Err(invalid(
            selector,
            &format!("expected object, found scalar when resolving '{key}'"),
        )),
    }
}

fn descend_index<'a>(value: &'a Value, idx: usize, selector: &str) -> Result<&'a Value> {
    match value {
        Value::Array(arr) => arr
            .get(idx)
            .ok_or_else(|| invalid(selector, &format!("index {idx} out of range"))),
        _ => Err(invalid(
            selector,
            &format!("expected array for index [{idx}]"),
        )),
    }
}

fn invalid(selector: &str, reason: &str) -> ZoteroMcpError {
    ZoteroMcpError::InvalidInput(format!("invalid selector '{selector}': {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "metadata": {
                "title": "Attention Is All You Need",
                "authors": ["Vaswani", "Shazeer"]
            },
            "sections": [
                {"id": "intro", "heading": "Introduction", "text": "hello"},
                {"id": "methods", "heading": "Methods", "text": "world"}
            ]
        })
    }

    #[test]
    fn returns_root_when_empty() {
        let v = sample();
        assert_eq!(evaluate(&v, "").unwrap(), v);
        assert_eq!(evaluate(&v, ".").unwrap(), v);
    }

    #[test]
    fn dotted_path_resolves_scalar() {
        let v = sample();
        assert_eq!(
            evaluate(&v, "metadata.title").unwrap(),
            json!("Attention Is All You Need")
        );
    }

    #[test]
    fn array_index_in_brackets() {
        let v = sample();
        assert_eq!(
            evaluate(&v, "metadata.authors[1]").unwrap(),
            json!("Shazeer")
        );
        assert_eq!(
            evaluate(&v, "sections[0].heading").unwrap(),
            json!("Introduction")
        );
    }

    #[test]
    fn missing_key_errors() {
        let v = sample();
        let err = evaluate(&v, "metadata.doi").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("missing key 'doi'"));
        // Error must enumerate what is actually available so callers can
        // recover without guessing field names.
        assert!(msg.contains("available:"));
        assert!(msg.contains("title"));
        assert!(msg.contains("authors"));
    }

    #[test]
    fn out_of_range_errors() {
        let v = sample();
        let err = evaluate(&v, "sections[99]").unwrap_err();
        assert!(format!("{err}").contains("out of range"));
    }

    #[test]
    fn unclosed_bracket_errors() {
        let v = sample();
        assert!(evaluate(&v, "sections[0").is_err());
    }

    #[test]
    fn metadata_abstract_selector_resolves_on_paper_structure() {
        use crate::models::{PaperMetadata, PaperStructure, PaperStructureSource};
        let structure = PaperStructure {
            item_key: "ABCD".to_string(),
            attachment_key: None,
            metadata: PaperMetadata {
                title: Some("Title".to_string()),
                authors: vec!["Vaswani".to_string()],
                abstract_note: Some("Attention summary.".to_string()),
                doi: None,
                year: None,
            },
            sections: vec![],
            references: vec![],
            figures: vec![],
            source: PaperStructureSource::ZoteroFulltext,
        };
        let v = serde_json::to_value(&structure).unwrap();
        // Canonical selector — what the user typed.
        assert_eq!(
            evaluate(&v, "metadata.abstract").unwrap(),
            json!("Attention summary.")
        );
    }

    #[test]
    fn paper_metadata_accepts_legacy_abstract_note_alias() {
        // Anything that previously emitted JSON with `abstract_note` must
        // still deserialize cleanly into `PaperMetadata`.
        use crate::models::PaperMetadata;
        let legacy = json!({
            "title": "Old",
            "abstract_note": "From the old shape",
            "doi": null,
            "year": null,
        });
        let parsed: PaperMetadata = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.abstract_note.as_deref(), Some("From the old shape"));

        // camelCase Zotero shape also works.
        let zotero = json!({
            "title": "Z",
            "abstractNote": "From Zotero",
            "doi": null,
            "year": null,
        });
        let parsed: PaperMetadata = serde_json::from_value(zotero).unwrap();
        assert_eq!(parsed.abstract_note.as_deref(), Some("From Zotero"));
    }
}
