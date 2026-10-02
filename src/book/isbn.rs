//! ISBN normalization, validation, and conversion helpers.

/// Normalizes an ISBN string by stripping "ISBN" prefixes, hyphens, and spaces,
/// validating its checksum, and returning canonical 13-digit ISBN.
/// If a valid 10-digit ISBN is provided, it is converted to canonical ISBN-13.
/// If the input contains multiple ISBN candidates (e.g. separated by whitespace,
/// comma, or semicolon as often found in Zotero), returns the first valid one.
pub(crate) fn normalize_isbn(raw: &str) -> Option<String> {
    let tokens: Vec<&str> = raw
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if tokens.len() > 1 {
        for token in &tokens {
            if let Some(norm) = normalize_single_isbn(token) {
                return Some(norm);
            }
        }
        // Spaced ISBNs like "978 1 4919 0307 0" split into multiple tokens,
        // none of which is a full ISBN alone. Fall back to parsing the whole string.
        return normalize_single_isbn(raw);
    }

    normalize_single_isbn(raw)
}

fn normalize_single_isbn(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let upper = trimmed.to_ascii_uppercase();
    const PREFIXES: &[&str] = &[
        "ISBN-13:", "ISBN-10:", "ISBN13:", "ISBN10:", "ISBN-13", "ISBN-10", "ISBN13", "ISBN10",
        "ISBN:", "ISBN",
    ];
    let rest = if let Some(prefix) = PREFIXES.iter().find(|p| upper.starts_with(*p)) {
        trimmed[prefix.len()..].trim_start_matches([' ', ':', '-'])
    } else {
        trimmed
    };

    // Filter to digits, plus 'X' if in the last position
    let mut cleaned = Vec::with_capacity(13);
    for c in rest.chars() {
        if c.is_ascii_digit() {
            cleaned.push(c as u8);
        } else if (c == 'X' || c == 'x') && cleaned.len() == 9 {
            cleaned.push(b'X');
        } else if c == '-'
            || c == ' '
            || c == '.'
            || c == '\u{2010}'
            || c == '\u{2013}'
            || c == '\u{2014}'
            || c == '\u{2212}'
        {
            // Ignore hyphens, dashes, spaces, dots
        } else {
            // Unexpected character in ISBN body
            return None;
        }
    }

    if cleaned.len() == 13 && is_valid_isbn13_bytes(&cleaned) {
        // Safe: cleaned contains only ASCII digits
        String::from_utf8(cleaned).ok()
    } else if cleaned.len() == 10 && is_valid_isbn10_bytes(&cleaned) {
        isbn10_to_isbn13_bytes(&cleaned)
    } else {
        None
    }
}

/// Checks whether the byte slice is a valid ISBN-10.
#[cfg(test)]
pub(crate) fn is_valid_isbn10(isbn: &str) -> bool {
    let bytes = isbn.as_bytes();
    is_valid_isbn10_bytes(bytes)
}

fn is_valid_isbn10_bytes(bytes: &[u8]) -> bool {
    if bytes.len() != 10 {
        return false;
    }
    let mut sum: u32 = 0;
    for (i, &b) in bytes[..9].iter().enumerate() {
        if !b.is_ascii_digit() {
            return false;
        }
        sum += (10 - i as u32) * (b - b'0') as u32;
    }
    let check_digit = match bytes[9] {
        b'X' | b'x' => 10,
        b if b.is_ascii_digit() => (b - b'0') as u32,
        _ => return false,
    };
    sum += check_digit;
    sum.is_multiple_of(11)
}

/// Checks whether the byte slice is a valid ISBN-13.
#[cfg(test)]
pub(crate) fn is_valid_isbn13(isbn: &str) -> bool {
    let bytes = isbn.as_bytes();
    is_valid_isbn13_bytes(bytes)
}

fn is_valid_isbn13_bytes(bytes: &[u8]) -> bool {
    if bytes.len() != 13 {
        return false;
    }
    // Canonical ISBN-13 always uses Bookland prefixes 978 or 979
    if !bytes.starts_with(b"978") && !bytes.starts_with(b"979") {
        return false;
    }
    let mut sum: u32 = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if !b.is_ascii_digit() {
            return false;
        }
        let digit = (b - b'0') as u32;
        let weight = if i % 2 == 0 { 1 } else { 3 };
        sum += digit * weight;
    }
    sum.is_multiple_of(10)
}

/// Converts validated 10-digit ISBN bytes into a 13-digit ISBN string.
fn isbn10_to_isbn13_bytes(isbn10: &[u8]) -> Option<String> {
    if isbn10.len() != 10 {
        return None;
    }
    let mut out = Vec::with_capacity(13);
    out.extend_from_slice(b"978");
    out.extend_from_slice(&isbn10[..9]);

    let mut sum: u32 = 0;
    for (i, &b) in out.iter().enumerate() {
        if !b.is_ascii_digit() {
            return None;
        }
        let digit = (b - b'0') as u32;
        let weight = if i % 2 == 0 { 1 } else { 3 };
        sum += digit * weight;
    }
    let check = ((10 - (sum % 10)) % 10) as u8;
    out.push(b'0' + check);
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_isbn10() {
        assert!(is_valid_isbn10("0306406152"));
        assert!(is_valid_isbn10("0471958697"));
        assert!(is_valid_isbn10("080442957X"));
        assert!(!is_valid_isbn10("0306406153"));
        assert!(!is_valid_isbn10("12345"));
    }

    #[test]
    fn test_valid_isbn13() {
        assert!(is_valid_isbn13("9780306406157"));
        assert!(is_valid_isbn13("9781491903070")); // Designing Data-Intensive Applications
        assert!(!is_valid_isbn13("9780306406158"));
    }

    #[test]
    fn test_isbn10_to_isbn13() {
        assert_eq!(
            isbn10_to_isbn13_bytes(b"0306406152"),
            Some("9780306406157".to_string())
        );
    }

    #[test]
    fn test_normalize_isbn() {
        assert_eq!(
            normalize_isbn("978-1-4919-0307-0"),
            Some("9781491903070".to_string())
        );
        assert_eq!(
            normalize_isbn("ISBN 978-1-4919-0307-0"),
            Some("9781491903070".to_string())
        );
        assert_eq!(
            normalize_isbn("isbn: 9781491903070"),
            Some("9781491903070".to_string())
        );
        assert_eq!(
            normalize_isbn("0-306-40615-2"),
            Some("9780306406157".to_string())
        );
        assert_eq!(
            normalize_isbn("ISBN-13: 978-1-4919-0307-0"),
            Some("9781491903070".to_string())
        );
        assert_eq!(
            normalize_isbn("ISBN-10: 0-306-40615-2"),
            Some("9780306406157".to_string())
        );
        assert_eq!(
            normalize_isbn("ISBN13: 9781491903070"),
            Some("9781491903070".to_string())
        );
        assert_eq!(
            normalize_isbn("ISBN10: 0306406152"),
            Some("9780306406157".to_string())
        );
        assert_eq!(
            normalize_isbn("isbn13 9781491903070"),
            Some("9781491903070".to_string())
        );
        assert_eq!(
            normalize_isbn("isbn:080442957x"),
            Some("9780804429573".to_string())
        );
        // Multi-ISBN string separation (whitespace, comma, semicolon)
        assert_eq!(
            normalize_isbn("9781491903070 1491903074"),
            Some("9781491903070".to_string())
        );
        assert_eq!(
            normalize_isbn("invalid, 9781491903070; 1491903074"),
            Some("9781491903070".to_string())
        );
        // Spaced ISBN
        assert_eq!(
            normalize_isbn("978 1 4919 0307 0"),
            Some("9781491903070".to_string())
        );
        // Non-ASCII Unicode hyphens / dashes (U+2010, U+2013, U+2014)
        assert_eq!(
            normalize_isbn("978‐1‐4919‐0307‐0"),
            Some("9781491903070".to_string())
        );
        assert_eq!(
            normalize_isbn("978–1–4919–0307–0"),
            Some("9781491903070".to_string())
        );
        assert_eq!(
            normalize_isbn("978—1—4919—0307—0"),
            Some("9781491903070".to_string())
        );
        // Non-978/979 prefix rejected
        assert_eq!(normalize_isbn("1234567890128"), None);
        assert_eq!(normalize_isbn("invalid-isbn"), None);
        assert_eq!(normalize_isbn(""), None);
    }
}
