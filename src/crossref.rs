use crate::error::{Result, ZoteroMcpError};
use crate::models::CrossrefWork;
use crate::validation::looks_like_doi;
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.crossref.org";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";

#[derive(Clone)]
pub struct CrossrefClient {
    client: Client,
    base_url: String,
}

impl CrossrefClient {
    pub fn new(base_url: Option<&str>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(USER_AGENT)
            .build()
            .expect("failed to build reqwest client");

        Self {
            client,
            base_url: base_url
                .unwrap_or(DEFAULT_BASE_URL)
                .trim_end_matches('/')
                .to_string(),
        }
    }

    pub async fn resolve_doi(&self, doi: &str) -> Result<CrossrefWork> {
        let trimmed = doi.trim();
        if !looks_like_doi(trimmed) {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "'{trimmed}' does not look like a valid DOI"
            )));
        }

        let encoded = urlencoding::encode(trimmed);
        let url = format!("{}/works/{encoded}", self.base_url);

        let response = self.client.get(&url).send().await?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(ZoteroMcpError::Api {
                status: 404,
                message: format!("DOI '{trimmed}' not found in Crossref"),
            });
        }
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<no body>"));
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("Crossref API error: {body}"),
            });
        }

        let raw: RawCrossrefResponse = response.json().await?;
        Ok(convert_message(trimmed, raw.message))
    }
}

impl std::fmt::Debug for CrossrefClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrossrefClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

// --- Raw Crossref wire types (private) ---

#[derive(Debug, Deserialize)]
struct RawCrossrefResponse {
    message: RawCrossrefMessage,
}

#[derive(Debug, Deserialize)]
struct RawCrossrefMessage {
    #[serde(rename = "DOI")]
    doi: Option<String>,
    title: Option<Vec<String>>,
    author: Option<Vec<RawAuthor>>,
    #[serde(rename = "container-title")]
    container_title: Option<Vec<String>>,
    #[serde(rename = "published-print")]
    published_print: Option<RawDateField>,
    #[serde(rename = "published-online")]
    published_online: Option<RawDateField>,
    #[serde(rename = "abstract")]
    abstract_text: Option<String>,
    #[serde(rename = "URL")]
    url: Option<String>,
    publisher: Option<String>,
    #[serde(rename = "type")]
    work_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawAuthor {
    given: Option<String>,
    family: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawDateField {
    #[serde(rename = "date-parts")]
    date_parts: Option<Vec<Vec<Option<u32>>>>,
}

// --- Conversion ---

fn convert_message(doi_input: &str, msg: RawCrossrefMessage) -> CrossrefWork {
    let doi = msg.doi.unwrap_or_else(|| doi_input.to_string());

    let title = msg.title.and_then(|v| v.into_iter().next());

    let authors = msg
        .author
        .unwrap_or_default()
        .into_iter()
        .map(format_author)
        .collect();

    let year = extract_year(msg.published_print.as_ref())
        .or_else(|| extract_year(msg.published_online.as_ref()));

    let journal = msg.container_title.and_then(|v| v.into_iter().next());

    let abstract_note = msg.abstract_text.map(|s| strip_xml_tags(&s));

    CrossrefWork {
        doi,
        title,
        authors,
        year,
        journal,
        abstract_note,
        url: msg.url,
        publisher: msg.publisher,
        item_type: msg.work_type,
    }
}

fn format_author(a: RawAuthor) -> String {
    if let Some(name) = a.name {
        return name;
    }
    match (a.family.as_deref(), a.given.as_deref()) {
        (Some(f), Some(g)) => format!("{f}, {g}"),
        (Some(f), None) => f.to_string(),
        (None, Some(g)) => g.to_string(),
        (None, None) => String::new(),
    }
}

fn extract_year(field: Option<&RawDateField>) -> Option<String> {
    field?
        .date_parts
        .as_ref()?
        .first()?
        .first()?
        .map(|y| y.to_string())
}

fn strip_xml_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut inside_tag = false;
    for ch in input.chars() {
        if ch == '<' {
            inside_tag = true;
        } else if ch == '>' {
            inside_tag = false;
        } else if !inside_tag {
            out.push(ch);
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_xml_tags_removes_jats() {
        let input = "<jats:p>This is <jats:italic>italic</jats:italic> text.</jats:p>";
        assert_eq!(strip_xml_tags(input), "This is italic text.");
    }

    #[test]
    fn strip_xml_tags_passthrough_plain_text() {
        assert_eq!(strip_xml_tags("no tags here"), "no tags here");
    }

    #[test]
    fn format_author_full_name() {
        let a = RawAuthor {
            given: Some("Ed".to_string()),
            family: Some("Boyden".to_string()),
            name: None,
        };
        assert_eq!(format_author(a), "Boyden, Ed");
    }

    #[test]
    fn format_author_single_name() {
        let a = RawAuthor {
            given: None,
            family: None,
            name: Some("World Health Organization".to_string()),
        };
        assert_eq!(format_author(a), "World Health Organization");
    }

    #[test]
    fn format_author_family_only() {
        let a = RawAuthor {
            given: None,
            family: Some("Smith".to_string()),
            name: None,
        };
        assert_eq!(format_author(a), "Smith");
    }

    #[test]
    fn extract_year_from_date_parts() {
        let field = RawDateField {
            date_parts: Some(vec![vec![Some(2023), Some(8), Some(14)]]),
        };
        assert_eq!(extract_year(Some(&field)), Some("2023".to_string()));
    }

    #[test]
    fn extract_year_missing() {
        let field = RawDateField {
            date_parts: Some(vec![vec![]]),
        };
        assert_eq!(extract_year(Some(&field)), None);
        assert_eq!(extract_year(None), None);
    }

    #[test]
    fn convert_full_crossref_message() {
        let json = serde_json::json!({
            "status": "ok",
            "message": {
                "DOI": "10.1038/nature12373",
                "title": ["Optical control of mammalian endogenous transcription and epigenetic states"],
                "author": [
                    {"given": "Silvana", "family": "Konermann"},
                    {"given": "Mark D.", "family": "Brigham"}
                ],
                "container-title": ["Nature"],
                "published-print": {"date-parts": [[2013, 8, 22]]},
                "abstract": "<jats:p>A method is described.</jats:p>",
                "URL": "http://dx.doi.org/10.1038/nature12373",
                "publisher": "Springer Science and Business Media LLC",
                "type": "journal-article"
            }
        });

        let raw: RawCrossrefResponse = serde_json::from_value(json).unwrap();
        let work = convert_message("10.1038/nature12373", raw.message);

        assert_eq!(work.doi, "10.1038/nature12373");
        assert_eq!(
            work.title.as_deref(),
            Some("Optical control of mammalian endogenous transcription and epigenetic states")
        );
        assert_eq!(work.authors, vec!["Konermann, Silvana", "Brigham, Mark D."]);
        assert_eq!(work.year.as_deref(), Some("2013"));
        assert_eq!(work.journal.as_deref(), Some("Nature"));
        assert_eq!(
            work.abstract_note.as_deref(),
            Some("A method is described.")
        );
        assert_eq!(
            work.url.as_deref(),
            Some("http://dx.doi.org/10.1038/nature12373")
        );
        assert_eq!(
            work.publisher.as_deref(),
            Some("Springer Science and Business Media LLC")
        );
        assert_eq!(work.item_type.as_deref(), Some("journal-article"));
    }

    #[test]
    fn convert_minimal_crossref_message() {
        let json = serde_json::json!({
            "status": "ok",
            "message": {
                "DOI": "10.1234/test",
                "type": "other"
            }
        });

        let raw: RawCrossrefResponse = serde_json::from_value(json).unwrap();
        let work = convert_message("10.1234/test", raw.message);

        assert_eq!(work.doi, "10.1234/test");
        assert!(work.title.is_none());
        assert!(work.authors.is_empty());
        assert!(work.year.is_none());
        assert!(work.journal.is_none());
        assert!(work.abstract_note.is_none());
        assert!(work.url.is_none());
        assert!(work.publisher.is_none());
        assert_eq!(work.item_type.as_deref(), Some("other"));
    }

    #[test]
    fn resolve_doi_rejects_invalid_format() {
        let client = CrossrefClient::new(None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.resolve_doi("not-a-doi")).unwrap_err();
        assert!(err.to_string().contains("does not look like a valid DOI"));
    }

    #[test]
    fn convert_message_uses_published_online_fallback() {
        let json = serde_json::json!({
            "status": "ok",
            "message": {
                "DOI": "10.1234/online-only",
                "published-online": {"date-parts": [[2024, 1, 15]]}
            }
        });

        let raw: RawCrossrefResponse = serde_json::from_value(json).unwrap();
        let work = convert_message("10.1234/online-only", raw.message);
        assert_eq!(work.year.as_deref(), Some("2024"));
    }
}
