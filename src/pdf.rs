use crate::chunking;
use crate::models::{AttachmentSummary, FulltextContent, ItemVoxPayload, VoxTextPayload};

pub fn prepare_vox_payload(source: &str, text: &str, max_chars: usize) -> VoxTextPayload {
    let chunks = chunking::split_for_tts(text, max_chars);
    VoxTextPayload {
        source: source.to_string(),
        chunk_count: chunks.len(),
        chunks,
    }
}

pub fn prepare_vox_payload_from_fulltext(
    source: &str,
    fulltext: &FulltextContent,
    max_chars: usize,
) -> VoxTextPayload {
    prepare_vox_payload(source, &fulltext.content, max_chars)
}

pub fn select_attachment_for_reading<'a>(
    attachments: &'a [AttachmentSummary],
    preferred_key: Option<&str>,
) -> Option<&'a AttachmentSummary> {
    if let Some(key) = preferred_key
        && let Some(found) = attachments.iter().find(|a| a.key == key)
    {
        return Some(found);
    }

    attachments
        .iter()
        .find(|a| is_pdf_attachment(a))
        .or_else(|| attachments.first())
}

pub fn build_item_vox_payload(
    item_key: &str,
    item_title: &str,
    attachment: &AttachmentSummary,
    fulltext: &FulltextContent,
    max_chars: usize,
) -> ItemVoxPayload {
    let source = format!("item:{item_key}/attachment:{}", attachment.key);
    let vox = prepare_vox_payload_from_fulltext(&source, fulltext, max_chars);

    ItemVoxPayload {
        item_key: item_key.to_string(),
        item_title: item_title.to_string(),
        attachment: attachment.clone(),
        indexed_pages: fulltext.indexed_pages,
        total_pages: fulltext.total_pages,
        indexed_chars: fulltext.indexed_chars,
        total_chars: fulltext.total_chars,
        vox,
    }
}

fn is_pdf_attachment(attachment: &AttachmentSummary) -> bool {
    if let Some(content_type) = attachment.content_type.as_deref()
        && content_type.eq_ignore_ascii_case("application/pdf")
    {
        return true;
    }

    attachment
        .path
        .as_deref()
        .map(|path| path.to_ascii_lowercase().contains(".pdf"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_counts_chunks() {
        let payload = prepare_vox_payload("manual", "Hello. World.", 20);
        assert_eq!(payload.chunk_count, payload.chunks.len());
        assert!(!payload.chunks.is_empty());
    }

    #[test]
    fn select_attachment_prefers_explicit_key() {
        let attachments = vec![
            AttachmentSummary {
                key: "A".to_string(),
                title: "text".to_string(),
                content_type: Some("text/plain".to_string()),
                path: None,
            },
            AttachmentSummary {
                key: "B".to_string(),
                title: "pdf".to_string(),
                content_type: Some("application/pdf".to_string()),
                path: None,
            },
        ];

        let selected = select_attachment_for_reading(&attachments, Some("A")).unwrap();
        assert_eq!(selected.key, "A");
    }

    #[test]
    fn select_attachment_prefers_pdf_when_no_explicit_key() {
        let attachments = vec![
            AttachmentSummary {
                key: "A".to_string(),
                title: "notes".to_string(),
                content_type: Some("text/plain".to_string()),
                path: None,
            },
            AttachmentSummary {
                key: "B".to_string(),
                title: "paper".to_string(),
                content_type: Some("application/pdf".to_string()),
                path: None,
            },
        ];

        let selected = select_attachment_for_reading(&attachments, None).unwrap();
        assert_eq!(selected.key, "B");
    }

    #[test]
    fn select_attachment_falls_back_to_first() {
        let attachments = vec![AttachmentSummary {
            key: "A".to_string(),
            title: "notes".to_string(),
            content_type: Some("text/plain".to_string()),
            path: None,
        }];

        let selected = select_attachment_for_reading(&attachments, None).unwrap();
        assert_eq!(selected.key, "A");
    }
}
