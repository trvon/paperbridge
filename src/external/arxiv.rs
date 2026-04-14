use crate::error::{Result, ZoteroMcpError};
use crate::models::{PaperHit, PaperSource};
use quick_xml::Reader;
use quick_xml::events::Event;
use reqwest::Client;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::sleep;

const DEFAULT_BASE_URL: &str = "http://export.arxiv.org/api/query";
const USER_AGENT: &str = "paperbridge/0.1.0 (mailto:paperbridge@users.noreply.github.com)";
const MIN_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct ArxivClient {
    client: Client,
    base_url: String,
    last_request: Arc<Mutex<Option<Instant>>>,
}

impl ArxivClient {
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
            last_request: Arc::new(Mutex::new(None)),
        }
    }

    async fn throttle(&self) {
        let mut guard = self.last_request.lock().await;
        if let Some(prev) = *guard {
            let elapsed = prev.elapsed();
            if elapsed < MIN_INTERVAL {
                sleep(MIN_INTERVAL - elapsed).await;
            }
        }
        *guard = Some(Instant::now());
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "arXiv search query must not be empty".to_string(),
            ));
        }

        self.throttle().await;

        let encoded = urlencoding::encode(trimmed);
        let url = format!(
            "{}?search_query=all:{encoded}&max_results={limit}",
            self.base_url
        );

        let response = self.client.get(&url).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("arXiv API error at {url}"),
            });
        }

        let body = response.text().await?;
        parse_atom_feed(&body)
    }
}

impl std::fmt::Debug for ArxivClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArxivClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

fn parse_atom_feed(xml: &str) -> Result<Vec<PaperHit>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut hits: Vec<PaperHit> = Vec::new();
    let mut current: Option<EntryBuilder> = None;
    let mut path: Vec<String> = Vec::new();
    let mut text_buf = String::new();
    let mut current_author: Option<String> = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let local = local_name(&name).to_string();
                if local == "entry" {
                    current = Some(EntryBuilder::default());
                } else if local == "author" && current.is_some() {
                    current_author = Some(String::new());
                }
                path.push(local);
                text_buf.clear();
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let local = local_name(&name);
                if local == "link" && current.is_some() {
                    let mut rel = None;
                    let mut href = None;
                    let mut typ = None;
                    for attr in e.attributes().flatten() {
                        let k = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        let v = attr.unescape_value().ok().map(|c| c.to_string());
                        match k.as_str() {
                            "rel" => rel = v,
                            "href" => href = v,
                            "type" => typ = v,
                            _ => {}
                        }
                    }
                    if let Some(entry) = current.as_mut() {
                        apply_link(entry, (rel, href, typ));
                    }
                }
            }
            Ok(Event::Text(e)) => {
                let t = e.unescape().map(|c| c.to_string()).unwrap_or_default();
                text_buf.push_str(&t);
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let local = local_name(&name).to_string();
                if let Some(entry) = current.as_mut() {
                    match local.as_str() {
                        "id" if path_ends_with(&path, &["entry", "id"]) => {
                            entry.id = Some(text_buf.trim().to_string());
                        }
                        "title" if path_ends_with(&path, &["entry", "title"]) => {
                            entry.title =
                                Some(text_buf.split_whitespace().collect::<Vec<_>>().join(" "));
                        }
                        "summary" if path_ends_with(&path, &["entry", "summary"]) => {
                            entry.summary =
                                Some(text_buf.split_whitespace().collect::<Vec<_>>().join(" "));
                        }
                        "published" if path_ends_with(&path, &["entry", "published"]) => {
                            entry.published = Some(text_buf.trim().to_string());
                        }
                        "name" if path_ends_with(&path, &["entry", "author", "name"]) => {
                            if let Some(author) = current_author.as_mut() {
                                author.push_str(text_buf.trim());
                            }
                        }
                        "author" if path_ends_with(&path, &["entry", "author"]) => {
                            if let Some(author) = current_author.take()
                                && !author.is_empty()
                            {
                                entry.authors.push(author);
                            }
                        }
                        _ => {}
                    }
                }

                if local == "entry"
                    && let Some(entry) = current.take()
                    && let Some(hit) = entry.build()
                {
                    hits.push(hit);
                }

                path.pop();
                text_buf.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(ZoteroMcpError::Serde(format!("arXiv XML parse error: {e}")));
            }
            _ => {}
        }
    }

    Ok(hits)
}

fn local_name(qualified: &str) -> &str {
    qualified.rsplit(':').next().unwrap_or(qualified)
}

fn path_ends_with(path: &[String], suffix: &[&str]) -> bool {
    if path.len() < suffix.len() {
        return false;
    }
    let offset = path.len() - suffix.len();
    suffix
        .iter()
        .enumerate()
        .all(|(i, s)| path[offset + i] == *s)
}

#[derive(Default)]
struct EntryBuilder {
    id: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    published: Option<String>,
    authors: Vec<String>,
    abs_url: Option<String>,
    pdf_url: Option<String>,
}

fn apply_link(entry: &mut EntryBuilder, link: (Option<String>, Option<String>, Option<String>)) {
    let (rel, href, typ) = link;
    if let Some(href) = href {
        if typ.as_deref() == Some("application/pdf") {
            entry.pdf_url = Some(href);
        } else if rel.as_deref() == Some("alternate") {
            entry.abs_url = Some(href);
        }
    }
}

impl EntryBuilder {
    fn build(self) -> Option<PaperHit> {
        let title = self.title?;
        let id = self.id;
        let arxiv_id = id.as_ref().and_then(|s| {
            s.rsplit('/').next().map(|tail| {
                // Strip version suffix (e.g., v1, v2)
                if let Some(idx) = tail.rfind('v') {
                    let (base, ver) = tail.split_at(idx);
                    if ver[1..].chars().all(|c| c.is_ascii_digit()) {
                        return base.to_string();
                    }
                }
                tail.to_string()
            })
        });
        let year = self.published.as_deref().and_then(|p| {
            p.split('-').next().and_then(|s| {
                if s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()) {
                    Some(s.to_string())
                } else {
                    None
                }
            })
        });

        Some(PaperHit {
            source: PaperSource::Arxiv,
            title,
            authors: self.authors,
            year,
            doi: None,
            arxiv_id,
            abstract_note: self.summary,
            url: self.abs_url.or(id),
            pdf_url: self.pdf_url,
            venue: Some("arXiv".to_string()),
            citation_count: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_atom_feed_extracts_entries() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2301.00001v2</id>
    <published>2023-01-02T10:00:00Z</published>
    <title>Sample Paper Title</title>
    <summary>This is the abstract.</summary>
    <author><name>Alice Smith</name></author>
    <author><name>Bob Jones</name></author>
    <link href="http://arxiv.org/abs/2301.00001v2" rel="alternate" type="text/html"/>
    <link title="pdf" href="http://arxiv.org/pdf/2301.00001v2" rel="related" type="application/pdf"/>
  </entry>
</feed>"#;
        let hits = parse_atom_feed(xml).unwrap();
        assert_eq!(hits.len(), 1);
        let h = &hits[0];
        assert_eq!(h.title, "Sample Paper Title");
        assert_eq!(h.authors, vec!["Alice Smith", "Bob Jones"]);
        assert_eq!(h.year.as_deref(), Some("2023"));
        assert_eq!(h.arxiv_id.as_deref(), Some("2301.00001"));
        assert_eq!(
            h.pdf_url.as_deref(),
            Some("http://arxiv.org/pdf/2301.00001v2")
        );
        assert_eq!(h.url.as_deref(), Some("http://arxiv.org/abs/2301.00001v2"));
        assert_eq!(h.abstract_note.as_deref(), Some("This is the abstract."));
    }

    #[test]
    fn parse_atom_feed_empty_returns_empty_vec() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>no results</title></feed>"#;
        let hits = parse_atom_feed(xml).unwrap();
        assert_eq!(hits.len(), 0);
    }

    #[test]
    fn search_rejects_empty_query() {
        let client = ArxivClient::new(None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.search("  ", 5)).unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[tokio::test]
    async fn search_hits_endpoint_and_parses_atom() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let atom = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <id>http://arxiv.org/abs/2401.00001v1</id>
    <published>2024-01-05T10:00:00Z</published>
    <title>Mock Paper</title>
    <summary>Mock abstract.</summary>
    <author><name>Jane Doe</name></author>
    <link href="http://arxiv.org/abs/2401.00001v1" rel="alternate" type="text/html"/>
    <link title="pdf" href="http://arxiv.org/pdf/2401.00001v1" rel="related" type="application/pdf"/>
  </entry>
</feed>"#;
        Mock::given(method("GET"))
            .and(query_param("search_query", "all:quantum"))
            .and(query_param("max_results", "3"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(atom)
                    .append_header("content-type", "application/atom+xml"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = ArxivClient::new(Some(&server.uri()));
        let hits = client.search("quantum", 3).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Mock Paper");
        assert_eq!(hits[0].authors, vec!["Jane Doe"]);
        assert_eq!(hits[0].arxiv_id.as_deref(), Some("2401.00001"));
    }

    #[tokio::test]
    async fn search_returns_api_error_on_non_success_status() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client = ArxivClient::new(Some(&server.uri()));
        let err = client.search("q", 1).await.unwrap_err();
        match err {
            ZoteroMcpError::Api { status, .. } => assert_eq!(status, 503),
            other => panic!("expected Api error, got {other:?}"),
        }
    }
}
