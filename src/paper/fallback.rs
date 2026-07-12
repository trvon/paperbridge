use crate::models::{
    FulltextContent, ItemDetail, PaperMetadata, PaperSection, PaperSectionKind, PaperStructure,
    PaperStructureSource,
};

pub fn build(item: &ItemDetail, fulltext: &FulltextContent) -> PaperStructure {
    let metadata = PaperMetadata {
        title: Some(item.title.clone()).filter(|s| !s.trim().is_empty()),
        authors: item.creators.clone(),
        abstract_note: item.abstract_note.clone(),
        doi: extract_doi(item),
        year: item.year.clone(),
    };

    let sections = build_sections(metadata.abstract_note.as_deref(), &fulltext.content);

    PaperStructure {
        item_key: item.key.clone(),
        attachment_key: Some(fulltext.item_key.clone()),
        metadata,
        sections,
        references: Vec::new(),
        figures: Vec::new(),
        source: PaperStructureSource::ZoteroFulltext,
    }
}

pub(crate) fn build_sections(abstract_note: Option<&str>, content: &str) -> Vec<PaperSection> {
    let abstract_note = abstract_note.map(str::trim).filter(|s| !s.is_empty());
    let mut sections = Vec::new();
    if let Some(abstract_note) = abstract_note {
        sections.push(PaperSection {
            id: "abstract".to_string(),
            heading: "Abstract".to_string(),
            kind: Some(PaperSectionKind::Abstract),
            level: 1,
            text: abstract_note.to_string(),
            subsections: Vec::new(),
        });
    }

    let mut body_sections = split_fulltext_sections(content);
    if abstract_note.is_some() {
        // Drop any body section that's clearly an abstract — either tagged
        // as such (split_fulltext_sections always populates kind) or with a
        // heading that reads "Abstract" verbatim. The heading-name check
        // exists as defense in depth so future upstream parsers that leave
        // `kind: None` don't reintroduce the duplicate.
        body_sections.retain(|section| {
            !matches!(section.kind, Some(PaperSectionKind::Abstract))
                && !section.heading.eq_ignore_ascii_case("abstract")
        });
    }
    sections.extend(body_sections);
    sections
}

fn split_fulltext_sections(content: &str) -> Vec<PaperSection> {
    let content = content.trim();
    if content.is_empty() {
        return Vec::new();
    }

    let mut sections = Vec::new();
    let mut current: Option<(String, PaperSectionKind, Vec<String>)> = None;
    let mut preamble = Vec::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some((heading, kind, remainder)) = classify_latex_heading(line) {
            flush_section(&mut sections, &mut current);
            current = Some((heading, kind, Vec::new()));
            if !remainder.is_empty()
                && let Some((_, _, lines)) = current.as_mut()
            {
                lines.push(remainder);
            }
            continue;
        }
        if line == "\\end{abstract}" {
            continue;
        }

        if matches!(
            current.as_ref().map(|(_, kind, _)| kind),
            Some(PaperSectionKind::References)
        ) {
            if let Some((_, _, lines)) = current.as_mut() {
                lines.push(line.to_string());
            }
        } else if let Some((heading, kind)) = classify_heading(line) {
            flush_section(&mut sections, &mut current);
            current = Some((heading, kind, Vec::new()));
        } else if let Some((_, _, lines)) = current.as_mut() {
            lines.push(line.to_string());
        } else {
            preamble.push(line.to_string());
        }
    }

    flush_section(&mut sections, &mut current);

    if sections.is_empty() {
        return vec![PaperSection {
            id: "body".to_string(),
            heading: "Body".to_string(),
            kind: Some(PaperSectionKind::Other),
            level: 1,
            text: content.to_string(),
            subsections: Vec::new(),
        }];
    }

    if !preamble.is_empty() {
        sections.insert(
            0,
            PaperSection {
                id: "body".to_string(),
                heading: "Body".to_string(),
                kind: Some(PaperSectionKind::Other),
                level: 1,
                text: collapse_lines(&preamble),
                subsections: Vec::new(),
            },
        );
    }

    sections
}

fn classify_latex_heading(line: &str) -> Option<(String, PaperSectionKind, String)> {
    if let Some(rest) = line.strip_prefix("\\begin{abstract}") {
        return Some((
            "Abstract".into(),
            PaperSectionKind::Abstract,
            rest.replace("\\end{abstract}", "").trim().to_string(),
        ));
    }
    for command in ["section", "subsection", "subsubsection"] {
        let prefix = format!("\\{command}{{");
        let Some(rest) = line.strip_prefix(&prefix) else {
            continue;
        };
        let end = rest.find('}')?;
        let heading = rest[..end].trim();
        let (_, kind) = classify_heading(heading)?;
        let remainder = strip_latex_label(rest[end + 1..].trim());
        return Some((canonical_heading(&kind, heading), kind, remainder));
    }
    None
}

fn strip_latex_label(text: &str) -> String {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("\\label{")
        && let Some(end) = rest.find('}')
    {
        return rest[end + 1..].trim().to_string();
    }
    text.to_string()
}

fn flush_section(
    sections: &mut Vec<PaperSection>,
    current: &mut Option<(String, PaperSectionKind, Vec<String>)>,
) {
    let Some((heading, kind, lines)) = current.take() else {
        return;
    };
    let id = unique_section_id(sections, &kind);
    sections.push(PaperSection {
        id,
        heading,
        kind: Some(kind),
        level: 1,
        text: collapse_lines(&lines),
        subsections: Vec::new(),
    });
}

fn classify_heading(line: &str) -> Option<(String, PaperSectionKind)> {
    let trimmed = line.trim();
    if trimmed.len() > 96 || trimmed.ends_with('.') || trimmed.contains(": ") {
        return None;
    }

    let display_heading = strip_ordering_prefix(trimmed).trim();
    let normalized = normalize_heading(trimmed);
    let heading_like = looks_like_section_heading(display_heading);
    let kind = match normalized.as_str() {
        "abstract" | "resumen" | "resumo" | "résumé" | "摘要" => PaperSectionKind::Abstract,
        "introduction" | "introducción" | "introduccion" | "introdução" | "introducao"
        | "einleitung" => PaperSectionKind::Introduction,
        "background" | "preliminaries" | "antecedentes" | "contexte" => {
            PaperSectionKind::Background
        }
        "related work"
        | "prior work"
        | "trabajos relacionados"
        | "état de l'art"
        | "estado del arte" => PaperSectionKind::RelatedWork,
        "method" | "methods" | "methodology" | "approach" | "proposed approach" | "método"
        | "metodo" | "metodología" | "metodologia" | "méthode" | "méthodologie" => {
            PaperSectionKind::Method
        }
        "design"
        | "system design"
        | "architecture"
        | "design and implementation"
        | "diseño"
        | "diseno"
        | "arquitectura"
        | "conception" => PaperSectionKind::Design,
        "implementation" | "implementación" | "implementacion" | "implémentation" => {
            PaperSectionKind::Implementation
        }
        "evaluation"
        | "experimental evaluation"
        | "experiments"
        | "experiment"
        | "empirical evaluation"
        | "evaluación"
        | "evaluacion"
        | "experimentos"
        | "évaluation" => PaperSectionKind::Evaluation,
        "results" | "findings" | "resultados" | "résultats" | "ergebnisse" => {
            PaperSectionKind::Results
        }
        "discussion" | "analysis" | "discusión" | "discusion" | "analyse" => {
            PaperSectionKind::Discussion
        }
        "limitations" | "threats to validity" | "limitaciones" | "limitations et menaces" => {
            PaperSectionKind::Limitations
        }
        "conclusion"
        | "conclusions"
        | "concluding remarks"
        | "conclusión"
        | "conclusion et perspectives"
        | "conclusao"
        | "conclusão" => PaperSectionKind::Conclusion,
        "acknowledgements" | "acknowledgments" | "agradecimientos" | "remerciements" => {
            PaperSectionKind::Acknowledgements
        }
        "references" | "bibliography" | "referencias" | "bibliografía" | "bibliografia"
        | "bibliographie" | "参考文献" => PaperSectionKind::References,
        "appendix" | "appendices" | "apéndice" | "apendice" | "annexe" => {
            PaperSectionKind::Appendix
        }
        _ if heading_like && normalized.contains("related work") => PaperSectionKind::RelatedWork,
        _ if heading_like && normalized.contains("implementation") => {
            PaperSectionKind::Implementation
        }
        _ if heading_like
            && (normalized.contains("evaluation") || normalized.contains("experiment")) =>
        {
            PaperSectionKind::Evaluation
        }
        _ if heading_like && normalized.contains("result") => PaperSectionKind::Results,
        _ if heading_like
            && (normalized.contains("design") || normalized.contains("architecture")) =>
        {
            PaperSectionKind::Design
        }
        _ if heading_like && normalized.contains("conclusion") => PaperSectionKind::Conclusion,
        _ => return None,
    };

    Some((canonical_heading(&kind, display_heading), kind))
}

fn normalize_heading(line: &str) -> String {
    let trimmed = line
        .trim()
        .trim_matches(|c: char| c == ':' || c == '-' || c == '—')
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c.is_whitespace())
        .trim();
    // Unicode-aware `to_lowercase` here (not `to_ascii_lowercase`): heading
    // text comes from upstream parsed PDFs and can contain accented characters
    // ("Résumé", "Méthode") that must compare equal to their lowercased forms
    // during classify_heading. The English-stopword check at line 276 uses
    // `to_ascii_lowercase` because it matches against ASCII-only literals.
    strip_ordering_prefix(trimmed).to_lowercase()
}

fn strip_ordering_prefix(line: &str) -> &str {
    let line = line.trim();
    if let Some((prefix, rest)) = line.split_once(['.', ')'])
        && !rest.trim().is_empty()
    {
        let prefix = prefix.trim();
        if prefix.len() == 1 && prefix.chars().all(|c| c.is_ascii_alphabetic()) {
            return rest.trim();
        }
        if !prefix.is_empty()
            && prefix
                .chars()
                .all(|c| matches!(c.to_ascii_uppercase(), 'I' | 'V' | 'X' | 'L' | 'C'))
        {
            return rest.trim();
        }
    }
    line
}

fn looks_like_section_heading(line: &str) -> bool {
    let words: Vec<&str> = line
        .split_whitespace()
        .filter(|word| word.chars().any(|c| c.is_alphabetic()))
        .collect();
    if words.is_empty() || words.len() > 9 {
        return false;
    }

    let alpha_count = line.chars().filter(|c| c.is_alphabetic()).count();
    let uppercase_count = line.chars().filter(|c| c.is_uppercase()).count();
    if alpha_count > 0 && uppercase_count * 100 / alpha_count >= 60 {
        return true;
    }

    words.iter().all(|word| {
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric());
        let lower = trimmed.to_ascii_lowercase();
        matches!(
            lower.as_str(),
            "and" | "or" | "of" | "the" | "a" | "an" | "for" | "to" | "with" | "in" | "on"
        ) || trimmed
            .chars()
            .next()
            .map(char::is_uppercase)
            .unwrap_or(false)
    })
}

fn canonical_heading(kind: &PaperSectionKind, original: &str) -> String {
    match kind {
        PaperSectionKind::Abstract => "Abstract",
        PaperSectionKind::Introduction => "Introduction",
        PaperSectionKind::Background => "Background",
        PaperSectionKind::RelatedWork => "Related Work",
        PaperSectionKind::Method => "Method",
        PaperSectionKind::Design => "Design",
        PaperSectionKind::Implementation => "Implementation",
        PaperSectionKind::Evaluation => "Evaluation",
        PaperSectionKind::Results => "Results",
        PaperSectionKind::Discussion => "Discussion",
        PaperSectionKind::Limitations => "Limitations",
        PaperSectionKind::Conclusion => "Conclusion",
        PaperSectionKind::Acknowledgements => "Acknowledgements",
        PaperSectionKind::References => "References",
        PaperSectionKind::Appendix => "Appendix",
        PaperSectionKind::Other => original,
    }
    .to_string()
}

fn unique_section_id(sections: &[PaperSection], kind: &PaperSectionKind) -> String {
    let base = section_id(kind);
    if !sections.iter().any(|section| section.id == base) {
        return base;
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !sections.iter().any(|section| section.id == candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn section_id(kind: &PaperSectionKind) -> String {
    match kind {
        PaperSectionKind::RelatedWork => "related_work",
        PaperSectionKind::Acknowledgements => "acknowledgements",
        PaperSectionKind::Other => "body",
        _ => {
            return format!("{kind:?}")
                .chars()
                .flat_map(char::to_lowercase)
                .collect();
        }
    }
    .to_string()
}

fn collapse_lines(lines: &[String]) -> String {
    lines
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn extract_doi(item: &ItemDetail) -> Option<String> {
    // Zotero stores DOI in extra or url frequently. Cheap pass: look in extra
    // for a "DOI: ..." line; otherwise try url if it looks like a doi.org link.
    if let Some(extra) = item.extra.as_deref() {
        for line in extra.lines() {
            if let Some(rest) = line.trim().strip_prefix("DOI:") {
                let doi = rest.trim();
                if !doi.is_empty() {
                    return Some(doi.to_string());
                }
            }
        }
    }
    if let Some(url) = item.url.as_deref()
        && let Some(idx) = url.find("doi.org/")
    {
        let doi = &url[idx + "doi.org/".len()..];
        if !doi.is_empty() {
            return Some(doi.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TagInput;

    fn sample_item() -> ItemDetail {
        ItemDetail {
            key: "ITEM1".to_string(),
            version: Some(7),
            item_type: "journalArticle".to_string(),
            title: "A Paper".to_string(),
            creators: vec!["Ada Lovelace".to_string()],
            year: Some("2024".to_string()),
            abstract_note: Some("Abstract here".to_string()),
            url: Some("https://doi.org/10.1000/xyz".to_string()),
            date: None,
            tags: Vec::<TagInput>::new(),
            collections: Vec::new(),
            extra: None,
            parent_item: None,
            attachments: Vec::new(),
        }
    }

    fn sample_fulltext() -> FulltextContent {
        FulltextContent {
            item_key: "ATT1".to_string(),
            content: "Hello world.".to_string(),
            indexed_pages: Some(1),
            total_pages: Some(1),
            indexed_chars: Some(12),
            total_chars: Some(12),
        }
    }

    #[test]
    fn build_populates_metadata() {
        let structure = build(&sample_item(), &sample_fulltext());
        assert_eq!(structure.item_key, "ITEM1");
        assert_eq!(structure.attachment_key.as_deref(), Some("ATT1"));
        assert_eq!(structure.metadata.title.as_deref(), Some("A Paper"));
        assert_eq!(structure.metadata.doi.as_deref(), Some("10.1000/xyz"));
        assert_eq!(structure.sections.len(), 2);
        assert_eq!(structure.sections[0].heading, "Abstract");
        assert_eq!(structure.sections[1].text, "Hello world.");
        assert!(matches!(
            structure.source,
            PaperStructureSource::ZoteroFulltext
        ));
    }

    #[test]
    fn empty_fulltext_yields_no_sections() {
        let mut ft = sample_fulltext();
        ft.content = String::new();
        let structure = build(&sample_item(), &ft);
        assert_eq!(structure.sections.len(), 1);
        assert_eq!(structure.sections[0].heading, "Abstract");
    }

    #[test]
    fn doi_from_extra_line() {
        let mut item = sample_item();
        item.url = None;
        item.extra = Some("DOI: 10.1234/abc\nother: field".to_string());
        let structure = build(&item, &sample_fulltext());
        assert_eq!(structure.metadata.doi.as_deref(), Some("10.1234/abc"));
    }

    #[test]
    fn fallback_splits_common_paper_sections() {
        let mut ft = sample_fulltext();
        ft.content = "\
Introduction
We frame the problem.
Design
We describe the system design.
Evaluation
We measure throughput.
Conclusion
We summarize."
            .to_string();

        let structure = build(&sample_item(), &ft);
        let headings: Vec<&str> = structure
            .sections
            .iter()
            .map(|section| section.heading.as_str())
            .collect();
        assert_eq!(
            headings,
            vec![
                "Abstract",
                "Introduction",
                "Design",
                "Evaluation",
                "Conclusion"
            ]
        );
        assert_eq!(structure.sections[2].kind, Some(PaperSectionKind::Design));
        assert_eq!(
            structure.sections[3].kind,
            Some(PaperSectionKind::Evaluation)
        );
    }

    #[test]
    fn fallback_dedupes_metadata_abstract_against_fulltext_abstract() {
        let mut ft = sample_fulltext();
        ft.content = "\
Abstract
Abstract here
Introduction
We frame the problem."
            .to_string();

        let structure = build(&sample_item(), &ft);
        let headings: Vec<&str> = structure
            .sections
            .iter()
            .map(|section| section.heading.as_str())
            .collect();
        assert_eq!(headings, vec!["Abstract", "Introduction"]);
        assert_eq!(structure.sections[0].text, "Abstract here");
    }

    #[test]
    fn fallback_recognizes_localized_section_headings() {
        let mut item = sample_item();
        item.abstract_note = None;
        let mut ft = sample_fulltext();
        ft.content = "\
Resumen
Breve resumen.
Introducción
Contexto.
Resultados
Hallazgos.
Conclusão
Encerramento.
参考文献
[1] Example."
            .to_string();

        let structure = build(&item, &ft);
        let headings: Vec<&str> = structure
            .sections
            .iter()
            .map(|section| section.heading.as_str())
            .collect();
        assert_eq!(
            headings,
            vec![
                "Abstract",
                "Introduction",
                "Results",
                "Conclusion",
                "References"
            ]
        );
        assert_eq!(structure.sections[0].kind, Some(PaperSectionKind::Abstract));
        assert_eq!(
            structure.sections[4].kind,
            Some(PaperSectionKind::References)
        );
    }

    #[test]
    fn fallback_recognizes_latex_abstract_and_sections() {
        let sections = build_sections(
            None,
            "\\begin{abstract}Graph detector summary.\\end{abstract}\n\\section{Introduction} \\label{sec:intro}\nContext.\n\\section{Evaluation}\nAUC was 0.92.",
        );

        assert_eq!(
            sections
                .iter()
                .map(|section| section.heading.as_str())
                .collect::<Vec<_>>(),
            vec!["Abstract", "Introduction", "Evaluation"]
        );
        assert_eq!(sections[0].text, "Graph detector summary.");
        assert!(sections[2].text.contains("AUC was 0.92"));
    }
}
