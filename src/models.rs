use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct CreatorInput {
    pub creator_type: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct TagInput {
    pub tag: String,
    #[serde(rename = "type", alias = "tag_type")]
    pub tag_type: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct CollectionWriteRequest {
    pub name: String,
    pub parent_collection: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct CollectionUpdateRequest {
    pub key: String,
    pub version: Option<u64>,
    pub name: Option<String>,
    pub parent_collection: Option<String>,
    /// When true, clears parent_collection. Defaults to false when omitted.
    #[serde(default)]
    #[schemars(default)]
    pub clear_parent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct DeleteCollectionRequest {
    pub key: String,
    pub version: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct ItemWriteRequest {
    pub item_type: String,
    pub title: Option<String>,
    #[serde(default)]
    #[schemars(default)]
    pub creators: Vec<CreatorInput>,
    pub abstract_note: Option<String>,
    pub date: Option<String>,
    pub url: Option<String>,
    pub doi: Option<String>,
    pub isbn: Option<String>,
    #[serde(default)]
    #[schemars(default)]
    pub tags: Vec<TagInput>,
    #[serde(default)]
    #[schemars(default)]
    pub collections: Vec<String>,
    pub extra: Option<String>,
    pub parent_item: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct ItemUpdateRequest {
    pub key: String,
    pub version: Option<u64>,
    pub item_type: Option<String>,
    pub title: Option<String>,
    pub creators: Option<Vec<CreatorInput>>,
    pub abstract_note: Option<String>,
    pub date: Option<String>,
    pub url: Option<String>,
    pub doi: Option<String>,
    pub isbn: Option<String>,
    pub tags: Option<Vec<TagInput>>,
    pub collections: Option<Vec<String>>,
    pub extra: Option<String>,
    pub parent_item: Option<String>,
    /// When true, clears parent_item. Defaults to false when omitted (agent-friendly).
    #[serde(default)]
    #[schemars(default)]
    pub clear_parent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct DeleteItemRequest {
    pub key: String,
    pub version: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct SearchItemsQuery {
    pub q: Option<String>,
    pub qmode: Option<String>,
    pub item_type: Option<String>,
    pub tag: Option<String>,
    pub limit: u32,
    pub start: u32,
}

impl Default for SearchItemsQuery {
    fn default() -> Self {
        Self {
            q: None,
            qmode: None,
            item_type: None,
            tag: None,
            limit: 10,
            start: 0,
        }
    }
}

impl SearchItemsQuery {
    pub fn normalized(mut self) -> Self {
        // Agent-safe: never allow 0/unbounded; clamp to 1..=50 (hard max 100 for power users).
        if self.limit == 0 {
            self.limit = 10;
        }
        self.limit = self.limit.clamp(1, 100);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ListCollectionsQuery {
    pub top_only: bool,
    pub limit: u32,
    pub start: u32,
}

impl Default for ListCollectionsQuery {
    fn default() -> Self {
        Self {
            top_only: false,
            limit: 50,
            start: 0,
        }
    }
}

impl ListCollectionsQuery {
    pub fn normalized(mut self) -> Self {
        self.limit = self.limit.clamp(1, 100);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum ValidationIssueLevel {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ValidationIssue {
    pub level: ValidationIssueLevel,
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ValidationReport {
    pub valid: bool,
    pub issues: Vec<ValidationIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct BackendInfo {
    pub mode: String,
    pub read_library: bool,
    pub write_basic: bool,
    pub file_upload: bool,
    pub group_libraries: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ItemSummary {
    pub key: String,
    pub item_type: String,
    pub title: String,
    pub creators: Vec<String>,
    pub year: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct AttachmentSummary {
    pub key: String,
    pub title: String,
    pub content_type: Option<String>,
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ItemDetail {
    pub key: String,
    pub version: Option<u64>,
    pub item_type: String,
    pub title: String,
    pub creators: Vec<String>,
    pub year: Option<String>,
    pub abstract_note: Option<String>,
    pub url: Option<String>,
    pub date: Option<String>,
    pub tags: Vec<TagInput>,
    pub collections: Vec<String>,
    pub extra: Option<String>,
    pub parent_item: Option<String>,
    pub attachments: Vec<AttachmentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct CollectionSummary {
    pub key: String,
    pub name: String,
    pub parent_collection: Option<String>,
    pub item_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct FulltextContent {
    pub item_key: String,
    pub content: String,
    pub indexed_pages: Option<u32>,
    pub total_pages: Option<u32>,
    pub indexed_chars: Option<u32>,
    pub total_chars: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct VoxTextPayload {
    pub source: String,
    pub chunk_count: usize,
    pub chunks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ItemVoxPayload {
    pub item_key: String,
    pub item_title: String,
    pub attachment: AttachmentSummary,
    pub indexed_pages: Option<u32>,
    pub total_pages: Option<u32>,
    pub indexed_chars: Option<u32>,
    pub total_chars: Option<u32>,
    pub vox: VoxTextPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct SearchVoxPayload {
    pub query: String,
    pub result_index: usize,
    pub result_count: usize,
    pub selected_item: ItemSummary,
    pub prepared: ItemVoxPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct CrossrefWork {
    pub doi: String,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<String>,
    pub journal: Option<String>,
    pub abstract_note: Option<String>,
    pub url: Option<String>,
    pub publisher: Option<String>,
    pub item_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oa_pdf_url: Option<String>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    Eq,
    PartialEq,
    Hash,
    schemars::JsonSchema,
    clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum PaperSource {
    Arxiv,
    /// Documents from the user's YAMS-indexed research workspace.
    #[value(name = "research", alias = "yams")]
    #[serde(rename = "research", alias = "yams")]
    Research,
    #[value(name = "paperseed", alias = "local_cache", alias = "cache")]
    #[serde(alias = "local_cache", alias = "cache")]
    Paperseed,
    #[value(name = "hugging_face", alias = "huggingface", alias = "hf")]
    #[serde(alias = "huggingface", alias = "hf")]
    HuggingFace,
    #[value(name = "semantic_scholar", alias = "semanticscholar", alias = "s2")]
    #[serde(alias = "semanticscholar", alias = "s2")]
    SemanticScholar,
    Crossref,
    /// Canonical wire name: `openalex` (aliases: `open_alex`, `oa`).
    #[value(name = "openalex", alias = "open_alex", alias = "oa")]
    #[serde(rename = "openalex", alias = "open_alex", alias = "oa")]
    OpenAlex,
    #[value(name = "europe_pmc", alias = "europepmc", alias = "epmc")]
    #[serde(alias = "europepmc", alias = "epmc")]
    EuropePmc,
    #[value(name = "dblp")]
    Dblp,
    /// Canonical wire name: `openreview` (aliases: `open_review`, `or`).
    #[value(name = "openreview", alias = "open_review", alias = "or")]
    #[serde(rename = "openreview", alias = "open_review", alias = "or")]
    OpenReview,
    #[value(name = "core")]
    Core,
    #[value(name = "ads", alias = "nasa_ads", alias = "nasaads")]
    #[serde(alias = "nasa_ads", alias = "nasaads")]
    Ads,
    #[value(name = "pubmed", alias = "pm")]
    #[serde(alias = "pm")]
    Pubmed,
    /// Canonical wire name: `scholarapi` (aliases: `scholar_api`, `scholar`, …).
    #[value(
        name = "scholarapi",
        alias = "scholar_api",
        alias = "scholar",
        alias = "scolarapi"
    )]
    #[serde(
        rename = "scholarapi",
        alias = "scholar_api",
        alias = "scholar",
        alias = "scolarapi"
    )]
    ScholarApi,
}

/// How much detail to return for paper search hits.
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    Eq,
    PartialEq,
    Hash,
    schemars::JsonSchema,
    clap::ValueEnum,
    Default,
)]
#[serde(rename_all = "snake_case")]
pub enum SearchDetail {
    /// Title, authors (capped), year, ids, match, access, next — no full abstract.
    #[default]
    Compact,
    /// Full hit fields including abstract (still respects abstract_max_chars when set).
    Full,
}

/// Why a hit matched the query (agent-facing ranking signal).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, Hash, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    ExactId,
    ExactTitle,
    Phrase,
    Tokens,
    Weak,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct MatchInfo {
    pub kind: MatchKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Default, schemars::JsonSchema)]
pub struct PaperIds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doi: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arxiv: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pmid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zotero_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentState {
    Ready,
    MetadataOnly,
    Queued,
    Stale,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct AccessInfo {
    pub pdf: bool,
    pub cached: bool,
    pub full_text: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_state: Option<ContentState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Default, schemars::JsonSchema)]
pub struct SourceDiagnostic {
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Default, schemars::JsonSchema)]
pub struct SearchDiagnostics {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources_ok: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources_skipped: Vec<SourceDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources_failed: Vec<SourceDiagnostic>,
}

/// Paginated list envelope for library item search.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ItemListResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    pub total_count: u32,
    pub offset: u32,
    pub limit: u32,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
    pub hits: Vec<ItemSummary>,
}

/// Paginated list envelope for collections.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct CollectionListResult {
    pub total_count: u32,
    pub offset: u32,
    pub limit: u32,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
    pub hits: Vec<CollectionSummary>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    Eq,
    PartialEq,
    Hash,
    schemars::JsonSchema,
    clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum SearchCacheMode {
    /// Default behavior: use cache annotations and surface cache-only hits only
    /// when they pass a strong relevance gate.
    Auto,
    /// Blend cached papers into the main result list.
    Include,
    /// Search only the local Paperseed cache.
    Only,
    /// Do not query or annotate the local cache.
    Off,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct PaperMetadata {
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    #[serde(rename = "abstract", alias = "abstract_note", alias = "abstractNote")]
    pub abstract_note: Option<String>,
    pub doi: Option<String>,
    pub year: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaperSectionKind {
    Abstract,
    Introduction,
    Background,
    RelatedWork,
    Method,
    Design,
    Implementation,
    Evaluation,
    Results,
    Discussion,
    Limitations,
    Conclusion,
    Acknowledgements,
    References,
    Appendix,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct PaperSection {
    pub id: String,
    pub heading: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<PaperSectionKind>,
    pub level: u8,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subsections: Vec<PaperSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct PaperReference {
    pub id: String,
    pub raw: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    pub title: Option<String>,
    pub year: Option<String>,
    pub doi: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct PaperFigure {
    pub id: String,
    pub label: Option<String>,
    pub caption: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaperStructureSource {
    Grobid,
    ZoteroFulltext,
    GrobidUnavailable { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct PaperStructure {
    pub item_key: String,
    pub attachment_key: Option<String>,
    pub metadata: PaperMetadata,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<PaperSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<PaperReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub figures: Vec<PaperFigure>,
    pub source: PaperStructureSource,
}

/// A deterministic SKILL.md scaffold generated from a `PaperStructure`.
///
/// `name` and `description` are the YAML frontmatter fields; `markdown` is the
/// full skill document (frontmatter + body). paperbridge only produces a
/// scaffold — mechanical mapping of paper structure to markdown — leaving the
/// procedural judgment to the consuming agent.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct SkillPayload {
    pub name: String,
    pub description: String,
    pub markdown: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct PaperHit {
    /// Stable agent-facing id (`arxiv:…`, `doi:…`, `pmid:…`, `paperseed:…`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hit_id: Option<String>,
    pub source: PaperSource,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<String>,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pmid: Option<String>,
    #[serde(
        rename = "abstract",
        alias = "abstract_note",
        alias = "abstractNote",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub abstract_note: Option<String>,
    pub url: Option<String>,
    pub pdf_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oa_pdf_url: Option<String>,
    pub venue: Option<String>,
    pub citation_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CachedPaperSummary>,
    /// Optional BM25F relevance score for cache hits. Advisory — used to
    /// break ties in downstream ranking when other signals are equal. `None`
    /// for external hits (no comparable score available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relevance_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ids: Option<PaperIds>,
    #[serde(rename = "match", default, skip_serializing_if = "Option::is_none")]
    pub match_info: Option<MatchInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next: Vec<String>,
}

impl PaperHit {
    /// Construct a hit with enrichment fields left empty (filled later by search).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: PaperSource,
        title: String,
        authors: Vec<String>,
        year: Option<String>,
        doi: Option<String>,
        arxiv_id: Option<String>,
        pmid: Option<String>,
        abstract_note: Option<String>,
        url: Option<String>,
        pdf_url: Option<String>,
        oa_pdf_url: Option<String>,
        venue: Option<String>,
        citation_count: Option<u32>,
    ) -> Self {
        Self {
            hit_id: None,
            source,
            title,
            authors,
            year,
            doi,
            arxiv_id,
            pmid,
            abstract_note,
            url,
            pdf_url,
            oa_pdf_url,
            venue,
            citation_count,
            cache: None,
            relevance_score: None,
            ids: None,
            match_info: None,
            access: None,
            next: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct SearchPapersResult {
    pub query: String,
    pub total_count: u32,
    pub offset: u32,
    pub limit: u32,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<SearchDetail>,
    pub hits: Vec<PaperHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<SearchDiagnostics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct CachedPaperSummary {
    pub paper_id: String,
    pub cached: bool,
    pub has_full_text: bool,
    #[serde(default)]
    pub yams_indexed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, schemars::JsonSchema)]
pub struct CachedPaperDetail {
    pub paper_id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<String>,
    pub doi: Option<String>,
    pub arxiv_id: Option<String>,
    pub venue: Option<String>,
    pub abstract_note: Option<String>,
    pub source_url: Option<String>,
    pub stored_path: String,
    pub mime: String,
    pub yams_hash: Option<String>,
    pub has_full_text: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_limit_clamps_to_valid_range() {
        assert_eq!(
            SearchItemsQuery {
                limit: 999,
                ..SearchItemsQuery::default()
            }
            .normalized()
            .limit,
            100
        );
        assert_eq!(
            SearchItemsQuery {
                limit: 5,
                ..SearchItemsQuery::default()
            }
            .normalized()
            .limit,
            5
        );
    }

    #[test]
    fn collection_query_limit_clamps_to_valid_range() {
        assert_eq!(
            ListCollectionsQuery {
                limit: 0,
                ..ListCollectionsQuery::default()
            }
            .normalized()
            .limit,
            1
        );
        assert_eq!(
            ListCollectionsQuery {
                limit: 1000,
                ..ListCollectionsQuery::default()
            }
            .normalized()
            .limit,
            100
        );
    }

    #[test]
    fn paper_source_deserializes_advertised_aliases() {
        fn parse(s: &str) -> PaperSource {
            serde_json::from_str(&format!("\"{s}\"")).expect("valid PaperSource alias")
        }

        assert_eq!(parse("openalex"), PaperSource::OpenAlex);
        assert_eq!(parse("open_alex"), PaperSource::OpenAlex);
        assert_eq!(parse("oa"), PaperSource::OpenAlex);

        assert_eq!(parse("research"), PaperSource::Research);
        assert_eq!(parse("yams"), PaperSource::Research);

        assert_eq!(parse("openreview"), PaperSource::OpenReview);
        assert_eq!(parse("open_review"), PaperSource::OpenReview);
        assert_eq!(parse("or"), PaperSource::OpenReview);

        assert_eq!(parse("scholarapi"), PaperSource::ScholarApi);
        assert_eq!(parse("scholar_api"), PaperSource::ScholarApi);
        assert_eq!(parse("scholar"), PaperSource::ScholarApi);

        assert_eq!(parse("hugging_face"), PaperSource::HuggingFace);
        assert_eq!(parse("huggingface"), PaperSource::HuggingFace);
        assert_eq!(parse("hf"), PaperSource::HuggingFace);

        assert_eq!(parse("semantic_scholar"), PaperSource::SemanticScholar);
        assert_eq!(parse("semanticscholar"), PaperSource::SemanticScholar);
        assert_eq!(parse("s2"), PaperSource::SemanticScholar);

        assert_eq!(parse("europe_pmc"), PaperSource::EuropePmc);
        assert_eq!(parse("europepmc"), PaperSource::EuropePmc);
        assert_eq!(parse("epmc"), PaperSource::EuropePmc);

        assert_eq!(parse("ads"), PaperSource::Ads);
        assert_eq!(parse("nasa_ads"), PaperSource::Ads);

        assert_eq!(parse("pubmed"), PaperSource::Pubmed);
        assert_eq!(parse("pm"), PaperSource::Pubmed);
    }

    #[test]
    fn paper_source_serializes_to_canonical_wire_names() {
        assert_eq!(
            serde_json::to_string(&PaperSource::Research).unwrap(),
            "\"research\""
        );
        assert_eq!(
            serde_json::to_string(&PaperSource::OpenAlex).unwrap(),
            "\"openalex\""
        );
        assert_eq!(
            serde_json::to_string(&PaperSource::OpenReview).unwrap(),
            "\"openreview\""
        );
        assert_eq!(
            serde_json::to_string(&PaperSource::ScholarApi).unwrap(),
            "\"scholarapi\""
        );
    }

    #[test]
    fn item_write_request_defaults_empty_collections() {
        let item: ItemWriteRequest =
            serde_json::from_str(r#"{"item_type":"journalArticle","title":"T"}"#).unwrap();
        assert!(item.creators.is_empty());
        assert!(item.tags.is_empty());
        assert!(item.collections.is_empty());
    }

    #[test]
    fn item_update_request_defaults_clear_parent_false() {
        let item: ItemUpdateRequest =
            serde_json::from_str(r#"{"key":"ABCD1234","title":"T"}"#).unwrap();
        assert!(!item.clear_parent);
    }

    #[test]
    fn normalized_limit_zero_becomes_default_ten() {
        assert_eq!(
            SearchItemsQuery {
                limit: 0,
                ..SearchItemsQuery::default()
            }
            .normalized()
            .limit,
            10
        );
    }
}
