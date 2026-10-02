//! Domain models, identifiers, and navigation structures for technical books.

pub(crate) mod isbn;

pub(crate) use isbn::normalize_isbn;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A chapter or major structural unit of a technical book.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub(crate) struct BookChapter {
    /// Unique chapter identifier (e.g. "ch-1", "entry-1").
    pub(crate) id: String,
    /// 1-based chapter number when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) number: Option<u32>,
    /// Chapter title / heading.
    pub(crate) title: String,
    /// Start page number if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) start_page: Option<u32>,
    /// End page number if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) end_page: Option<u32>,
    /// Nested subheadings or sections.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) subsections: Vec<BookChapter>,
}

/// Table of Contents (TOC) structure for navigating a book without loading its full text.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub(crate) struct BookToc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) isbn: Option<String>,
    pub(crate) title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) authors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) total_chapters: Option<usize>,
    pub(crate) chapters: Vec<BookChapter>,
}

#[derive(Debug, Clone)]
pub(crate) struct CandidateHeading {
    pub(crate) number: u32,
    pub(crate) byte_offset: usize,
    pub(crate) heading_line_len: usize,
    pub(crate) title: String,
}

/// Collects raw candidate headings from text lines, including multiline heading titles.
pub(crate) fn collect_raw_candidate_headings(content: &str) -> Vec<CandidateHeading> {
    let mut raw_candidates = Vec::new();
    let mut byte_offset = 0;

    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    for (i, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim();
        let preceded_cleanly = i == 0
            || lines[i - 1].trim().is_empty()
            || lines[i - 1]
                .trim()
                .to_ascii_lowercase()
                .contains("contents")
            || detect_chapter_heading(lines[i - 1].trim()).is_some();
        if preceded_cleanly && let Some((num, mut ch_title)) = detect_chapter_heading(trimmed) {
            // If the heading line was only "Chapter N", check if the next line has the title
            if (ch_title == trimmed
                || ch_title.is_empty()
                || ch_title.eq_ignore_ascii_case("chapter"))
                && let Some(next_line) = lines
                    .get(i + 1..)
                    .and_then(|sub| sub.iter().find(|l| !l.trim().is_empty()))
            {
                let next_trimmed = next_line.trim();
                if next_trimmed.chars().count() <= 120
                    && detect_chapter_heading(next_trimmed).is_none()
                    && !next_trimmed.contains("....")
                    && !next_trimmed.starts_with(['[', '(', '{'])
                {
                    ch_title = next_trimmed.to_string();
                }
            }
            raw_candidates.push(CandidateHeading {
                number: num,
                byte_offset,
                heading_line_len: raw_line.len(),
                title: ch_title,
            });
        }
        byte_offset += raw_line.len();
    }

    raw_candidates
}

/// Separates raw candidate headings into (optional front-matter TOC block, body candidates).
/// A front-matter TOC block is an initial sequence of at least 2 strictly increasing candidates
/// closely spaced (e.g. <= 600 bytes apart) before a restart point where chapter numbering drops
/// below the highest chapter number seen so far (e.g. dropping from 5 back to 1, or 3 back to 2).
fn partition_toc_and_body_candidates(
    raw_candidates: &[CandidateHeading],
) -> (Vec<CandidateHeading>, Vec<CandidateHeading>) {
    if raw_candidates.len() < 3 {
        return (Vec::new(), raw_candidates.to_vec());
    }

    let mut max_seen = 0u32;
    let mut restart_idx = None;

    for (idx, c) in raw_candidates.iter().enumerate() {
        if max_seen > 0 && c.number < max_seen && idx >= 2 {
            let prior = &raw_candidates[..idx];
            // TOC entries must be strictly increasing with at least 2 distinct numbers
            let strictly_increasing = prior.windows(2).all(|w| w[0].number < w[1].number);
            // And closely spaced like a table of contents list (<= 600 bytes apart)
            let closely_spaced = prior
                .windows(2)
                .all(|w| w[1].byte_offset.saturating_sub(w[0].byte_offset) <= 600);

            if strictly_increasing && closely_spaced {
                restart_idx = Some(idx);
                break;
            }
        }
        if c.number > max_seen {
            max_seen = c.number;
        }
    }

    if let Some(idx) = restart_idx {
        (
            raw_candidates[..idx].to_vec(),
            raw_candidates[idx..].to_vec(),
        )
    } else {
        (Vec::new(), raw_candidates.to_vec())
    }
}

/// Dynamic programming to find the longest non-decreasing subsequence (repeats allowed for running headers).
/// This cleanly discards out-of-order forward references and backward cross-references.
/// Note: O(N^2) where N is typically < 100 candidate headings per document.
fn filter_monotonic_candidates(candidates: &[CandidateHeading]) -> Vec<CandidateHeading> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let n = candidates.len();
    let mut dp = vec![1usize; n];
    let mut parent: Vec<Option<usize>> = vec![None; n];

    for i in 0..n {
        for j in 0..i {
            if candidates[j].number <= candidates[i].number && dp[j] + 1 >= dp[i] {
                dp[i] = dp[j] + 1;
                parent[i] = Some(j);
            }
        }
    }

    let mut best_idx = 0;
    let mut max_len = 0;
    for (i, &len) in dp.iter().enumerate() {
        if len >= max_len {
            max_len = len;
            best_idx = i;
        }
    }

    let mut filtered_indices = Vec::with_capacity(max_len);
    let mut curr = Some(best_idx);
    while let Some(idx) = curr {
        filtered_indices.push(idx);
        curr = parent[idx];
    }
    filtered_indices.reverse();

    filtered_indices
        .into_iter()
        .map(|idx| candidates[idx].clone())
        .collect()
}

/// Heuristically extracts a Table of Contents from text (e.g. extracted PDF content or OCR).
pub(crate) fn extract_toc_from_text(title: &str, isbn: Option<&str>, content: &str) -> BookToc {
    let raw = collect_raw_candidate_headings(content);
    let (toc_candidates, body_candidates) = partition_toc_and_body_candidates(&raw);

    // If a front-matter TOC exists, use it as the authoritative chapter list.
    // Otherwise, filter the body candidates to the longest non-decreasing sequence.
    let candidates = if !toc_candidates.is_empty() {
        let mut list = toc_candidates;
        let max_toc_num = list.iter().map(|c| c.number).max().unwrap_or(0);
        let filtered_body = filter_monotonic_candidates(&body_candidates);
        for bc in filtered_body {
            if bc.number > max_toc_num {
                list.push(bc);
            }
        }
        list
    } else {
        filter_monotonic_candidates(&body_candidates)
    };

    let mut chapters = Vec::new();
    let mut seen_numbers = std::collections::HashSet::new();

    for c in candidates {
        if seen_numbers.insert(c.number) {
            chapters.push(BookChapter {
                id: format!("ch-{}", c.number),
                number: Some(c.number),
                title: c.title,
                start_page: None,
                end_page: None,
                subsections: Vec::new(),
            });
        }
    }

    let total = chapters.len();
    BookToc {
        isbn: isbn.map(ToString::to_string),
        title: title.to_string(),
        authors: Vec::new(),
        total_chapters: Some(total),
        chapters,
    }
}

/// Extracts the slice of text corresponding to a specific 1-based chapter number.
/// Returns the text slice along with the chapter title.
pub(crate) fn extract_chapter_slice(content: &str, chapter_num: u32) -> Option<(&str, String)> {
    let raw = collect_raw_candidate_headings(content);
    let (_, body_candidates) = partition_toc_and_body_candidates(&raw);
    let candidates = filter_monotonic_candidates(&body_candidates);
    if candidates.is_empty() {
        return None;
    }

    let mut best_slice: Option<(&str, String, usize)> = None;

    for (idx, c) in candidates.iter().enumerate() {
        if c.number != chapter_num {
            continue;
        }

        // The candidate chapter ends at the next candidate with a different chapter number, or EOF.
        let end_pos = candidates[idx + 1..]
            .iter()
            .find(|next| next.number != chapter_num)
            .map(|next| next.byte_offset)
            .unwrap_or(content.len());

        let start = c.byte_offset.min(content.len());
        let end = end_pos.min(content.len());
        if start < end {
            let slice = &content[start..end];
            let remaining_body = slice[c.heading_line_len.min(slice.len())..].trim();
            let body_len = remaining_body.chars().count();
            // Candidate must have real substantive body text beyond the heading line
            if body_len > 0 {
                match &best_slice {
                    Some((_, _, best_len)) if *best_len >= body_len => {}
                    _ => {
                        best_slice = Some((slice, c.title.clone(), body_len));
                    }
                }
            }
        }
    }

    best_slice.map(|(slice, title, _)| (slice, title))
}

fn parse_numeral_or_word(s: &str) -> Option<(u32, usize)> {
    let digit_len = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_len > 0 {
        let n = s[..digit_len].parse::<u32>().ok()?;
        return Some((n, digit_len));
    }
    let raw_token = s.split_whitespace().next()?;
    let leading_punct_len = raw_token.len()
        - raw_token
            .trim_start_matches([':', '.', '-', '—', ','])
            .len();
    let trimmed_token = raw_token.trim_matches([':', '.', '-', '—', ',']);
    let upper = trimmed_token.to_ascii_uppercase();
    const ROMAN: &[(&str, u32)] = &[
        ("I", 1),
        ("II", 2),
        ("III", 3),
        ("IV", 4),
        ("V", 5),
        ("VI", 6),
        ("VII", 7),
        ("VIII", 8),
        ("IX", 9),
        ("X", 10),
        ("XI", 11),
        ("XII", 12),
        ("XIII", 13),
        ("XIV", 14),
        ("XV", 15),
        ("XVI", 16),
        ("XVII", 17),
        ("XVIII", 18),
        ("XIX", 19),
        ("XX", 20),
    ];
    for &(r, val) in ROMAN {
        if upper == r {
            return Some((val, leading_punct_len + trimmed_token.len()));
        }
    }
    const WORDS: &[(&str, u32)] = &[
        ("ONE", 1),
        ("TWO", 2),
        ("THREE", 3),
        ("FOUR", 4),
        ("FIVE", 5),
        ("SIX", 6),
        ("SEVEN", 7),
        ("EIGHT", 8),
        ("NINE", 9),
        ("TEN", 10),
        ("ELEVEN", 11),
        ("TWELVE", 12),
        ("THIRTEEN", 13),
        ("FOURTEEN", 14),
        ("FIFTEEN", 15),
        ("SIXTEEN", 16),
        ("SEVENTEEN", 17),
        ("EIGHTEEN", 18),
        ("NINETEEN", 19),
        ("TWENTY", 20),
    ];
    for &(w, val) in WORDS {
        if upper == w {
            return Some((val, leading_punct_len + trimmed_token.len()));
        }
    }
    None
}

fn detect_chapter_heading(line: &str) -> Option<(u32, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 120 {
        return None;
    }
    // Reject TOC leader lines with dots (e.g. "Chapter 1 ....... 23")
    if trimmed.contains("....") || trimmed.contains(". . .") || trimmed.contains("……") {
        return None;
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("chapter ") || lower.starts_with("chapter\t") {
        let rest_orig = trimmed["chapter".len()..].trim_start();
        // Require an actual chapter number (digit, Roman numeral, or word like 'One', 'Two')
        let (num, match_len) = parse_numeral_or_word(rest_orig)?;

        let after_num = rest_orig[match_len..].trim_start();
        // Reject trailing commas or connectors like "Chapter 5, which discusses..."
        if after_num.starts_with(',') {
            return None;
        }

        let first_char = after_num.chars().next();
        let (has_separator, title_part) = if let Some(fc) = first_char
            && (fc == ':' || fc == '-' || fc == '—' || (fc == '.' && !after_num.starts_with("..")))
        {
            (true, after_num[fc.len_utf8()..].trim())
        } else {
            (false, after_num)
        };

        // Reject wrapped body prose (e.g. "Chapter 5 covers the details in Section 2.1"):
        // Real heading titles are either empty ("Chapter 5"), have a punctuation separator, or start with capital letter.
        if !has_separator
            && title_part
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase())
        {
            return None;
        }

        // Strip trailing dots/page numbers if any (e.g. "Reliability ... 15")
        let clean_title = if let Some(dot_idx) = title_part.rfind("..") {
            title_part[..dot_idx].trim()
        } else {
            title_part
        };

        // Reject sentences ending in punctuation (e.g. "Chapter 3. The next chapter builds on this."):
        // A true chapter title is a heading, never an English sentence ending with punctuation.
        if clean_title.ends_with(['.', '!', '?']) {
            return None;
        }

        let final_title = if clean_title.is_empty() {
            trimmed.to_string()
        } else {
            clean_title.to_string()
        };
        return Some((num, final_title));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_toc_and_chapter_slice() {
        let sample = "\
Front matter and preface.

Chapter 1: Foundations of Reliability
In this chapter we explore fault tolerance and high availability.
Systems must be designed for partial failure.

Chapter 2 - Data Models and Query Languages
Relational versus document models. Graph-based data models.

Chapter 3: Storage and Retrieval
Log-structured storage engines and B-Trees.
";

        let toc = extract_toc_from_text("Designing Data Systems", Some("9781491903070"), sample);
        assert_eq!(toc.total_chapters, Some(3));
        assert_eq!(toc.chapters.len(), 3);
        assert_eq!(toc.chapters[0].title, "Foundations of Reliability");
        assert_eq!(toc.chapters[0].number, Some(1));
        assert_eq!(toc.chapters[1].title, "Data Models and Query Languages");
        assert_eq!(toc.chapters[2].title, "Storage and Retrieval");

        let (ch1, title1) = extract_chapter_slice(sample, 1).unwrap();
        assert_eq!(title1, "Foundations of Reliability");
        assert!(ch1.contains("In this chapter we explore fault tolerance"));
        assert!(!ch1.contains("Relational versus document models"));

        let (ch2, title2) = extract_chapter_slice(sample, 2).unwrap();
        assert_eq!(title2, "Data Models and Query Languages");
        assert!(ch2.contains("Relational versus document models"));

        assert!(extract_chapter_slice(sample, 99).is_none());
    }

    #[test]
    fn test_extract_chapter_slice_crlf_and_multibyte_utf8() {
        let sample = "序章 前書き\r\n\r\n\
Chapter 1: 計算機科学\r\n\
これは日本語のテストです。Unicode — em dash and accents (café).\r\n\r\n\
Chapter 2: 分散システム\r\n\
Second chapter content.\r\n";

        let (ch1, title1) = extract_chapter_slice(sample, 1).expect("chapter 1 slice");
        assert_eq!(title1, "計算機科学");
        assert!(ch1.contains("これは日本語のテストです"));
        assert!(!ch1.contains("分散システム"));

        let (ch2, title2) = extract_chapter_slice(sample, 2).expect("chapter 2 slice");
        assert_eq!(title2, "分散システム");
        assert!(ch2.contains("Second chapter content."));
    }

    #[test]
    fn test_running_headers_and_toc_dot_leaders_ignored() {
        let sample = "\
Table of Contents
Chapter 1: Reliability ........... 1
Chapter 2: Scalability ........... 25

Chapter 1: Reliability
This is the real body of chapter 1. It contains extensive text explaining reliability principles.

Chapter 1: Reliability
Page 2 running header should not split chapter 1.
More chapter 1 body text spanning multiple paragraphs.

Chapter 2: Scalability
This is chapter 2.
";

        let toc = extract_toc_from_text("Book", None, sample);
        assert_eq!(toc.chapters.len(), 2);
        assert_eq!(toc.chapters[0].title, "Reliability");
        assert_eq!(toc.chapters[1].title, "Scalability");

        let (ch1, _) = extract_chapter_slice(sample, 1).unwrap();
        assert!(ch1.contains("This is the real body of chapter 1"));
        assert!(ch1.contains("Page 2 running header should not split"));
        assert!(!ch1.contains("This is chapter 2"));
    }

    #[test]
    fn test_frontmatter_toc_without_dots_and_wrapped_prose_ignored() {
        let sample = "\
Table of Contents
Chapter 1 Foundations
Chapter 2 Scalability
Chapter 3 Storage

Preface
Here is the preface text. Chapter 5 covers advanced topics later in the book.

Chapter 1: Foundations
This is the start of the real chapter 1 body text. It covers all basic concepts thoroughly.
Lots of detailed content for the first chapter.

Chapter 2: Scalability
This is the real chapter 2 body text.
";

        let (ch1, title1) = extract_chapter_slice(sample, 1).unwrap();
        assert_eq!(title1, "Foundations");
        assert!(ch1.contains("This is the start of the real chapter 1 body text"));
        assert!(!ch1.contains("Preface"));

        let (ch2, title2) = extract_chapter_slice(sample, 2).unwrap();
        assert_eq!(title2, "Scalability");
        assert!(ch2.contains("This is the real chapter 2 body text"));

        // "Chapter 5 covers advanced topics..." in preface should not be detected as chapter 5!
        assert!(extract_chapter_slice(sample, 5).is_none());
    }

    #[test]
    fn test_detect_chapter_heading_advanced() {
        assert_eq!(
            detect_chapter_heading("Chapter 5 covers the details in Section 2.1"),
            None
        );
        assert_eq!(
            detect_chapter_heading("Chapter 5, which covers all the basics"),
            None
        );
        assert_eq!(
            detect_chapter_heading("Chapter IV: Distributed Systems"),
            Some((4, "Distributed Systems".to_string()))
        );
        assert_eq!(
            detect_chapter_heading("Chapter Four: Distributed Systems"),
            Some((4, "Distributed Systems".to_string()))
        );
        assert_eq!(
            detect_chapter_heading("Chapter 5 Foundations"),
            Some((5, "Foundations".to_string()))
        );
        // Em dash heading separator
        assert_eq!(
            detect_chapter_heading("Chapter 1 — Foundations of Reliability"),
            Some((1, "Foundations of Reliability".to_string()))
        );
        // Non-numbered textbook headings must be rejected
        assert_eq!(detect_chapter_heading("Chapter Summary"), None);
        assert_eq!(detect_chapter_heading("Chapter Review"), None);
        assert_eq!(detect_chapter_heading("Chapter Exercises"), None);
        assert_eq!(detect_chapter_heading("Chapter Objectives"), None);
    }

    #[test]
    fn test_chapter_summary_exercises_and_review_not_treated_as_chapters() {
        let sample = "\
Chapter 1: Foundations
This is chapter 1 body text discussing basics.

Chapter Summary
In this chapter, we reviewed the fundamental concepts of distributed systems.

Chapter Exercises
1. Explain the difference between linearizability and serializability.

Chapter 2: Replication
This is chapter 2 body text discussing state replication.

Chapter Review
Key takeaways from chapter 2 replication strategies.
";

        let toc = extract_toc_from_text("Test Book", None, sample);
        assert_eq!(toc.chapters.len(), 2);
        assert_eq!(toc.chapters[0].title, "Foundations");
        assert_eq!(toc.chapters[0].number, Some(1));
        assert_eq!(toc.chapters[1].title, "Replication");
        assert_eq!(toc.chapters[1].number, Some(2));

        let (ch1, title1) = extract_chapter_slice(sample, 1).expect("chapter 1 slice");
        assert_eq!(title1, "Foundations");
        assert!(ch1.contains("This is chapter 1 body text"));
        assert!(ch1.contains("Chapter Summary"));
        assert!(ch1.contains("Chapter Exercises"));
        assert!(!ch1.contains("Chapter 2: Replication"));

        let (ch2, title2) = extract_chapter_slice(sample, 2).expect("chapter 2 slice");
        assert_eq!(title2, "Replication");
        assert!(ch2.contains("This is chapter 2 body text"));
        assert!(ch2.contains("Chapter Review"));
        assert!(!ch2.contains("Chapter 1: Foundations"));
    }

    #[test]
    fn test_wrapped_cross_reference_does_not_truncate_chapter() {
        let sample = "\
Chapter 1: Data Models
This is the beginning of chapter 1. We explore relational and document models in depth.
As we will see in more detail in
Chapter 9. The consensus problem requires careful coordination.
Continuing chapter 1 discussion on query languages and schema flexibility.
More paragraphs in chapter 1 body text.

Chapter 2: Storage Engines
This is the beginning of chapter 2 discussing log-structured storage.
";

        let toc = extract_toc_from_text("Test Book", None, sample);
        assert_eq!(toc.chapters.len(), 2);
        assert_eq!(toc.chapters[0].title, "Data Models");
        assert_eq!(toc.chapters[0].number, Some(1));
        assert_eq!(toc.chapters[1].title, "Storage Engines");
        assert_eq!(toc.chapters[1].number, Some(2));

        let (ch1, title1) = extract_chapter_slice(sample, 1).expect("chapter 1 slice");
        assert_eq!(title1, "Data Models");
        assert!(ch1.contains("This is the beginning of chapter 1"));
        assert!(ch1.contains("Chapter 9. The consensus problem"));
        assert!(ch1.contains("Continuing chapter 1 discussion"));
        assert!(ch1.contains("More paragraphs in chapter 1 body text"));
        assert!(!ch1.contains("Chapter 2: Storage Engines"));

        let (ch2, title2) = extract_chapter_slice(sample, 2).expect("chapter 2 slice");
        assert_eq!(title2, "Storage Engines");
        assert!(ch2.contains("This is the beginning of chapter 2"));
    }

    #[test]
    fn test_blank_line_forward_cross_reference_does_not_pollute_toc_or_slice() {
        let sample = "\
Chapter 1: Data Models
This is the beginning of chapter 1. We explore relational and document models in depth.

Chapter 9. The key idea is that consensus requires careful coordination across nodes.

Continuing chapter 1 discussion on query languages and schema flexibility.
More paragraphs in chapter 1 body text.

Chapter 2: Storage Engines
This is the beginning of chapter 2 discussing log-structured storage.

Chapter 9: Consensus and Distributed Transactions
This is the real chapter 9 body text explaining Paxos and Raft.
Extensive discussion on consensus algorithms.

Chapter 10: Batch Processing
This is chapter 10 covering MapReduce and dataflow engines.
";

        let toc = extract_toc_from_text("Test Book", None, sample);
        assert_eq!(toc.chapters.len(), 4);
        assert_eq!(toc.chapters[0].title, "Data Models");
        assert_eq!(toc.chapters[0].number, Some(1));
        assert_eq!(toc.chapters[1].title, "Storage Engines");
        assert_eq!(toc.chapters[1].number, Some(2));
        assert_eq!(
            toc.chapters[2].title,
            "Consensus and Distributed Transactions"
        );
        assert_eq!(toc.chapters[2].number, Some(9));
        assert_eq!(toc.chapters[3].title, "Batch Processing");
        assert_eq!(toc.chapters[3].number, Some(10));

        // Chapter 1 slice should include the forward reference as body text and end at Chapter 2
        let (ch1, title1) = extract_chapter_slice(sample, 1).expect("chapter 1 slice");
        assert_eq!(title1, "Data Models");
        assert!(ch1.contains("This is the beginning of chapter 1"));
        assert!(ch1.contains("Chapter 9. The key idea is that consensus"));
        assert!(ch1.contains("Continuing chapter 1 discussion"));
        assert!(!ch1.contains("Chapter 2: Storage Engines"));

        // Chapter 9 slice must be the REAL chapter 9, not the stray cross reference from chapter 1
        let (ch9, title9) = extract_chapter_slice(sample, 9).expect("chapter 9 slice");
        assert_eq!(title9, "Consensus and Distributed Transactions");
        assert!(ch9.contains("This is the real chapter 9 body text"));
        assert!(ch9.contains("Extensive discussion on consensus algorithms"));
        assert!(!ch9.contains("Chapter 1: Data Models"));
        assert!(!ch9.contains("Chapter 10: Batch Processing"));
    }

    #[test]
    fn test_frontmatter_toc_with_missed_body_chapter_heading() {
        let sample = "\
Table of Contents
Chapter 1 Foundations
Chapter 2 Data Models
Chapter 3 Storage
Chapter 4 Encoding
Chapter 5 Replication

Preface
This book explores principles of distributed systems.

Chapter 1: Foundations
This is chapter 1 body text explaining basic principles.
More content for chapter 1.

Chapter 2: Data Models
This is chapter 2 body text explaining relational and document models.
More content for chapter 2.

12
Chapter 3: Storage
This is chapter 3 body text where page number directly preceded heading without blank line.
So chapter 3 heading was missed in the body.

Chapter 4: Encoding
This is chapter 4 body text explaining serialization formats like Avro and Protobuf.
More content for chapter 4.

Chapter 5: Replication
This is chapter 5 body text explaining single-leader and multi-leader replication.
";

        // TOC should contain all 5 chapters because TOC block has all 5!
        let toc = extract_toc_from_text("Designing Data Systems", None, sample);
        assert_eq!(toc.chapters.len(), 5);
        assert_eq!(toc.chapters[0].title, "Foundations");
        assert_eq!(toc.chapters[1].title, "Data Models");
        assert_eq!(toc.chapters[2].title, "Storage");
        assert_eq!(toc.chapters[3].title, "Encoding");
        assert_eq!(toc.chapters[4].title, "Replication");

        // Chapter 1 slice should be pure Chapter 1 body text, NOT containing TOC or Preface
        let (ch1, title1) = extract_chapter_slice(sample, 1).expect("chapter 1 slice");
        assert_eq!(title1, "Foundations");
        assert!(ch1.contains("This is chapter 1 body text explaining basic principles"));
        assert!(!ch1.contains("Table of Contents"));
        assert!(!ch1.contains("Preface"));
        assert!(!ch1.contains("Chapter 2: Data Models"));

        // Chapter 2 slice should be pure Chapter 2 body text
        let (ch2, title2) = extract_chapter_slice(sample, 2).expect("chapter 2 slice");
        assert_eq!(title2, "Data Models");
        assert!(ch2.contains("This is chapter 2 body text"));
        assert!(!ch2.contains("Chapter 4: Encoding"));

        // Chapter 3 was not detected in body, so slice must return None (not TOC line!)
        assert!(extract_chapter_slice(sample, 3).is_none());

        // Chapter 4 slice should be pure Chapter 4 body text, NOT the entire book
        let (ch4, title4) = extract_chapter_slice(sample, 4).expect("chapter 4 slice");
        assert_eq!(title4, "Encoding");
        assert!(ch4.contains("This is chapter 4 body text explaining serialization formats"));
        assert!(!ch4.contains("Chapter 1: Foundations"));
        assert!(!ch4.contains("Chapter 2: Data Models"));
        assert!(!ch4.contains("Table of Contents"));

        // Chapter 5 slice should be pure Chapter 5 body text
        let (ch5, title5) = extract_chapter_slice(sample, 5).expect("chapter 5 slice");
        assert_eq!(title5, "Replication");
        assert!(ch5.contains("This is chapter 5 body text explaining single-leader"));
    }

    #[test]
    fn test_frontmatter_toc_with_all_body_chapters_detected() {
        let sample = "\
Table of Contents
Chapter 1 Foundations
Chapter 2 Data Models
Chapter 3 Storage
Chapter 4 Encoding
Chapter 5 Replication

Preface
Introductory notes and book overview.

Chapter 1: Foundations
Chapter 1 body text thoroughly explaining reliable architectures.

Chapter 2: Data Models
Chapter 2 body text comparing document, graph, and relational data.

Chapter 3: Storage
Chapter 3 body text describing LSM-trees and B-trees.

Chapter 4: Encoding
Chapter 4 body text discussing thrift, protobuf, and avro schema evolution.

Chapter 5: Replication
Chapter 5 body text covering leader-follower replication and failover.
";

        let (ch1, title1) = extract_chapter_slice(sample, 1).expect("chapter 1 slice");
        assert_eq!(title1, "Foundations");
        assert!(ch1.contains("Chapter 1 body text thoroughly explaining reliable architectures"));
        // Must NOT contain the front-matter TOC or Preface!
        assert!(!ch1.contains("Table of Contents"));
        assert!(!ch1.contains("Introductory notes and book overview"));
        assert!(!ch1.contains("Chapter 2: Data Models"));

        let (ch5, title5) = extract_chapter_slice(sample, 5).expect("chapter 5 slice");
        assert_eq!(title5, "Replication");
        assert!(ch5.contains("Chapter 5 body text covering leader-follower replication"));
    }

    #[test]
    fn test_adjacent_chapter_reference_sentence_ignored() {
        let sample = "\
Chapter 2: Storage Engines
This is chapter 2 discussing B-trees and write-ahead logs.

Chapter 3. The next chapter builds on this foundation to explore transactions.

Continuing chapter 2 discussion of page cache and disk storage.

Chapter 3: Transactions
This is the real chapter 3 body text explaining ACID and serializability.
";

        let (ch2, title2) = extract_chapter_slice(sample, 2).expect("chapter 2 slice");
        assert_eq!(title2, "Storage Engines");
        assert!(ch2.contains("This is chapter 2 discussing B-trees"));
        assert!(ch2.contains("Chapter 3. The next chapter builds on this foundation"));
        assert!(ch2.contains("Continuing chapter 2 discussion"));
        assert!(!ch2.contains("Chapter 3: Transactions"));

        let (ch3, title3) = extract_chapter_slice(sample, 3).expect("chapter 3 slice");
        assert_eq!(title3, "Transactions");
        assert!(ch3.contains("This is the real chapter 3 body text explaining ACID"));
        assert!(!ch3.contains("This is chapter 2 discussing B-trees"));
    }

    #[test]
    fn test_frontmatter_toc_with_missed_body_chapter_one() {
        let sample = "\
Contents
Chapter 1 Foundations
Chapter 2 Data Models
Chapter 3 Storage

Preface
Preface text here.

12
Chapter 1: Foundations
Body of chapter one without blank line before it so heading is missed in body.

Chapter 2: Data Models
Body of chapter two explaining relational models.

Chapter 3: Storage
Body of chapter three explaining storage engines.
";

        let toc = extract_toc_from_text("Test Book", None, sample);
        assert_eq!(toc.chapters.len(), 3);
        assert_eq!(toc.chapters[0].title, "Foundations");
        assert_eq!(toc.chapters[1].title, "Data Models");
        assert_eq!(toc.chapters[2].title, "Storage");

        // Chapter 1 was missed in body, so slice must return None (not TOC line!)
        assert!(extract_chapter_slice(sample, 1).is_none());

        // Chapter 2 slice must be pure Chapter 2 body text, NOT containing Chapter 1 or TOC
        let (ch2, title2) = extract_chapter_slice(sample, 2).expect("chapter 2 slice");
        assert_eq!(title2, "Data Models");
        assert!(ch2.contains("Body of chapter two explaining relational models"));
        assert!(!ch2.contains("Contents"));
        assert!(!ch2.contains("Preface"));
        assert!(!ch2.contains("Body of chapter one"));
        assert!(!ch2.contains("Body of chapter three"));

        // Chapter 3 slice must be pure Chapter 3 body text
        let (ch3, title3) = extract_chapter_slice(sample, 3).expect("chapter 3 slice");
        assert_eq!(title3, "Storage");
        assert!(ch3.contains("Body of chapter three explaining storage engines"));
        assert!(!ch3.contains("Body of chapter two"));
    }

    #[test]
    fn test_running_headers_on_short_opener_not_mistaken_for_toc() {
        let sample = "\
Chapter 1: Reliability
OPENING PAGE UNIQUE TEXT.

Chapter 1: Reliability
Page two text explaining failure modes.

Chapter 1: Reliability
Page three text explaining fault tolerance.

Chapter 2: Scalability
Page four text explaining horizontal scaling.
";

        let (ch1, title1) = extract_chapter_slice(sample, 1).expect("chapter 1 slice");
        assert_eq!(title1, "Reliability");
        // Chapter 1 slice must start at the very first heading and include opening page unique text
        assert!(ch1.contains("OPENING PAGE UNIQUE TEXT."));
        assert!(ch1.contains("Page two text explaining failure modes"));
        assert!(ch1.contains("Page three text explaining fault tolerance"));
        assert!(!ch1.contains("Chapter 2: Scalability"));

        let (ch2, title2) = extract_chapter_slice(sample, 2).expect("chapter 2 slice");
        assert_eq!(title2, "Scalability");
        assert!(ch2.contains("Page four text explaining horizontal scaling"));
        assert!(!ch2.contains("OPENING PAGE UNIQUE TEXT."));
    }

    #[test]
    fn test_multiline_and_em_dash_chapter_headings() {
        let sample = "\
CHAPTER 1
Foundations of Reliability
This is the full text of chapter 1 with plenty of detailed explanations and definitions.
Reliability means continuing to work correctly even when things go wrong.

Chapter 2 — Data Models and Query Languages
This is chapter 2 text. Relational databases represent data as relations and tuples.
Many systems use this model today.
";

        let toc = extract_toc_from_text("Designing Data", None, sample);
        assert_eq!(toc.chapters.len(), 2);
        assert_eq!(toc.chapters[0].title, "Foundations of Reliability");
        assert_eq!(toc.chapters[1].title, "Data Models and Query Languages");

        let (ch1, t1) = extract_chapter_slice(sample, 1).unwrap();
        assert_eq!(t1, "Foundations of Reliability");
        assert!(ch1.contains("Reliability means continuing to work correctly"));

        let (ch2, t2) = extract_chapter_slice(sample, 2).unwrap();
        assert_eq!(t2, "Data Models and Query Languages");
        assert!(ch2.contains("Relational databases represent data"));
    }

    #[test]
    fn test_lone_toc_heading_without_body_returns_none() {
        let sample = "Chapter 1 Foundations\nChapter 2 Scalability\n";
        // Neither heading has body content
        assert!(extract_chapter_slice(sample, 1).is_none());
        assert!(extract_chapter_slice(sample, 2).is_none());
    }
}
