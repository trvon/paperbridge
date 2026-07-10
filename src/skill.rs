//! Deterministic paper → SKILL.md scaffold generation.
//!
//! This module performs a purely mechanical mapping of a [`PaperStructure`]
//! into a Claude/agent SKILL.md document (YAML frontmatter + markdown body).
//! There is no ranking, summarization, or other model judgment here — the
//! consuming agent is expected to promote the scaffold into a genuine
//! operating procedure. Keeping this deterministic preserves paperbridge's
//! "bridge, not brain" scope.

use crate::models::{
    PaperReference, PaperSection, PaperSectionKind, PaperStructure, PaperStructureSource,
    SkillPayload,
};
use std::fmt::Write as _;

/// Maximum characters kept from any single body section group. Section text is
/// a scaffold, not the full paper — the agent reads the original for detail.
const MAX_SECTION_CHARS: usize = 1_200;
/// Maximum references listed under "Key references".
const MAX_REFERENCES: usize = 25;
/// Maximum length of the slugified frontmatter `name`.
const MAX_SLUG_LEN: usize = 60;

/// Build a deterministic SKILL.md scaffold from a parsed paper structure.
pub fn build_skill_scaffold(structure: &PaperStructure) -> SkillPayload {
    let title = structure
        .metadata
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());

    let name = slugify(title.unwrap_or(&structure.item_key));
    let description = build_description(title, structure.metadata.abstract_note.as_deref());

    let mut body = String::new();
    let _ = writeln!(body, "---");
    let _ = writeln!(body, "name: {name}");
    let _ = writeln!(body, "description: {}", yaml_quote(&description));
    let _ = writeln!(body, "---");
    body.push('\n');
    let _ = writeln!(body, "# {}", title.unwrap_or("Untitled paper"));
    body.push('\n');
    let _ = writeln!(body, "<!-- {} -->", provenance_note(&structure.source));
    if !structure.metadata.authors.is_empty() {
        let _ = writeln!(
            body,
            "<!-- authors: {} -->",
            structure.metadata.authors.join(", ")
        );
    }
    if let Some(doi) = structure
        .metadata
        .doi
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        let _ = writeln!(body, "<!-- doi: {doi} -->");
    }
    body.push('\n');

    // ## When to use ← abstract
    if let Some(text) = section_text(structure, &[PaperSectionKind::Abstract])
        .or_else(|| non_empty(structure.metadata.abstract_note.as_deref()))
    {
        push_section(&mut body, "When to use", &text);
    }

    // ## Method ← Method + Design + Implementation
    if let Some(text) = section_text(
        structure,
        &[
            PaperSectionKind::Method,
            PaperSectionKind::Design,
            PaperSectionKind::Implementation,
        ],
    ) {
        push_section(&mut body, "Method", &text);
    }

    // ## Evaluation ← Evaluation + Results
    if let Some(text) = section_text(
        structure,
        &[PaperSectionKind::Evaluation, PaperSectionKind::Results],
    ) {
        push_section(&mut body, "Evaluation", &text);
    }

    // ## Limitations ← Limitations
    if let Some(text) = section_text(structure, &[PaperSectionKind::Limitations]) {
        push_section(&mut body, "Limitations", &text);
    }

    // ## Key references ← top-N references
    if !structure.references.is_empty() {
        let _ = writeln!(body, "## Key references\n");
        for reference in structure.references.iter().take(MAX_REFERENCES) {
            let _ = writeln!(body, "- {}", format_reference(reference));
        }
        body.push('\n');
    }

    SkillPayload {
        name,
        description,
        markdown: body.trim_end().to_string() + "\n",
    }
}

/// Collect and digest the text of all sections (recursively) whose `kind`
/// matches one of `kinds`, in document order. Returns `None` when no matching
/// section carries text.
fn section_text(structure: &PaperStructure, kinds: &[PaperSectionKind]) -> Option<String> {
    let mut collected = String::new();
    for section in &structure.sections {
        collect_matching(section, kinds, &mut collected);
    }
    let trimmed = collected.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(truncate_on_boundary(trimmed, MAX_SECTION_CHARS))
}

fn collect_matching(section: &PaperSection, kinds: &[PaperSectionKind], out: &mut String) {
    if section
        .kind
        .as_ref()
        .is_some_and(|kind| kinds.contains(kind))
    {
        let text = section.text.trim();
        if !text.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(text);
        }
    }
    for sub in &section.subsections {
        collect_matching(sub, kinds, out);
    }
}

fn push_section(body: &mut String, heading: &str, text: &str) {
    let _ = writeln!(body, "## {heading}\n");
    let _ = writeln!(body, "{}\n", text.trim());
}

fn build_description(title: Option<&str>, abstract_note: Option<&str>) -> String {
    if let Some(text) = non_empty(abstract_note) {
        let sentences = first_sentences(&text, 2, 320);
        if !sentences.is_empty() {
            return sentences;
        }
    }
    match title {
        Some(title) => format!("Use when applying the methods or findings of \"{title}\"."),
        None => "Use when applying the methods or findings of this paper.".to_string(),
    }
}

fn provenance_note(source: &PaperStructureSource) -> String {
    match source {
        PaperStructureSource::Grobid => {
            "source: grobid — parsed section hierarchy, high fidelity".to_string()
        }
        PaperStructureSource::ZoteroFulltext => {
            "source: zotero_fulltext — heuristic sectioning, verify against the paper".to_string()
        }
        PaperStructureSource::GrobidUnavailable { reason } => {
            format!("source: grobid_unavailable ({reason}) — fell back to full text, verify")
        }
    }
}

fn format_reference(reference: &PaperReference) -> String {
    let mut parts = Vec::new();
    if let Some(title) = non_empty(reference.title.as_deref()) {
        parts.push(title);
    } else if !reference.raw.trim().is_empty() {
        parts.push(reference.raw.trim().to_string());
    }
    if let Some(year) = non_empty(reference.year.as_deref()) {
        parts.push(format!("({year})"));
    }
    if let Some(doi) = non_empty(reference.doi.as_deref()) {
        parts.push(format!("doi:{doi}"));
    }
    if parts.is_empty() {
        "(reference)".to_string()
    } else {
        parts.join(" ")
    }
}

/// Lowercase, ASCII-alphanumeric slug with hyphen separators, capped length.
fn slugify(input: &str) -> String {
    let mut slug = String::with_capacity(input.len().min(MAX_SLUG_LEN));
    let mut prev_hyphen = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen && !slug.is_empty() {
            slug.push('-');
            prev_hyphen = true;
        }
        if slug.len() >= MAX_SLUG_LEN {
            break;
        }
    }
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "paper-skill".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Quote a string as a YAML double-quoted scalar so colons, `#`, and leading
/// special characters in the description never break the frontmatter.
fn yaml_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    let single_line = escaped.replace(['\n', '\r'], " ");
    format!("\"{single_line}\"")
}

/// Take up to `max_sentences` sentences from `text`, capped at `max_chars`.
fn first_sentences(text: &str, max_sentences: usize, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    let mut sentences = 0;
    for ch in normalized.chars() {
        out.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            sentences += 1;
            if sentences >= max_sentences {
                break;
            }
        }
        if out.len() >= max_chars {
            break;
        }
    }
    out.trim().to_string()
}

/// Truncate `text` to at most `max_chars`, preferring the last whitespace
/// boundary before the limit, and appending an ellipsis marker when cut.
fn truncate_on_boundary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut end = max_chars;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let slice = &text[..end];
    let cut = slice.rfind(char::is_whitespace).unwrap_or(end);
    format!("{} …", slice[..cut].trim_end())
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{PaperMetadata, PaperReference, PaperSection};

    fn section(kind: PaperSectionKind, heading: &str, text: &str) -> PaperSection {
        PaperSection {
            id: heading.to_lowercase(),
            heading: heading.to_string(),
            kind: Some(kind),
            level: 1,
            text: text.to_string(),
            subsections: Vec::new(),
        }
    }

    fn grobid_structure() -> PaperStructure {
        PaperStructure {
            item_key: "ABCD1234".to_string(),
            attachment_key: Some("PDF5678".to_string()),
            metadata: PaperMetadata {
                title: Some("Attention Is All You Need".to_string()),
                authors: vec!["Ashish Vaswani".to_string(), "Noam Shazeer".to_string()],
                abstract_note: Some(
                    "We propose the Transformer, a model based solely on attention. \
                     It dispenses with recurrence and convolutions entirely. \
                     Experiments show it is superior in quality."
                        .to_string(),
                ),
                doi: Some("10.5555/3295222.3295349".to_string()),
                year: Some("2017".to_string()),
            },
            sections: vec![
                section(
                    PaperSectionKind::Method,
                    "Model",
                    "The Transformer uses stacked self-attention.",
                ),
                section(
                    PaperSectionKind::Design,
                    "Architecture",
                    "Encoder-decoder with multi-head attention.",
                ),
                section(
                    PaperSectionKind::Evaluation,
                    "Results",
                    "BLEU improved on WMT 2014.",
                ),
                section(
                    PaperSectionKind::Introduction,
                    "Intro",
                    "Recurrent models dominate.",
                ),
            ],
            references: vec![PaperReference {
                id: "b0".to_string(),
                raw: "Bahdanau et al. Neural machine translation.".to_string(),
                authors: vec!["Dzmitry Bahdanau".to_string()],
                title: Some(
                    "Neural machine translation by jointly learning to align and translate"
                        .to_string(),
                ),
                year: Some("2015".to_string()),
                doi: None,
            }],
            figures: Vec::new(),
            source: PaperStructureSource::Grobid,
        }
    }

    #[test]
    fn scaffold_renders_frontmatter_and_mapped_sections() {
        let payload = build_skill_scaffold(&grobid_structure());
        assert_eq!(payload.name, "attention-is-all-you-need");
        assert!(
            payload
                .description
                .starts_with("We propose the Transformer")
        );

        let md = &payload.markdown;
        assert!(md.starts_with("---\nname: attention-is-all-you-need\n"));
        assert!(md.contains("description: \"We propose the Transformer"));
        assert!(md.contains("## When to use"));
        assert!(md.contains("## Method"));
        // Method + Design sections are merged under Method.
        assert!(md.contains("stacked self-attention"));
        assert!(md.contains("multi-head attention"));
        assert!(md.contains("## Evaluation"));
        assert!(md.contains("BLEU improved"));
        assert!(md.contains("## Key references"));
        assert!(md.contains("Neural machine translation by jointly learning"));
        assert!(md.contains("<!-- source: grobid"));
        // Introduction has no target mapping and must be omitted.
        assert!(!md.contains("Recurrent models dominate"));
        // No empty Limitations section.
        assert!(!md.contains("## Limitations"));
    }

    #[test]
    fn zotero_fulltext_provenance_and_templated_description() {
        let mut structure = grobid_structure();
        structure.source = PaperStructureSource::ZoteroFulltext;
        structure.metadata.abstract_note = None;
        let payload = build_skill_scaffold(&structure);
        assert!(payload.description.contains("Attention Is All You Need"));
        assert!(payload.markdown.contains("<!-- source: zotero_fulltext"));
    }

    #[test]
    fn slugify_handles_punctuation_and_empty() {
        assert_eq!(
            slugify("Deep Learning: A Review!"),
            "deep-learning-a-review"
        );
        assert_eq!(slugify("   "), "paper-skill");
        assert_eq!(slugify("---"), "paper-skill");
    }

    #[test]
    fn yaml_quote_escapes_quotes_and_newlines() {
        assert_eq!(yaml_quote("a: \"b\"\nc"), "\"a: \\\"b\\\" c\"");
    }

    #[test]
    fn missing_title_falls_back_to_item_key_slug() {
        let mut structure = grobid_structure();
        structure.metadata.title = None;
        let payload = build_skill_scaffold(&structure);
        assert_eq!(payload.name, "abcd1234");
    }
}
