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
        Value::Object(map) => map
            .get(key)
            .ok_or_else(|| invalid(selector, &format!("missing key '{key}'"))),
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
        assert!(format!("{err}").contains("missing key 'doi'"));
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
}
