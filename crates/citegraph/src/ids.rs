//! Identifier normalization and the strong-ID veto, shared by the store and
//! paperbridge's search/dedupe paths.

use serde::{Deserialize, Serialize};

/// External identifier schemes stored in `work_ids.scheme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scheme {
    Doi,
    Arxiv,
    Pmid,
    Openalex,
    SemanticScholar,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Doi => "doi",
            Self::Arxiv => "arxiv",
            Self::Pmid => "pmid",
            Self::Openalex => "openalex",
            Self::SemanticScholar => "semantic_scholar",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "doi" => Self::Doi,
            "arxiv" => Self::Arxiv,
            "pmid" => Self::Pmid,
            "openalex" => Self::Openalex,
            "semantic_scholar" => Self::SemanticScholar,
            _ => return None,
        })
    }

    /// Schemes whose disagreement vetoes a merge.
    pub fn is_strong(self) -> bool {
        matches!(self, Self::Doi | Self::Arxiv | Self::Pmid)
    }
}

/// A normalized external identifier. Construct with [`ExtId::new`], which
/// applies the scheme's normalizer and rejects malformed values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExtId {
    pub scheme: Scheme,
    pub value: String,
}

impl ExtId {
    /// Normalize `raw` under `scheme`. A DOI under the arXiv prefix
    /// (`10.48550/arXiv.*`) becomes an `arxiv` id: the arXiv DOI and the
    /// arXiv id name the same preprint.
    pub fn new(scheme: Scheme, raw: &str) -> Option<Self> {
        let (scheme, value) = match scheme {
            Scheme::Doi => {
                let doi = normalize_doi(raw)?;
                match arxiv_from_doi(&doi) {
                    Some(arxiv) => (Scheme::Arxiv, arxiv),
                    None => (Scheme::Doi, doi),
                }
            }
            Scheme::Arxiv => (Scheme::Arxiv, normalize_arxiv(raw)?),
            Scheme::Pmid => {
                let p = raw.trim();
                let p = p.rsplit('/').next().unwrap_or(p).trim();
                (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| (Scheme::Pmid, p.trim_start_matches('0').to_string()))
                    .filter(|(_, v)| !v.is_empty())?
            }
            Scheme::Openalex => (Scheme::Openalex, normalize_openalex(raw)?),
            Scheme::SemanticScholar => {
                let v = raw.trim().to_ascii_lowercase();
                (!v.is_empty() && v.bytes().all(|b| b.is_ascii_hexdigit()))
                    .then_some((Scheme::SemanticScholar, v))?
            }
        };
        Some(Self { scheme, value })
    }

    pub fn doi(raw: &str) -> Option<Self> {
        Self::new(Scheme::Doi, raw)
    }

    pub fn arxiv(raw: &str) -> Option<Self> {
        Self::new(Scheme::Arxiv, raw)
    }

    pub fn openalex(raw: &str) -> Option<Self> {
        Self::new(Scheme::Openalex, raw)
    }

    pub fn semantic_scholar(raw: &str) -> Option<Self> {
        Self::new(Scheme::SemanticScholar, raw)
    }

    pub fn pmid(raw: &str) -> Option<Self> {
        Self::new(Scheme::Pmid, raw)
    }

    /// `scheme:value`, used in merge evidence and diagnostics.
    pub fn key(&self) -> String {
        format!("{}:{}", self.scheme.as_str(), self.value)
    }
}

/// OpenAlex work id `W<digits>` from a bare id or `https://openalex.org/W…`.
pub fn normalize_openalex(raw: &str) -> Option<String> {
    let t = raw.trim();
    let tail = t.rsplit('/').next().unwrap_or(t);
    let digits = tail.strip_prefix('W').or_else(|| tail.strip_prefix('w'))?;
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| format!("W{digits}"))
}

/// The arXiv id behind an arXiv DOI (`10.48550/arxiv.2401.12345`).
pub fn arxiv_from_doi(normalized_doi: &str) -> Option<String> {
    normalized_doi
        .strip_prefix("10.48550/arxiv.")
        .and_then(normalize_arxiv)
}

fn strip_scheme(s: &str) -> &str {
    s.strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s)
}

/// Percent-decode until stable. Stops at the first undecodable step (bad
/// UTF-8), keeping the last valid string.
fn percent_decode_fixpoint(raw: &str) -> String {
    let mut current = raw.to_string();
    // Each pass strictly shortens the string, so this terminates.
    while current.contains('%') {
        match urlencoding::decode(&current) {
            Ok(next) if next != current => current = next.into_owned(),
            _ => break,
        }
    }
    current
}

/// Lowercase and percent-decode `raw`, then strip DOI wrappers: `doi:`, and
/// `http(s)://`, `www.`/`dx.` + `doi.org/` resolver prefixes. Does not
/// validate or strip trailing punctuation.
pub fn strip_doi_prefix(raw: &str) -> String {
    let lowered = percent_decode_fixpoint(raw.trim()).trim().to_lowercase();
    let rest = strip_scheme(&lowered);
    let rest = rest
        .strip_prefix("www.")
        .or_else(|| rest.strip_prefix("dx."))
        .unwrap_or(rest);
    let stripped = rest
        .strip_prefix("doi.org/")
        .or_else(|| lowered.strip_prefix("doi:"))
        .unwrap_or(lowered.as_str());
    stripped.trim().to_string()
}

/// Remove sentence punctuation a DOI picked up from prose: trailing
/// `.,;:'"` and a closing `)`/`]` that has no matching opener.
fn strip_trailing_punctuation(doi: &str) -> &str {
    let mut s = doi;
    loop {
        let before = s.len();
        s = s.trim_end_matches(['.', ',', ';', ':', '\'', '"']);
        for (open, close) in [('(', ')'), ('[', ']')] {
            if s.ends_with(close) && s.matches(open).count() < s.matches(close).count() {
                s = &s[..s.len() - 1];
            }
        }
        if s.len() == before {
            return s;
        }
    }
}

fn doi_shape(n: &str) -> bool {
    n.strip_prefix("10.")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(prefix, suffix)| {
            !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_digit()) && !suffix.is_empty()
        })
        && !n.chars().any(char::is_whitespace)
}

/// Lowercase and strip DOI wrappers without percent-decoding: the form
/// paperbridge used before the shared normalizer.
fn strip_doi_prefix_raw(raw: &str) -> String {
    let lowered = raw.trim().to_lowercase();
    let rest = strip_scheme(&lowered);
    let rest = rest
        .strip_prefix("www.")
        .or_else(|| rest.strip_prefix("dx."))
        .unwrap_or(rest);
    rest.strip_prefix("doi.org/")
        .or_else(|| lowered.strip_prefix("doi:"))
        .unwrap_or(lowered.as_str())
        .trim()
        .to_string()
}

/// Canonical DOI: lowercase `10.<digits>/<suffix>`, percent-decoded, with
/// resolver wrappers and trailing prose punctuation removed and no
/// whitespace. `None` when the input is not a DOI.
///
/// Never stricter than [`normalize_doi_legacy`]: when decoding introduces
/// whitespace or stripping punctuation empties the suffix, the less
/// processed form is kept, so every DOI paperbridge accepted before still
/// normalizes.
pub fn normalize_doi(raw: &str) -> Option<String> {
    let decoded = strip_doi_prefix(raw);
    let undecoded = strip_doi_prefix_raw(raw);
    [
        strip_trailing_punctuation(&decoded),
        decoded.as_str(),
        strip_trailing_punctuation(&undecoded),
        undecoded.as_str(),
    ]
    .into_iter()
    .find(|n| doi_shape(n))
    .map(str::to_string)
}

/// The DOI form paperbridge produced before percent-decoding and
/// punctuation stripping were added (lowercase, wrappers removed).
pub fn normalize_doi_legacy(raw: &str) -> Option<String> {
    let n = strip_doi_prefix_raw(raw);
    doi_shape(&n).then_some(n)
}

/// Forms to try when resolving a DOI upstream: canonical first, then the
/// legacy form if it differs (a DOI that really ends in `.` or contains
/// `%2F`, or a `doi:` hit_id minted before the shared normalizer).
pub fn doi_lookup_forms(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for form in [normalize_doi(raw), normalize_doi_legacy(raw)]
        .into_iter()
        .flatten()
    {
        if !out.contains(&form) {
            out.push(form);
        }
    }
    out
}

/// Lowercase `raw` and strip arXiv wrappers: `arxiv:`, and
/// `http(s)://(www.|export.)arxiv.org/(abs|pdf)/` plus a trailing `.pdf`.
/// Keeps any version suffix. Does not validate.
pub fn strip_arxiv_prefix(raw: &str) -> String {
    let lowered = raw.trim().to_lowercase();
    let rest = strip_scheme(&lowered);
    let rest = rest
        .strip_prefix("www.")
        .or_else(|| rest.strip_prefix("export."))
        .unwrap_or(rest);
    let id = rest
        .strip_prefix("arxiv.org/abs/")
        .or_else(|| rest.strip_prefix("arxiv.org/pdf/"))
        .or_else(|| lowered.strip_prefix("arxiv:"))
        .unwrap_or(lowered.as_str())
        .trim();
    id.strip_suffix(".pdf").unwrap_or(id).to_string()
}

/// Canonical version-less arXiv id (`2401.12345` or `hep-th/9901001`).
/// `None` when the input is not an arXiv id.
pub fn normalize_arxiv(raw: &str) -> Option<String> {
    let id = strip_arxiv_prefix(raw);
    if id.is_empty() {
        return None;
    }
    let base = strip_arxiv_version(&id);
    let modern = base.split_once('.').is_some_and(|(date, number)| {
        date.len() == 4
            && (4..=5).contains(&number.len())
            && date
                .bytes()
                .chain(number.bytes())
                .all(|b| b.is_ascii_digit())
    });
    let legacy = base.split_once('/').is_some_and(|(archive, number)| {
        !archive.is_empty()
            && archive
                .bytes()
                .all(|b| b.is_ascii_alphabetic() || b == b'-' || b == b'.')
            && number.len() == 7
            && number.bytes().all(|b| b.is_ascii_digit())
    });
    (modern || legacy).then(|| base.to_string())
}

/// Strip a trailing arXiv version (`v<digits>`), if present.
pub fn strip_arxiv_version(id: &str) -> &str {
    id.rfind('v')
        .and_then(|idx| {
            let ver = &id[idx + 1..];
            (!ver.is_empty() && ver.bytes().all(|b| b.is_ascii_digit())).then_some(&id[..idx])
        })
        .unwrap_or(id)
}

/// Both sides report values for one strong scheme and share none of them.
/// This is the veto half of paperbridge's hit dedupe: a DOI, arXiv or PMID
/// contradiction means two records are different works.
pub fn scheme_conflict<'a, I, J>(left: I, right: J) -> bool
where
    I: IntoIterator<Item = &'a str>,
    J: IntoIterator<Item = &'a str> + Clone,
{
    let mut any_left = false;
    for l in left {
        any_left = true;
        if right.clone().into_iter().any(|r| r == l) {
            return false;
        }
    }
    any_left && right.into_iter().next().is_some()
}

/// Strong identifiers of one work (or one equivalence class of works).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StrongIds {
    pub doi: Vec<String>,
    pub arxiv: Vec<String>,
    pub pmid: Vec<String>,
}

impl StrongIds {
    pub fn push(&mut self, id: &ExtId) {
        let list = match id.scheme {
            Scheme::Doi => &mut self.doi,
            Scheme::Arxiv => &mut self.arxiv,
            Scheme::Pmid => &mut self.pmid,
            Scheme::Openalex | Scheme::SemanticScholar => return,
        };
        if !list.contains(&id.value) {
            list.push(id.value.clone());
        }
    }

    pub fn of<'a>(ids: impl IntoIterator<Item = &'a ExtId>) -> Self {
        let mut out = Self::default();
        for id in ids {
            out.push(id);
        }
        out
    }

    fn values(&self, scheme: Scheme) -> &[String] {
        match scheme {
            Scheme::Doi => &self.doi,
            Scheme::Arxiv => &self.arxiv,
            Scheme::Pmid => &self.pmid,
            Scheme::Openalex | Scheme::SemanticScholar => &[],
        }
    }

    /// True when the two sides contradict on DOI, arXiv id or PMID.
    pub fn conflicts(&self, other: &Self) -> bool {
        [Scheme::Doi, Scheme::Arxiv, Scheme::Pmid]
            .into_iter()
            .any(|s| {
                scheme_conflict(
                    self.values(s).iter().map(String::as_str),
                    other.values(s).iter().map(String::as_str),
                )
            })
    }

    /// Joining the two sides would put an arXiv id and a publisher DOI in
    /// one class. Preprint and published versions are never merged.
    pub fn mixes_versions(&self, other: &Self) -> bool {
        let all_arxiv = !self.arxiv.is_empty() || !other.arxiv.is_empty();
        let all_doi = !self.doi.is_empty() || !other.doi.is_empty();
        all_arxiv && all_doi
    }

    /// Adding `id` to this set would contradict an existing value.
    pub fn rejects(&self, id: &ExtId) -> bool {
        let existing = self.values(id.scheme);
        !existing.is_empty() && !existing.contains(&id.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn doi_wrappers_and_prose_punctuation() {
        for raw in [
            "10.1234/ABC",
            "doi:10.1234/abc",
            "https://doi.org/10.1234/abc",
            "http://dx.doi.org/10.1234/abc",
            "https://doi.org/10.1234%2Fabc",
            "https://doi.org/10.1234%252Fabc",
            "10.1234/abc.",
            "10.1234/abc,",
            "(see 10.1234/abc)".trim_start_matches("(see "),
            "10.1234/abc).",
            "\"10.1234/abc\"".trim_start_matches('"'),
        ] {
            assert_eq!(normalize_doi(raw).as_deref(), Some("10.1234/abc"), "{raw}");
        }
        assert_eq!(
            normalize_doi("10.1016/0021-9991(85)90012-X").as_deref(),
            Some("10.1016/0021-9991(85)90012-x")
        );
        assert_eq!(
            normalize_doi("10.1000/abc(1)").as_deref(),
            Some("10.1000/abc(1)")
        );
        // Never stricter than before: these were accepted and still are.
        assert_eq!(
            normalize_doi("10.1234/a%20b").as_deref(),
            Some("10.1234/a%20b")
        );
        assert_eq!(normalize_doi("10.1234/.").as_deref(), Some("10.1234/."));
        assert_eq!(normalize_doi("https://example.com/10.1234/abc"), None);
    }

    /// Each input whose output changed with the shared normalizer (review
    /// table): canonical form, legacy form, and both tried on lookup.
    #[test]
    fn changed_doi_outputs_keep_a_legacy_lookup() {
        let cases: &[(&str, &str, &str)] = &[
            ("10.1234/abc.", "10.1234/abc", "10.1234/abc."),
            ("10.1000/xyz)", "10.1000/xyz", "10.1000/xyz)"),
            ("10.1234/a%2Fb", "10.1234/a/b", "10.1234/a%2fb"),
            ("10.1234/a%20b", "10.1234/a%20b", "10.1234/a%20b"),
            ("10.1234/x%", "10.1234/x%", "10.1234/x%"),
            ("10.1234/abc;", "10.1234/abc", "10.1234/abc;"),
            ("10.1234/foo]", "10.1234/foo", "10.1234/foo]"),
            (
                "10.1002/(SICI)1097-4571(199110)42:9<676::AID-ASI6>3.0.CO;2-#",
                "10.1002/(sici)1097-4571(199110)42:9<676::aid-asi6>3.0.co;2-#",
                "10.1002/(sici)1097-4571(199110)42:9<676::aid-asi6>3.0.co;2-#",
            ),
        ];
        for &(raw, canonical, legacy) in cases {
            assert_eq!(normalize_doi(raw).as_deref(), Some(canonical), "{raw}");
            assert_eq!(normalize_doi_legacy(raw).as_deref(), Some(legacy), "{raw}");
            let forms = doi_lookup_forms(raw);
            assert_eq!(forms[0], canonical);
            assert!(forms.iter().any(|f| f == legacy), "{raw}");
            // An old `doi:` hit_id (legacy form) still resolves to the same
            // canonical id.
            assert_eq!(
                normalize_doi(legacy).as_deref(),
                Some(canonical),
                "{legacy}"
            );
        }
    }

    #[test]
    fn arxiv_doi_becomes_arxiv_id() {
        let id = ExtId::doi("https://doi.org/10.48550/arXiv.1706.03762").unwrap();
        assert_eq!(id.scheme, Scheme::Arxiv);
        assert_eq!(id.value, "1706.03762");
        assert_eq!(ExtId::doi("10.5555/x").unwrap().scheme, Scheme::Doi);
    }

    #[test]
    fn source_native_ids() {
        assert_eq!(
            ExtId::openalex("https://openalex.org/W2741809807")
                .unwrap()
                .value,
            "W2741809807"
        );
        assert!(ExtId::openalex("A123").is_none());
        assert_eq!(
            ExtId::pmid("https://pubmed.ncbi.nlm.nih.gov/0123")
                .unwrap()
                .value,
            "123"
        );
        assert!(ExtId::semantic_scholar("not hex!").is_none());
    }

    #[test]
    fn veto_matches_dedupe_semantics() {
        let a = StrongIds::of(&[ExtId::doi("10.1/a").unwrap()]);
        let b = StrongIds::of(&[ExtId::doi("10.1/b").unwrap()]);
        let none = StrongIds::default();
        assert!(a.conflicts(&b));
        assert!(!a.conflicts(&none));
        assert!(!a.conflicts(&a));
        // A preprint (arXiv id, possibly from its arXiv DOI) and a publisher DOI
        // are not a contradiction.
        let pre = StrongIds::of(&[ExtId::doi("10.48550/arxiv.1706.03762").unwrap()]);
        assert!(!pre.conflicts(&a));
        assert!(a.rejects(&ExtId::doi("10.1/b").unwrap()));
        assert!(!a.rejects(&ExtId::arxiv("1706.03762").unwrap()));
    }

    fn doi_core() -> impl Strategy<Value = String> {
        ("[0-9]{4,5}", "[a-z0-9][a-z0-9._;()/:-]{0,20}[a-z0-9]")
            .prop_map(|(prefix, suffix)| format!("10.{prefix}/{suffix}"))
    }

    proptest! {
        #[test]
        fn doi_normalization_is_idempotent(raw in ".{0,40}") {
            if let Some(n) = normalize_doi(&raw) {
                prop_assert_eq!(normalize_doi(&n), Some(n.clone()));
            }
        }

        #[test]
        fn never_stricter_than_legacy(raw in "(doi:|https://doi.org/)?10\\.[0-9]{4}/[ -~]{1,20}") {
            if normalize_doi_legacy(&raw).is_some() {
                prop_assert!(normalize_doi(&raw).is_some(), "{}", raw);
            }
        }

        #[test]
        fn doi_survives_wrapping_encoding_and_punctuation(
            core in doi_core(),
            wrap in prop::sample::select(vec!["", "doi:", "https://doi.org/", "http://dx.doi.org/"]),
            tail in prop::sample::select(vec!["", ".", ",", ";", ").", "\""]),
            encode: bool,
        ) {
            // Unbalanced parens inside the core legitimately change where a
            // trailing `)` is kept; only check balanced cores.
            prop_assume!(core.matches('(').count() == core.matches(')').count());
            let body = if encode { core.replace('/', "%2F") } else { core.clone() };
            let raw = format!("{wrap}{body}{tail}");
            prop_assert_eq!(normalize_doi(&raw), Some(core));
        }

        #[test]
        fn arxiv_normalization_is_idempotent(raw in ".{0,30}") {
            if let Some(n) = normalize_arxiv(&raw) {
                prop_assert_eq!(normalize_arxiv(&n), Some(n.clone()));
            }
        }
    }
}
