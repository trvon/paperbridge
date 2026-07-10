use crate::models::{License, PaperMetadata};
use crate::policy::parse_license;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegalSource {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: SourceKind,
    pub supports_metadata: bool,
    pub supports_open_pdf: bool,
    pub notes: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    Metadata,
    OpenAccessRepository,
    LicenseResolver,
    UserImport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchPlan {
    pub doi: String,
    pub source: Option<String>,
    pub allowed_sources: Vec<&'static str>,
    pub policy: &'static str,
    pub next_step: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaperbridgeMetadata {
    pub title: Option<String>,
    pub doi: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<u16>,
    pub venue: Option<String>,
    pub license: Option<String>,
    pub source_url: Option<String>,
}

pub fn legal_sources() -> Vec<LegalSource> {
    vec![
        LegalSource {
            id: "openalex",
            name: "OpenAlex",
            kind: SourceKind::LicenseResolver,
            supports_metadata: true,
            supports_open_pdf: true,
            notes: "Metadata plus best open-access PDF/location resolution.",
        },
        LegalSource {
            id: "crossref",
            name: "Crossref",
            kind: SourceKind::Metadata,
            supports_metadata: true,
            supports_open_pdf: false,
            notes: "DOI metadata resolution.",
        },
        LegalSource {
            id: "unpaywall",
            name: "Unpaywall",
            kind: SourceKind::LicenseResolver,
            supports_metadata: true,
            supports_open_pdf: true,
            notes: "Open-access location and license resolution.",
        },
        LegalSource {
            id: "arxiv",
            name: "arXiv",
            kind: SourceKind::OpenAccessRepository,
            supports_metadata: true,
            supports_open_pdf: true,
            notes: "Preprints and open PDFs hosted by arXiv.",
        },
        LegalSource {
            id: "pmc",
            name: "PubMed Central",
            kind: SourceKind::OpenAccessRepository,
            supports_metadata: true,
            supports_open_pdf: true,
            notes: "Biomedical open-access corpus.",
        },
        LegalSource {
            id: "user-import",
            name: "User import",
            kind: SourceKind::UserImport,
            supports_metadata: true,
            supports_open_pdf: true,
            notes: "Files the user already has rights to store locally.",
        },
    ]
}

pub fn fetch_plan(doi: impl Into<String>, source: Option<String>) -> FetchPlan {
    FetchPlan {
        doi: doi.into(),
        source,
        allowed_sources: legal_sources()
            .into_iter()
            .filter(|source| source.supports_open_pdf)
            .map(|source| source.id)
            .collect(),
        policy: "download only when an open-access/public-domain license is known",
        next_step: "wire an HTTP resolver or pass an already-authorized file through import/ingest",
    }
}

pub fn metadata_from_paperbridge_json(raw: &str) -> serde_json::Result<PaperbridgeMetadata> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    Ok(PaperbridgeMetadata {
        title: first_string(&value, &["title", "data.title", "metadata.title"]),
        doi: first_string(
            &value,
            &["doi", "DOI", "data.DOI", "data.doi", "metadata.doi"],
        ),
        authors: authors(&value),
        year: first_u16(&value, &["year", "metadata.year"]).or_else(|| year_from_date(&value)),
        venue: first_string(
            &value,
            &[
                "venue",
                "publicationTitle",
                "data.publicationTitle",
                "metadata.venue",
            ],
        ),
        license: first_string(&value, &["license", "metadata.license", "data.rights"]),
        source_url: first_string(
            &value,
            &["url", "data.url", "metadata.source_url", "source_url"],
        ),
    })
}

pub fn apply_metadata(base: &mut PaperMetadata, metadata: PaperbridgeMetadata) {
    if let Some(title) = metadata.title {
        base.title = title;
    }
    if metadata.doi.is_some() {
        base.doi = metadata.doi;
    }
    if !metadata.authors.is_empty() {
        base.authors = metadata.authors;
    }
    if metadata.year.is_some() {
        base.year = metadata.year;
    }
    if metadata.venue.is_some() {
        base.venue = metadata.venue;
    }
    if let Some(license) = metadata.license {
        let parsed = parse_license(&license);
        if parsed != License::Unknown {
            base.license = parsed;
        }
    }
    if metadata.source_url.is_some() {
        base.source_url = metadata.source_url;
    }
}

fn first_string(value: &serde_json::Value, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        get_path(value, path)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn first_u16(value: &serde_json::Value, paths: &[&str]) -> Option<u16> {
    paths.iter().find_map(|path| {
        get_path(value, path).and_then(|value| {
            value
                .as_u64()
                .and_then(|year| u16::try_from(year).ok())
                .or_else(|| value.as_str().and_then(|s| s.parse::<u16>().ok()))
        })
    })
}

fn year_from_date(value: &serde_json::Value) -> Option<u16> {
    first_string(value, &["date", "data.date", "metadata.date"]).and_then(|date| {
        date.split(|c: char| !c.is_ascii_digit())
            .find(|part| part.len() == 4)
            .and_then(|year| year.parse::<u16>().ok())
    })
}

fn authors(value: &serde_json::Value) -> Vec<String> {
    get_path(value, "authors")
        .or_else(|| get_path(value, "metadata.authors"))
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .filter(|authors: &Vec<String>| !authors.is_empty())
        .or_else(|| zotero_creators(value))
        .unwrap_or_default()
}

fn zotero_creators(value: &serde_json::Value) -> Option<Vec<String>> {
    let creators = get_path(value, "creators")
        .or_else(|| get_path(value, "data.creators"))?
        .as_array()?;
    let names: Vec<String> = creators
        .iter()
        .filter_map(|creator| {
            let first = creator.get("firstName").and_then(|value| value.as_str());
            let last = creator.get("lastName").and_then(|value| value.as_str());
            match (first, last) {
                (Some(first), Some(last)) => Some(format!("{first} {last}")),
                (None, Some(last)) => Some(last.to_string()),
                _ => creator
                    .get("name")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
            }
        })
        .collect();
    (!names.is_empty()).then_some(names)
}

fn get_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    path.split('.')
        .try_fold(value, |current, segment| current.get(segment))
}
