//! PDF verification and the extraction fallback: when the built-in extractor
//! recovers far less text than `pdftotext`, the `pdftotext` text is stored.

use serde::Serialize;
use std::ops::Not;
use std::path::Path;
use std::time::Duration;

/// Smallest body accepted as a paper PDF. Error pages and "access denied"
/// stubs that happen to be PDFs are smaller than this.
pub const MIN_PDF_BYTES: usize = 8 * 1024;
/// Largest body pulled out of the browser; matches the Paperseed extraction cap.
pub const MAX_PDF_BYTES: usize = 100 * 1024 * 1024;
/// The built-in text is replaced when it is below this share of `pdftotext`.
pub const MIN_BUILTIN_RATIO: f64 = 0.5;
/// The built-in text is replaced when it averages fewer characters per page.
pub const MIN_CHARS_PER_PAGE: usize = 200;

const PDFTOTEXT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfRejection {
    NotPdf,
    TooSmall { bytes: usize, floor: usize },
    Truncated,
}

impl std::fmt::Display for PdfRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PdfRejection::NotPdf => write!(f, "response does not start with %PDF-"),
            PdfRejection::TooSmall { bytes, floor } => write!(
                f,
                "PDF is {bytes} bytes, below the {floor}-byte floor for a paper"
            ),
            PdfRejection::Truncated => write!(f, "PDF has no %%EOF trailer (truncated download)"),
        }
    }
}

pub fn has_pdf_magic(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
}

/// Check the `%PDF-` magic, the size floor, and the `%%EOF` trailer.
pub fn verify_pdf(bytes: &[u8], min_bytes: usize) -> std::result::Result<(), PdfRejection> {
    if has_pdf_magic(bytes).not() {
        return Err(PdfRejection::NotPdf);
    }
    if bytes.len() < min_bytes {
        return Err(PdfRejection::TooSmall {
            bytes: bytes.len(),
            floor: min_bytes,
        });
    }
    let tail = &bytes[bytes.len().saturating_sub(2048)..];
    if tail.windows(5).any(|window| window == b"%%EOF").not() {
        return Err(PdfRejection::Truncated);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Extractor {
    /// Paperseed's in-process extractor (`pdf-extract`).
    Builtin,
    /// Poppler's `pdftotext` command.
    Pdftotext,
    /// Neither extractor produced text; the PDF is stored without full text.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ExtractionReport {
    pub extractor: Extractor,
    pub reason: String,
    /// Non-whitespace characters from the built-in extractor.
    pub builtin_chars: usize,
    /// Non-whitespace characters from `pdftotext`; absent when not installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdftotext_chars: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<usize>,
}

/// Measured output of `pdftotext` for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdftotextOutput {
    pub text: String,
    pub pages: usize,
}

pub fn text_chars(text: &str) -> usize {
    text.chars().filter(|c| c.is_whitespace().not()).count()
}

/// Decide which extractor's text to keep. Character counts exclude
/// whitespace so `pdftotext` layout padding does not skew the comparison.
pub fn select_extractor(
    builtin_chars: usize,
    pdftotext: Option<(usize, usize)>,
) -> (Extractor, String) {
    let Some((pdftotext_chars, pages)) = pdftotext else {
        return if builtin_chars == 0 {
            (
                Extractor::None,
                "built-in extractor produced no text and pdftotext is not installed".to_string(),
            )
        } else {
            (
                Extractor::Builtin,
                "pdftotext is not installed; built-in text kept unchecked".to_string(),
            )
        };
    };
    if builtin_chars == 0 && pdftotext_chars == 0 {
        return (
            Extractor::None,
            "neither extractor produced text (scanned PDF?)".to_string(),
        );
    }
    if pdftotext_chars <= builtin_chars {
        return (
            Extractor::Builtin,
            "built-in text is at least as long as pdftotext".to_string(),
        );
    }
    let ratio = builtin_chars as f64 / pdftotext_chars as f64;
    if ratio < MIN_BUILTIN_RATIO {
        return (
            Extractor::Pdftotext,
            format!(
                "built-in text is {:.0}% of pdftotext, below the {:.0}% floor",
                ratio * 100.0,
                MIN_BUILTIN_RATIO * 100.0
            ),
        );
    }
    let per_page = builtin_chars / pages.max(1);
    if per_page < MIN_CHARS_PER_PAGE {
        return (
            Extractor::Pdftotext,
            format!(
                "built-in text averages {per_page} characters per page, below the {MIN_CHARS_PER_PAGE} floor"
            ),
        );
    }
    (
        Extractor::Builtin,
        format!(
            "built-in text is {:.0}% of pdftotext and {per_page} characters per page",
            ratio * 100.0
        ),
    )
}

/// Turn raw `pdftotext` output into stored text: form feeds become page
/// breaks and trailing whitespace is dropped.
pub fn normalize_pdftotext(raw: &str) -> PdftotextOutput {
    let pages = raw.matches('\u{000c}').count().max(1);
    let mut text = String::with_capacity(raw.len());
    for line in raw.replace('\u{000c}', "\n\n").lines() {
        text.push_str(line.trim_end());
        text.push('\n');
    }
    PdftotextOutput {
        text: text.trim().to_string(),
        pages,
    }
}

/// Run `pdftotext` on a local file. `None` when the command is missing,
/// fails, or times out; the caller then keeps the built-in text.
pub async fn run_pdftotext(command: &str, pdf: &Path) -> Option<PdftotextOutput> {
    let child = tokio::process::Command::new(command)
        .args(["-enc", "UTF-8"])
        .arg(pdf)
        .arg("-")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(PDFTOTEXT_TIMEOUT, child)
        .await
        .ok()?
        .ok()?;
    if output.status.success().not() {
        return None;
    }
    Some(normalize_pdftotext(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf_of(len: usize) -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        bytes.resize(len.saturating_sub(6), b' ');
        bytes.extend_from_slice(b"%%EOF\n");
        bytes
    }

    #[test]
    fn verify_pdf_checks_magic_floor_and_trailer() {
        assert_eq!(verify_pdf(&pdf_of(MIN_PDF_BYTES), MIN_PDF_BYTES), Ok(()));
        assert_eq!(
            verify_pdf(b"<!DOCTYPE html><html>Sign in</html>", MIN_PDF_BYTES),
            Err(PdfRejection::NotPdf)
        );
        assert_eq!(
            verify_pdf(&pdf_of(512), MIN_PDF_BYTES),
            Err(PdfRejection::TooSmall {
                bytes: 512,
                floor: MIN_PDF_BYTES
            })
        );
        let mut truncated = pdf_of(MIN_PDF_BYTES);
        truncated.truncate(MIN_PDF_BYTES - 100);
        truncated.resize(MIN_PDF_BYTES, b' ');
        assert_eq!(
            verify_pdf(&truncated, MIN_PDF_BYTES),
            Err(PdfRejection::Truncated)
        );
        assert_eq!(verify_pdf(b"", 0), Err(PdfRejection::NotPdf));
    }

    #[test]
    fn extractor_selection_follows_ratio_and_per_page_floors() {
        // The case from the field: 10k built-in against 170k from pdftotext.
        let (extractor, reason) = select_extractor(10_000, Some((170_000, 40)));
        assert_eq!(extractor, Extractor::Pdftotext);
        assert!(reason.contains("6% of pdftotext"), "{reason}");

        // Comparable output: keep the built-in text.
        assert_eq!(
            select_extractor(48_000, Some((50_000, 12))).0,
            Extractor::Builtin
        );
        assert_eq!(
            select_extractor(50_000, Some((48_000, 12))).0,
            Extractor::Builtin
        );
        // Above the ratio but thin per page.
        let (extractor, reason) = select_extractor(1_500, Some((2_500, 10)));
        assert_eq!(extractor, Extractor::Pdftotext);
        assert!(reason.contains("150 characters per page"), "{reason}");
        // Without pdftotext the built-in text is kept and the report says so.
        let (extractor, reason) = select_extractor(10_000, None);
        assert_eq!(extractor, Extractor::Builtin);
        assert!(reason.contains("not installed"));
        assert_eq!(select_extractor(0, None).0, Extractor::None);
        assert_eq!(select_extractor(0, Some((0, 3))).0, Extractor::None);
        assert_eq!(
            select_extractor(0, Some((9_000, 3))).0,
            Extractor::Pdftotext
        );
    }

    #[test]
    fn pdftotext_output_is_normalized_and_pages_counted() {
        let output = normalize_pdftotext("Title   \nbody one\n\u{000c}page two  \n\u{000c}");
        assert_eq!(output.pages, 2);
        assert_eq!(output.text, "Title\nbody one\n\n\npage two");
        assert_eq!(text_chars(&output.text), 19);
    }

    #[tokio::test]
    async fn missing_pdftotext_command_yields_none() {
        let missing = run_pdftotext("paperbridge-no-such-pdftotext", Path::new("x.pdf")).await;
        assert_eq!(missing, None);
    }
}
