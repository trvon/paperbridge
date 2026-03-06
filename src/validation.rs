use crate::models::{
    CollectionUpdateRequest, CollectionWriteRequest, CreatorInput, DeleteCollectionRequest,
    DeleteItemRequest, ItemUpdateRequest, ItemWriteRequest, ValidationIssue, ValidationIssueLevel,
    ValidationReport,
};

pub fn validate_collection_request(req: &CollectionWriteRequest) -> ValidationReport {
    let mut issues = Vec::new();

    if req.name.trim().is_empty() {
        issues.push(error("name", "collection name cannot be empty"));
    }

    ValidationReport {
        valid: !issues
            .iter()
            .any(|i| i.level == ValidationIssueLevel::Error),
        issues,
    }
}

pub fn validate_item_request(req: &ItemWriteRequest) -> ValidationReport {
    let mut issues = Vec::new();

    if req.item_type.trim().is_empty() {
        issues.push(error("item_type", "item_type cannot be empty"));
    }

    if let Some(title) = req.title.as_deref()
        && title.trim().is_empty()
    {
        issues.push(error("title", "title cannot be blank when provided"));
    }

    if req.item_type == "journalArticle" && req.title.as_deref().unwrap_or("").trim().is_empty() {
        issues.push(error(
            "title",
            "journalArticle items should include a title",
        ));
    }

    for (idx, creator) in req.creators.iter().enumerate() {
        validate_creator(creator, idx, &mut issues);
    }

    if let Some(doi) = req.doi.as_deref()
        && !doi.trim().is_empty()
        && !looks_like_doi(doi)
    {
        issues.push(warning("doi", "DOI does not match a common DOI pattern"));
    }

    if let Some(isbn) = req.isbn.as_deref()
        && !isbn.trim().is_empty()
        && !looks_like_isbn(isbn)
    {
        issues.push(warning(
            "isbn",
            "ISBN does not look like a valid 10 or 13 digit ISBN",
        ));
    }

    for (idx, tag) in req.tags.iter().enumerate() {
        if tag.tag.trim().is_empty() {
            issues.push(error(&format!("tags[{idx}]"), "tag text cannot be empty"));
        }
    }

    ValidationReport {
        valid: !issues
            .iter()
            .any(|i| i.level == ValidationIssueLevel::Error),
        issues,
    }
}

pub fn validate_collection_update_request(req: &CollectionUpdateRequest) -> ValidationReport {
    let mut issues = Vec::new();

    if req.key.trim().is_empty() {
        issues.push(error("key", "collection key cannot be empty"));
    }

    if let Some(name) = req.name.as_deref()
        && name.trim().is_empty()
    {
        issues.push(error(
            "name",
            "collection name cannot be blank when provided",
        ));
    }

    ValidationReport {
        valid: !issues
            .iter()
            .any(|i| i.level == ValidationIssueLevel::Error),
        issues,
    }
}

pub fn validate_item_update_request(req: &ItemUpdateRequest) -> ValidationReport {
    let mut issues = Vec::new();

    if req.key.trim().is_empty() {
        issues.push(error("key", "item key cannot be empty"));
    }

    if let Some(item_type) = req.item_type.as_deref()
        && item_type.trim().is_empty()
    {
        issues.push(error(
            "item_type",
            "item_type cannot be blank when provided",
        ));
    }

    if let Some(title) = req.title.as_deref()
        && title.trim().is_empty()
    {
        issues.push(error("title", "title cannot be blank when provided"));
    }

    if let Some(creators) = req.creators.as_ref() {
        for (idx, creator) in creators.iter().enumerate() {
            validate_creator(creator, idx, &mut issues);
        }
    }

    if let Some(doi) = req.doi.as_deref()
        && !doi.trim().is_empty()
        && !looks_like_doi(doi)
    {
        issues.push(warning("doi", "DOI does not match a common DOI pattern"));
    }

    if let Some(isbn) = req.isbn.as_deref()
        && !isbn.trim().is_empty()
        && !looks_like_isbn(isbn)
    {
        issues.push(warning(
            "isbn",
            "ISBN does not look like a valid 10 or 13 digit ISBN",
        ));
    }

    if let Some(tags) = req.tags.as_ref() {
        for (idx, tag) in tags.iter().enumerate() {
            if tag.tag.trim().is_empty() {
                issues.push(error(&format!("tags[{idx}]"), "tag text cannot be empty"));
            }
        }
    }

    ValidationReport {
        valid: !issues
            .iter()
            .any(|i| i.level == ValidationIssueLevel::Error),
        issues,
    }
}

pub fn validate_delete_collection_request(req: &DeleteCollectionRequest) -> ValidationReport {
    validate_required_key(&req.key, "key")
}

pub fn validate_delete_item_request(req: &DeleteItemRequest) -> ValidationReport {
    validate_required_key(&req.key, "key")
}

fn validate_required_key(key: &str, field: &str) -> ValidationReport {
    let mut issues = Vec::new();
    if key.trim().is_empty() {
        issues.push(error(field, "key cannot be empty"));
    }

    ValidationReport {
        valid: !issues
            .iter()
            .any(|i| i.level == ValidationIssueLevel::Error),
        issues,
    }
}

fn validate_creator(creator: &CreatorInput, idx: usize, issues: &mut Vec<ValidationIssue>) {
    if creator.creator_type.trim().is_empty() {
        issues.push(error(
            &format!("creators[{idx}].creator_type"),
            "creator_type cannot be empty",
        ));
    }

    let has_single = creator
        .name
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty());
    let has_split = creator
        .last_name
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty())
        || creator
            .first_name
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty());

    if !has_single && !has_split {
        issues.push(error(
            &format!("creators[{idx}]"),
            "creator must include either name or first/last name",
        ));
    }
}

fn looks_like_doi(value: &str) -> bool {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("10.") && lower.contains('/')
}

fn looks_like_isbn(value: &str) -> bool {
    let digits: String = value
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == 'X' || *c == 'x')
        .collect();
    digits.len() == 10 || digits.len() == 13
}

fn error(field: &str, message: &str) -> ValidationIssue {
    ValidationIssue {
        level: ValidationIssueLevel::Error,
        field: field.to_string(),
        message: message.to_string(),
    }
}

fn warning(field: &str, message: &str) -> ValidationIssue {
    ValidationIssue {
        level: ValidationIssueLevel::Warning,
        field: field.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_collection_rejects_empty_name() {
        let report = validate_collection_request(&CollectionWriteRequest {
            name: "   ".to_string(),
            parent_collection: None,
        });
        assert!(!report.valid);
        assert_eq!(report.issues[0].field, "name");
    }

    #[test]
    fn validate_item_flags_missing_title_and_bad_doi() {
        let report = validate_item_request(&ItemWriteRequest {
            item_type: "journalArticle".to_string(),
            title: None,
            creators: vec![CreatorInput {
                creator_type: "author".to_string(),
                first_name: Some("Ada".to_string()),
                last_name: Some("Lovelace".to_string()),
                name: None,
            }],
            abstract_note: None,
            date: None,
            url: None,
            doi: Some("bad-doi".to_string()),
            isbn: None,
            tags: vec![],
            collections: vec![],
            extra: None,
            parent_item: None,
        });
        assert!(!report.valid);
        assert!(report.issues.iter().any(|i| i.field == "title"));
        assert!(report.issues.iter().any(|i| i.field == "doi"));
    }

    #[test]
    fn validate_item_update_requires_key() {
        let report = validate_item_update_request(&ItemUpdateRequest {
            key: " ".to_string(),
            version: None,
            item_type: None,
            title: Some("Updated".to_string()),
            creators: None,
            abstract_note: None,
            date: None,
            url: None,
            doi: None,
            isbn: None,
            tags: None,
            collections: None,
            extra: None,
            parent_item: None,
            clear_parent: false,
        });
        assert!(!report.valid);
        assert_eq!(report.issues[0].field, "key");
    }

    #[test]
    fn validate_delete_collection_requires_key() {
        let report = validate_delete_collection_request(&DeleteCollectionRequest {
            key: " ".to_string(),
            version: None,
        });
        assert!(!report.valid);
    }
}
