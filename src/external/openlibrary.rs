use crate::book::normalize_isbn;
use crate::error::{Result, ZoteroMcpError};
use crate::external::send_with_retry;
use crate::models::{PaperHit, PaperSource, WorkType};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://openlibrary.org";

fn user_agent() -> String {
    format!(
        "paperbridge/{} (mailto:paperbridge@users.noreply.github.com)",
        env!("CARGO_PKG_VERSION")
    )
}

#[derive(Clone)]
pub struct OpenLibraryClient {
    client: Client,
    base_url: String,
}

impl OpenLibraryClient {
    pub fn new(base_url: Option<&str>) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent(user_agent())
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            client,
            base_url: base_url
                .unwrap_or(DEFAULT_BASE_URL)
                .trim_end_matches('/')
                .to_string(),
        }
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<PaperHit>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(ZoteroMcpError::InvalidInput(
                "Open Library search query must not be empty".to_string(),
            ));
        }

        let encoded = urlencoding::encode(trimmed);
        let url = format!(
            "{}/search.json?q={encoded}&limit={limit}&fields=key,title,author_name,first_publish_year,isbn,publisher,ia,public_scan_b,first_sentence",
            self.base_url
        );

        let response = send_with_retry("openlibrary", self.client.get(&url)).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(ZoteroMcpError::Api {
                status: status.as_u16(),
                message: format!("Open Library API error at {url}"),
            });
        }

        let raw: RawSearchResponse = response.json().await?;
        Ok(raw.docs.into_iter().map(convert_doc).collect())
    }

    pub async fn get_by_isbn(&self, raw_isbn: &str) -> Result<Option<PaperHit>> {
        let Some(isbn) = normalize_isbn(raw_isbn) else {
            return Ok(None);
        };

        let url = format!("{}/isbn/{isbn}.json", self.base_url);
        let response = send_with_retry("openlibrary", self.client.get(&url)).await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(ZoteroMcpError::Api {
                status: response.status().as_u16(),
                message: format!("Open Library ISBN API error at {url}"),
            });
        }

        let raw: RawBookDoc = response.json().await?;
        let mut hit = convert_isbn_book(raw, isbn);
        hit.work_type = Some(WorkType::Book);
        Ok(Some(hit))
    }

    pub(crate) async fn get_toc(&self, raw_isbn: &str) -> Result<Option<crate::book::BookToc>> {
        let Some(isbn) = normalize_isbn(raw_isbn) else {
            return Ok(None);
        };

        let url = format!("{}/isbn/{isbn}.json", self.base_url);
        let response = send_with_retry("openlibrary", self.client.get(&url)).await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(ZoteroMcpError::Api {
                status: response.status().as_u16(),
                message: format!("Open Library TOC API error at {url}"),
            });
        }

        let raw: RawBookDoc = response.json().await?;
        let title = raw.title.as_deref().unwrap_or("Untitled Book");
        if let Some(ref toc_val) = raw.table_of_contents {
            return Ok(parse_openlibrary_toc(toc_val, title, Some(&isbn)));
        }
        Ok(None)
    }
}

impl std::fmt::Debug for OpenLibraryClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenLibraryClient")
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
struct RawSearchResponse {
    #[serde(default)]
    docs: Vec<RawSearchDoc>,
}

#[derive(Debug, Deserialize)]
struct RawSearchDoc {
    key: String,
    title: Option<String>,
    #[serde(default)]
    author_name: Vec<String>,
    first_publish_year: Option<i32>,
    #[serde(default)]
    isbn: Vec<String>,
    #[serde(default)]
    publisher: Vec<String>,
    #[serde(default)]
    ia: Vec<String>,
    public_scan_b: Option<bool>,
    first_sentence: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct RawBookDoc {
    title: Option<String>,
    publish_date: Option<String>,
    #[serde(default)]
    publishers: Vec<String>,
    #[serde(default)]
    table_of_contents: Option<serde_json::Value>,
    #[serde(default)]
    ia: Vec<String>,
    #[serde(default)]
    ocaid: Option<String>,
}

fn convert_doc(doc: RawSearchDoc) -> PaperHit {
    let title = doc.title.unwrap_or_else(|| "Untitled Book".to_string());
    let year = doc.first_publish_year.map(|y| y.to_string());
    let venue = doc.publisher.into_iter().next();
    let abstract_note = doc.first_sentence.and_then(|s| s.into_iter().next());

    // Only store validated canonical ISBNs
    let isbn = doc.isbn.iter().find_map(|raw| normalize_isbn(raw));

    let work_url = if doc.key.starts_with('/') {
        format!("https://openlibrary.org{}", doc.key)
    } else {
        format!("https://openlibrary.org/{}", doc.key)
    };

    let ia_id = doc.ia.into_iter().find(|s| !s.trim().is_empty());
    // Only public scans are freely downloadable without lending restrictions
    let is_open_access = doc.public_scan_b == Some(true) && ia_id.is_some();
    let (oa_pdf_url, landing_url) = if let Some(ref ia) = ia_id {
        let landing = format!("https://archive.org/details/{ia}");
        let pdf = if is_open_access {
            Some(format!("https://archive.org/download/{ia}/{ia}.pdf"))
        } else {
            None
        };
        (pdf, Some(landing))
    } else {
        (None, None)
    };

    let mut hit = PaperHit::new(
        PaperSource::OpenLibrary,
        title,
        doc.author_name,
        year,
        None,
        None,
        None,
        abstract_note,
        landing_url.or(Some(work_url)),
        None,
        oa_pdf_url,
        venue,
        None,
    );
    hit.work_type = Some(WorkType::Book);
    hit.isbn = isbn;
    hit
}

fn convert_isbn_book(doc: RawBookDoc, canonical_isbn: String) -> PaperHit {
    let title = doc.title.unwrap_or_else(|| "Untitled Book".to_string());
    let venue = doc.publishers.into_iter().next();
    let year = doc.publish_date.as_deref().and_then(extract_year_from_date);
    let ia_id = doc.ocaid.or_else(|| doc.ia.into_iter().next());
    let landing_url = ia_id
        .as_ref()
        .map(|ia| format!("https://archive.org/details/{ia}"))
        .or_else(|| Some(format!("https://openlibrary.org/isbn/{canonical_isbn}")));

    let mut hit = PaperHit::new(
        PaperSource::OpenLibrary,
        title,
        Vec::new(),
        year,
        None,
        None,
        None,
        None,
        landing_url,
        None,
        None, // Free OA download requires verified public_scan_b from search endpoint
        venue,
        None,
    );
    hit.work_type = Some(WorkType::Book);
    hit.isbn = Some(canonical_isbn);
    hit
}

fn extract_chapter_number_from_title(title: &str) -> Option<u32> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Check if the label or title starts with a bare number (e.g. "1", "1.", "1: Intro", "1 - Intro")
    // Reject section numbers like "1.1", "1.2.3"
    let digit_len = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_len > 0 {
        let after_digits = &trimmed[digit_len..];
        let is_section_dotted = after_digits.starts_with('.')
            && after_digits[1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit());
        if !is_section_dotted {
            let after_trimmed = after_digits.trim_start();
            if (after_trimmed.is_empty()
                || after_trimmed.starts_with(['.', ':', '-', '—', ' ', '\t']))
                && let Ok(n) = trimmed[..digit_len].parse::<u32>()
                && (1..1000).contains(&n)
            {
                return Some(n);
            }
        }
    }

    let lower = trimmed.to_ascii_lowercase();

    // Check all occurrences of "chapter"
    let mut search_idx = 0;
    while let Some(rel_idx) = lower[search_idx..].find("chapter") {
        let idx = search_idx + rel_idx;
        search_idx = idx + "chapter".len();
        let before_ok = idx == 0 || !lower.as_bytes()[idx - 1].is_ascii_alphanumeric();
        if before_ok {
            let after = &lower[idx + "chapter".len()..];
            let num_str: String = after
                .chars()
                .skip_while(|c| *c == ' ' || *c == '.' || *c == '-' || *c == ':' || *c == '\t')
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(n) = num_str.parse::<u32>() {
                return Some(n);
            }
        }
    }

    // Check all occurrences of "ch." or "ch "
    for prefix in &["ch.", "ch "] {
        let mut search_idx = 0;
        while let Some(rel_idx) = lower[search_idx..].find(prefix) {
            let idx = search_idx + rel_idx;
            search_idx = idx + prefix.len();
            let before_ok = idx == 0 || !lower.as_bytes()[idx - 1].is_ascii_alphanumeric();
            if before_ok {
                let after = &lower[idx + prefix.len()..];
                let num_str: String = after
                    .chars()
                    .skip_while(|c| *c == ' ' || *c == ':' || *c == '-' || *c == '.')
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(n) = num_str.parse::<u32>() {
                    return Some(n);
                }
            }
        }
    }

    None
}

pub(crate) fn parse_openlibrary_toc(
    val: &serde_json::Value,
    title: &str,
    isbn: Option<&str>,
) -> Option<crate::book::BookToc> {
    let arr = val.as_array()?;
    let min_level = arr
        .iter()
        .filter_map(|item| {
            item.as_object()
                .and_then(|obj| obj.get("level").and_then(|l| l.as_u64()))
        })
        .min();

    // Check if min_level consists exclusively of Parts (e.g. "Part I", "Part II") or front matter.
    // If so, the actual chapters live at min_level + 1 (e.g. Designing Data-Intensive Applications).
    let target_level = if let Some(min_l) = min_level {
        let min_items: Vec<_> = arr
            .iter()
            .filter_map(|item| {
                item.as_object().and_then(|obj| {
                    let lvl = obj.get("level").and_then(|l| l.as_u64())?;
                    (lvl == min_l).then_some(obj)
                })
            })
            .collect();
        let all_parts_or_front = !min_items.is_empty()
            && min_items.iter().all(|obj| {
                let t = obj
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                t.starts_with("part ") || t.starts_with("part\t") || is_non_chapter_heading(&t)
            });
        if all_parts_or_front { min_l + 1 } else { min_l }
    } else {
        0
    };

    let mut filtered_items = Vec::new();
    for item in arr {
        if let Some(s) = item.as_str() {
            filtered_items.push((s.trim().to_string(), None, None));
        } else if let Some(obj) = item.as_object() {
            if let Some(level) = obj.get("level").and_then(|l| l.as_u64())
                && level != target_level
            {
                continue;
            }
            let t = obj
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("Untitled Chapter")
                .trim()
                .to_string();
            let p = obj
                .get("pagenum")
                .and_then(|p| p.as_str())
                .and_then(|p| p.parse::<u32>().ok());
            let label_num = obj
                .get("label")
                .and_then(|l| l.as_str())
                .and_then(extract_chapter_number_from_title);
            filtered_items.push((t, p, label_num));
        }
    }

    let mut chapters: Vec<crate::book::BookChapter> = Vec::new();
    for (i, (ch_title, page, explicit_num)) in filtered_items.into_iter().enumerate() {
        if ch_title.is_empty() {
            continue;
        }

        if is_dotted_section_title(&ch_title) {
            if let Some(parent) = chapters.last_mut() {
                let sub_idx = parent.subsections.len() + 1;
                let sub_id = format!("{}.{}", parent.id, sub_idx);
                parent.subsections.push(crate::book::BookChapter {
                    id: sub_id,
                    number: None,
                    title: ch_title,
                    start_page: page,
                    end_page: None,
                    subsections: Vec::new(),
                });
            }
            continue;
        }

        let num = explicit_num.or_else(|| extract_chapter_number_from_title(&ch_title));
        let id = if let Some(n) = num {
            format!("ch-{n}")
        } else {
            format!("entry-{}", i + 1)
        };

        chapters.push(crate::book::BookChapter {
            id,
            number: num,
            title: ch_title,
            start_page: page,
            end_page: None,
            subsections: Vec::new(),
        });
    }

    if chapters.is_empty() {
        return None;
    }

    // If none of the chapters had explicit "Chapter N" in their titles or labels,
    // assign sequential numbers to non-front/back matter chapters.
    let any_numbered = chapters.iter().any(|c| c.number.is_some());
    if !any_numbered {
        let mut next_num = 1;
        for (i, ch) in chapters.iter_mut().enumerate() {
            if is_non_chapter_heading(&ch.title) {
                ch.number = None;
                ch.id = format!("entry-{}", i + 1);
            } else {
                let n = next_num;
                next_num += 1;
                ch.number = Some(n);
                ch.id = format!("ch-{n}");
            }
        }
    }

    let numbered_count = chapters.iter().filter(|c| c.number.is_some()).count();
    let total_chapters = if numbered_count > 0 {
        Some(numbered_count)
    } else {
        Some(chapters.len())
    };

    Some(crate::book::BookToc {
        isbn: isbn.map(ToString::to_string),
        title: title.to_string(),
        authors: Vec::new(),
        total_chapters,
        chapters,
    })
}

fn is_non_chapter_heading(title: &str) -> bool {
    let lower = title.trim().to_ascii_lowercase();
    let stripped = lower.trim_start_matches(|c: char| !c.is_alphabetic());
    matches!(
        stripped,
        "preface"
            | "foreword"
            | "introduction"
            | "prologue"
            | "epilogue"
            | "conclusion"
            | "afterword"
            | "notes"
            | "dedication"
            | "acknowledgments"
            | "acknowledgements"
            | "about the author"
            | "about the authors"
            | "table of contents"
            | "contents"
            | "index"
            | "bibliography"
            | "references"
            | "appendix"
            | "glossary"
            | "colophon"
            | "copyright"
            | "list of figures"
            | "list of tables"
    ) || stripped.starts_with("part ")
        || stripped.starts_with("part\t")
        || stripped.starts_with("appendix ")
        || stripped.starts_with("conclusion")
        || stripped.starts_with("afterword")
        || stripped.starts_with("epilogue")
        || stripped.starts_with("about the author")
}

fn is_dotted_section_title(title: &str) -> bool {
    let trimmed = title.trim();
    let digit_len = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_len > 0 {
        let after = &trimmed[digit_len..];
        if after.starts_with('.')
            && after[1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        {
            return true;
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    for prefix in &["section ", "sec. ", "sec "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest_trimmed = rest.trim_start();
            let d_len = rest_trimmed
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .count();
            if d_len > 0 {
                let after = &rest_trimmed[d_len..];
                if after.starts_with('.')
                    && after[1..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit())
                {
                    return true;
                }
            }
        }
    }
    false
}

fn extract_year_from_date(s: &str) -> Option<String> {
    for word in s.split(|c: char| !c.is_ascii_digit()) {
        if word.len() == 4
            && (word.starts_with("18") || word.starts_with("19") || word.starts_with("20"))
        {
            return Some(word.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_doc_extracts_isbn_and_public_scan() {
        let doc = RawSearchDoc {
            key: "/works/OL12345W".to_string(),
            title: Some("Designing Data-Intensive Applications".to_string()),
            author_name: vec!["Martin Kleppmann".to_string()],
            first_publish_year: Some(2017),
            isbn: vec!["978-1-4919-0307-0".to_string()],
            publisher: vec!["O'Reilly Media".to_string()],
            ia: vec!["designingdataint0000klep".to_string()],
            public_scan_b: Some(true),
            first_sentence: Some(vec![
                "Data is at the center of system challenges.".to_string(),
            ]),
        };

        let hit = convert_doc(doc);
        assert_eq!(hit.source, PaperSource::OpenLibrary);
        assert_eq!(hit.work_type, Some(WorkType::Book));
        assert_eq!(hit.isbn, Some("9781491903070".to_string()));
        assert_eq!(hit.year, Some("2017".to_string()));
        assert_eq!(hit.venue, Some("O'Reilly Media".to_string()));
        assert!(hit.oa_pdf_url.is_some());
        assert!(hit.oa_pdf_url.unwrap().contains("archive.org/download"));
    }

    #[test]
    fn test_lending_only_scan_not_marked_as_oa_pdf() {
        let doc = RawSearchDoc {
            key: "/works/OL99999W".to_string(),
            title: Some("Restricted Book".to_string()),
            author_name: vec!["Author".to_string()],
            first_publish_year: Some(2020),
            isbn: vec!["9781491903070".to_string()],
            publisher: vec![],
            ia: vec!["restrictedbook0000".to_string()],
            public_scan_b: Some(false), // NOT a public scan
            first_sentence: None,
        };

        let hit = convert_doc(doc);
        assert!(hit.oa_pdf_url.is_none());
        assert!(hit.url.is_some()); // Archive.org landing page exists
    }

    #[test]
    fn test_extract_year_from_date() {
        assert_eq!(extract_year_from_date("March 2017"), Some("2017".into()));
        assert_eq!(extract_year_from_date("1999-10-15"), Some("1999".into()));
        assert_eq!(extract_year_from_date("circa 1888"), Some("1888".into()));
        assert_eq!(extract_year_from_date("unknown"), None);
    }

    #[test]
    fn test_parse_openlibrary_toc() {
        let val = serde_json::json!([
            "Preface",
            "Chapter 1: Reliable Systems",
            {"title": "Chapter 2: Data Models", "pagenum": "27"}
        ]);
        let toc = parse_openlibrary_toc(&val, "My Book", Some("9781491903070")).unwrap();
        assert_eq!(toc.total_chapters, Some(2));
        assert_eq!(toc.chapters[0].title, "Preface");
        assert_eq!(toc.chapters[0].number, None);
        assert_eq!(toc.chapters[0].id, "entry-1");
        assert_eq!(toc.chapters[1].title, "Chapter 1: Reliable Systems");
        assert_eq!(toc.chapters[1].number, Some(1));
        assert_eq!(toc.chapters[1].id, "ch-1");
        assert_eq!(toc.chapters[2].title, "Chapter 2: Data Models");
        assert_eq!(toc.chapters[2].number, Some(2));
        assert_eq!(toc.chapters[2].id, "ch-2");
        assert_eq!(toc.chapters[2].start_page, Some(27));
    }

    #[test]
    fn test_extract_chapter_number_word_boundaries() {
        assert_eq!(extract_chapter_number_from_title("A New Approach. 1"), None);
        assert_eq!(
            extract_chapter_number_from_title("Approach and Ch 4"),
            Some(4)
        );
        assert_eq!(extract_chapter_number_from_title("1"), Some(1));
        assert_eq!(extract_chapter_number_from_title("2. Foundations"), Some(2));
        // Section numbers like 1.1 or 1.2.3 rejected
        assert_eq!(extract_chapter_number_from_title("1.1 Reliability"), None);
        assert_eq!(extract_chapter_number_from_title("1.2.3 Data Models"), None);
        assert_eq!(
            extract_chapter_number_from_title("Chapter 1: Foundations"),
            Some(1)
        );
        assert_eq!(
            extract_chapter_number_from_title("Ch. 3 - Distributed State"),
            Some(3)
        );
        assert_eq!(
            extract_chapter_number_from_title("Part 1: Ch 4 Transactions"),
            Some(4)
        );
    }

    #[test]
    fn test_parse_openlibrary_toc_with_labels_subsections_and_fallback() {
        let val = serde_json::json!([
            {"title": "Preface"},
            {"label": "1", "title": "Foundations of Data Systems"},
            {"level": 2, "title": "1.1 Reliability"},
            {"level": 1, "title": "Data Storage and Retrieval"},
            {"title": "Index"},
            {"title": "Appendix A"}
        ]);
        let toc = parse_openlibrary_toc(&val, "Book", None).unwrap();
        // 1.1 Reliability (level: 2) is filtered out relative to min_level 1
        assert_eq!(toc.chapters.len(), 5);
        assert_eq!(toc.chapters[0].title, "Preface");
        assert_eq!(toc.chapters[0].number, None);
        assert_eq!(toc.chapters[0].id, "entry-1");

        // Bare number "1" in label
        assert_eq!(toc.chapters[1].title, "Foundations of Data Systems");
        assert_eq!(toc.chapters[1].number, Some(1));
        assert_eq!(toc.chapters[1].id, "ch-1");

        assert_eq!(toc.chapters[2].title, "Data Storage and Retrieval");

        // Back matter not numbered as chapters
        assert_eq!(toc.chapters[3].title, "Index");
        assert_eq!(toc.chapters[3].number, None);
        assert_eq!(toc.chapters[4].title, "Appendix A");
        assert_eq!(toc.chapters[4].number, None);
    }

    #[test]
    fn test_parse_openlibrary_toc_parts_organization() {
        let val = serde_json::json!([
            {"level": 0, "title": "Part I: Storage and Retrieval"},
            {"level": 1, "label": "1", "title": "Reliable Systems"},
            {"level": 1, "label": "2", "title": "Data Models"},
            {"level": 0, "title": "Part II: Distributed Data"},
            {"level": 1, "label": "3", "title": "Replication"},
            {"level": 1, "label": "4", "title": "Partitioning"}
        ]);
        let toc = parse_openlibrary_toc(&val, "Designing Data", None).unwrap();
        // Parts at level 0 skipped; chapters at level 1 extracted!
        assert_eq!(toc.chapters.len(), 4);
        assert_eq!(toc.chapters[0].title, "Reliable Systems");
        assert_eq!(toc.chapters[0].number, Some(1));
        assert_eq!(toc.chapters[3].title, "Partitioning");
        assert_eq!(toc.chapters[3].number, Some(4));
    }

    #[test]
    fn test_parse_openlibrary_toc_flat_with_dotted_sections() {
        let val = serde_json::json!([
            {"title": "Foundations of Data Systems"},
            {"title": "1.1 Reliability and Fault Tolerance"},
            {"title": "1.2 Scalability and Performance"},
            {"title": "Data Storage and Retrieval"},
            {"title": "2.1 Storage Engines"}
        ]);
        let toc = parse_openlibrary_toc(&val, "Book", None).unwrap();
        // The dotted sections are nested under their respective chapters rather than becoming top-level chapters
        assert_eq!(toc.chapters.len(), 2);
        assert_eq!(toc.chapters[0].title, "Foundations of Data Systems");
        assert_eq!(toc.chapters[0].number, Some(1));
        assert_eq!(toc.chapters[0].subsections.len(), 2);
        assert_eq!(
            toc.chapters[0].subsections[0].title,
            "1.1 Reliability and Fault Tolerance"
        );
        assert_eq!(
            toc.chapters[0].subsections[1].title,
            "1.2 Scalability and Performance"
        );

        assert_eq!(toc.chapters[1].title, "Data Storage and Retrieval");
        assert_eq!(toc.chapters[1].number, Some(2));
        assert_eq!(toc.chapters[1].subsections.len(), 1);
        assert_eq!(toc.chapters[1].subsections[0].title, "2.1 Storage Engines");
    }
}
