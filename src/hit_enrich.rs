//! Agent-facing enrichment for [`PaperHit`]: hit_id, ids, match, access, next, detail.

use crate::models::{
    AccessInfo, ContentState, MatchInfo, MatchKind, PaperHit, PaperIds, PaperSource, SearchDetail,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Mint a stable `hit_id` and fill ids/access/next (match filled separately).
pub fn enrich_hit_identity(hit: &mut PaperHit) {
    let research_hash = hit
        .hit_id
        .as_deref()
        .and_then(|id| id.strip_prefix("research:"))
        .filter(|hash| !hash.is_empty())
        .map(str::to_string);
    let arxiv = hit
        .arxiv_id
        .as_deref()
        .map(strip_arxiv_version)
        .filter(|s| !s.is_empty());
    let doi = hit
        .doi
        .as_deref()
        .map(|d| {
            d.trim()
                .trim_start_matches("https://doi.org/")
                .trim_start_matches("http://doi.org/")
                .trim_start_matches("doi:")
                .to_ascii_lowercase()
        })
        .filter(|s| !s.is_empty());
    let pmid = hit
        .pmid
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let paper_id = hit.cache.as_ref().map(|c| c.paper_id.clone());

    hit.ids = Some(PaperIds {
        doi: doi.clone(),
        arxiv: arxiv.clone(),
        pmid: pmid.clone(),
        zotero_key: None,
        paper_id: paper_id.clone(),
        research_hash: research_hash.clone(),
    });

    hit.hit_id = Some(if let Some(ref hash) = research_hash {
        format!("research:{hash}")
    } else if let Some(ref a) = arxiv {
        format!("arxiv:{a}")
    } else if let Some(ref d) = doi {
        format!("doi:{d}")
    } else if let Some(ref p) = pmid {
        format!("pmid:{p}")
    } else if let Some(ref p) = paper_id {
        format!("paperseed:{p}")
    } else if let Some(url) = hit
        .oa_pdf_url
        .as_deref()
        .or(hit.pdf_url.as_deref())
        .or(hit.url.as_deref())
        .filter(|u| !u.is_empty())
    {
        // URL-only hits must remain openable without server-side search state.
        // Prefer a PDF URL so `open_paper { hit_id }` can retrieve content.
        format!("url:{url}")
    } else {
        format!("title:{}", short_hash(&hit.title))
    });

    let existing_access = hit.access.take();
    let pdf = existing_access.as_ref().is_some_and(|a| a.pdf)
        || hit.pdf_url.is_some()
        || hit.oa_pdf_url.is_some();
    let cached = existing_access.as_ref().is_some_and(|a| a.cached)
        || hit.cache.as_ref().is_some_and(|c| c.cached);
    let full_text = existing_access.as_ref().is_some_and(|a| a.full_text)
        || hit.cache.as_ref().is_some_and(|c| c.has_full_text);
    let content_state = existing_access
        .and_then(|access| access.content_state)
        .or_else(|| {
            (hit.source == PaperSource::Research).then_some(if full_text {
                ContentState::Ready
            } else {
                ContentState::Stale
            })
        });
    hit.access = Some(AccessInfo {
        pdf,
        cached,
        full_text,
        content_state,
    });

    let mut next = Vec::new();
    if cached || full_text {
        next.push("open_paper".into());
        next.push("get_paper_structure".into());
    } else if pdf || doi.is_some() || arxiv.is_some() {
        next.push("open_paper".into());
        if doi.is_some() {
            next.push("resolve_doi".into());
        }
    } else if doi.is_some() {
        next.push("resolve_doi".into());
    }
    hit.next = next;
}

pub fn enrich_match(hit: &mut PaperHit, query: &str) {
    let kind = classify_match(query, hit);
    let score = match kind {
        MatchKind::ExactId => Some(1.0),
        MatchKind::ExactTitle => Some(0.95),
        MatchKind::Phrase => Some(0.8),
        MatchKind::Tokens => Some(0.5),
        MatchKind::Weak => Some(0.2),
    };
    hit.match_info = Some(MatchInfo { kind, score });
}

pub fn apply_detail(hit: &mut PaperHit, detail: SearchDetail, abstract_max_chars: Option<usize>) {
    // Cap authors in compact mode for token cost.
    if detail == SearchDetail::Compact && hit.authors.len() > 3 {
        hit.authors.truncate(3);
    }

    match detail {
        SearchDetail::Compact => {
            hit.abstract_note = None;
            // Drop verbose venue noise optionally? keep venue if short.
            if hit.venue.as_ref().is_some_and(|v| v.len() > 80) {
                hit.venue = None;
            }
        }
        SearchDetail::Full => {
            // Full detail remains token-bounded by default. An explicit zero
            // opts into the complete abstract, matching the wire contract.
            let max = abstract_max_chars.unwrap_or(280);
            if max != 0
                && let Some(abs) = hit.abstract_note.as_mut()
                && abs.chars().count() > max
            {
                *abs = abs
                    .chars()
                    .take(max.saturating_sub(1))
                    .chain(std::iter::once('…'))
                    .collect();
            }
        }
    }
}

fn classify_match(query: &str, hit: &PaperHit) -> MatchKind {
    let q = normalize(query);
    if q.is_empty() {
        return MatchKind::Weak;
    }

    // ID shapes
    if let Some(doi) = normalize_doi_str(query)
        && hit.doi.as_deref().and_then(normalize_doi_str).as_deref() == Some(doi.as_str())
    {
        return MatchKind::ExactId;
    }
    if let Some(arxiv) = normalize_arxiv_str(query) {
        let hit_a = hit
            .arxiv_id
            .as_deref()
            .map(strip_arxiv_version)
            .map(|s| s.to_ascii_lowercase());
        if hit_a.as_deref() == Some(arxiv.as_str()) {
            return MatchKind::ExactId;
        }
    }

    let title = normalize(&hit.title);
    if !title.is_empty() && title == q {
        return MatchKind::ExactTitle;
    }
    if !title.is_empty() && title.contains(&q) && q.split_whitespace().count() >= 2 {
        return MatchKind::Phrase;
    }

    let q_tokens: Vec<&str> = q.split_whitespace().collect();
    let t_tokens: Vec<&str> = title.split_whitespace().collect();
    if !q_tokens.is_empty() {
        let matches = q_tokens
            .iter()
            .filter(|t| t_tokens.iter().any(|tt| tt == *t))
            .count();
        if matches == q_tokens.len() {
            return MatchKind::Tokens;
        }
        if matches.saturating_mul(2) >= q_tokens.len() {
            return MatchKind::Tokens;
        }
    }
    MatchKind::Weak
}

fn normalize(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_doi_str(raw: &str) -> Option<String> {
    let lowered = raw.trim().to_lowercase();
    let n = lowered
        .strip_prefix("https://doi.org/")
        .or_else(|| lowered.strip_prefix("http://doi.org/"))
        .or_else(|| lowered.strip_prefix("doi:"))
        .unwrap_or(lowered.as_str())
        .trim();
    if n.contains('/') && n.len() > 6 {
        Some(n.to_string())
    } else {
        None
    }
}

fn normalize_arxiv_str(raw: &str) -> Option<String> {
    let lowered = raw.trim().to_lowercase();
    let id = lowered
        .strip_prefix("https://arxiv.org/abs/")
        .or_else(|| lowered.strip_prefix("http://arxiv.org/abs/"))
        .or_else(|| lowered.strip_prefix("arxiv:"))
        .unwrap_or(lowered.as_str())
        .trim();
    if id.is_empty() {
        return None;
    }
    let base = strip_arxiv_version(id);
    // new-style arxiv ids look like 1706.03762
    if base.chars().any(|c| c.is_ascii_digit()) && base.contains('.') {
        Some(base.to_ascii_lowercase())
    } else {
        None
    }
}

fn strip_arxiv_version(id: &str) -> String {
    if let Some(idx) = id.rfind('v') {
        let (base, ver) = id.split_at(idx);
        if ver.len() > 1 && ver[1..].chars().all(|c| c.is_ascii_digit()) {
            return base.to_string();
        }
    }
    id.to_string()
}

fn short_hash(s: &str) -> String {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    format!("{:012x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PaperSource;

    #[test]
    fn hit_id_prefers_arxiv() {
        let mut hit = PaperHit::new(
            PaperSource::Arxiv,
            "Attention Is All You Need".into(),
            vec!["Vaswani".into()],
            Some("2017".into()),
            None,
            Some("1706.03762v7".into()),
            None,
            None,
            Some("https://arxiv.org/abs/1706.03762".into()),
            Some("https://arxiv.org/pdf/1706.03762".into()),
            Some("https://arxiv.org/pdf/1706.03762".into()),
            Some("arXiv".into()),
            None,
        );
        enrich_hit_identity(&mut hit);
        assert_eq!(hit.hit_id.as_deref(), Some("arxiv:1706.03762"));
        assert!(hit.access.as_ref().is_some_and(|a| a.pdf));
    }

    #[test]
    fn research_hit_preserves_hash_and_availability() {
        let mut hit = PaperHit::new(
            PaperSource::Research,
            "Which Component Drives Detection?".into(),
            vec![],
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        hit.hit_id = Some("research:abc123".into());
        hit.access = Some(AccessInfo {
            pdf: false,
            cached: true,
            full_text: true,
            content_state: Some(ContentState::Ready),
        });

        enrich_hit_identity(&mut hit);

        assert_eq!(hit.hit_id.as_deref(), Some("research:abc123"));
        assert_eq!(
            hit.ids.and_then(|ids| ids.research_hash),
            Some("abc123".into())
        );
        assert_eq!(
            hit.access.and_then(|access| access.content_state),
            Some(ContentState::Ready)
        );
        assert_eq!(hit.next, vec!["open_paper", "get_paper_structure"]);
    }

    #[test]
    fn exact_title_match_kind() {
        let mut hit = PaperHit::new(
            PaperSource::Crossref,
            "Attention Is All You Need".into(),
            vec![],
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        enrich_match(&mut hit, "attention is all you need");
        assert_eq!(hit.match_info.unwrap().kind, MatchKind::ExactTitle);
    }

    #[test]
    fn compact_strips_abstract() {
        let mut hit = PaperHit::new(
            PaperSource::Arxiv,
            "T".into(),
            vec!["A".into(), "B".into(), "C".into(), "D".into()],
            None,
            None,
            None,
            None,
            Some("long abstract text".into()),
            None,
            None,
            None,
            None,
            None,
        );
        apply_detail(&mut hit, SearchDetail::Compact, None);
        assert!(hit.abstract_note.is_none());
        assert_eq!(hit.authors.len(), 3);
    }

    #[test]
    fn full_detail_caps_abstract_by_default() {
        let mut hit = PaperHit::new(
            PaperSource::Arxiv,
            "T".into(),
            vec![],
            None,
            None,
            None,
            None,
            Some("a".repeat(300)),
            None,
            None,
            None,
            None,
            None,
        );
        apply_detail(&mut hit, SearchDetail::Full, None);
        assert_eq!(hit.abstract_note.unwrap().chars().count(), 280);
    }

    #[test]
    fn full_detail_zero_keeps_complete_abstract() {
        let abstract_note = "complete abstract".repeat(30);
        let mut hit = PaperHit::new(
            PaperSource::Arxiv,
            "T".into(),
            vec![],
            None,
            None,
            None,
            None,
            Some(abstract_note.clone()),
            None,
            None,
            None,
            None,
            None,
        );
        apply_detail(&mut hit, SearchDetail::Full, Some(0));
        assert_eq!(hit.abstract_note.as_deref(), Some(abstract_note.as_str()));
    }

    #[test]
    fn url_only_pdf_hit_has_openable_hit_id() {
        let mut hit = PaperHit::new(
            PaperSource::OpenReview,
            "T".into(),
            vec![],
            None,
            None,
            None,
            None,
            None,
            Some("https://openreview.net/forum?id=abc".into()),
            Some("https://openreview.net/pdf?id=abc".into()),
            Some("https://openreview.net/pdf?id=abc".into()),
            None,
            None,
        );
        enrich_hit_identity(&mut hit);
        assert_eq!(
            hit.hit_id.as_deref(),
            Some("url:https://openreview.net/pdf?id=abc")
        );
        assert_eq!(hit.next, vec!["open_paper"]);
    }
}
