use crate::models::{
    FulltextContent, ItemDetail, PaperMetadata, PaperSection, PaperStructure, PaperStructureSource,
};

pub fn build(item: &ItemDetail, fulltext: &FulltextContent) -> PaperStructure {
    let metadata = PaperMetadata {
        title: Some(item.title.clone()).filter(|s| !s.trim().is_empty()),
        authors: item.creators.clone(),
        abstract_note: item.abstract_note.clone(),
        doi: extract_doi(item),
        year: item.year.clone(),
    };

    let sections = if fulltext.content.trim().is_empty() {
        Vec::new()
    } else {
        vec![PaperSection {
            id: "body".to_string(),
            heading: "Body".to_string(),
            level: 1,
            text: fulltext.content.clone(),
            subsections: Vec::new(),
        }]
    };

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
        assert_eq!(structure.sections.len(), 1);
        assert_eq!(structure.sections[0].text, "Hello world.");
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
        assert!(structure.sections.is_empty());
    }

    #[test]
    fn doi_from_extra_line() {
        let mut item = sample_item();
        item.url = None;
        item.extra = Some("DOI: 10.1234/abc\nother: field".to_string());
        let structure = build(&item, &sample_fulltext());
        assert_eq!(structure.metadata.doi.as_deref(), Some("10.1234/abc"));
    }
}
