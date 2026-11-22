//! Read-through citation commands (`papers refs|cited-by|related`, MCP
//! `get_citations`) over the `citegraph` store.
//!
//! Sources: OpenAlex is primary for references (`referenced_works`),
//! cited-by (`cites:` cursor) and related works; Semantic Scholar is the
//! fallback for references and cited-by and the only source of contexts,
//! intents and `isInfluential`. Every request passes a per-call budget and a
//! per-source token bucket. Store access runs on the blocking pool and
//! never spans an HTTP await.

pub mod bucket;
mod openalex;
mod semantic_scholar;

pub use bucket::{BudgetUse, CallBudget, Rate, TokenBucket};

use crate::error::{Result, ZoteroMcpError};
use crate::hit_enrich::{apply_detail, enrich_hit_identity};
use crate::models::{CountKind, PaperHit, PaperSource, SearchDetail, SourceDiagnostic};
use bucket::BudgetLedger;
use citegraph::{
    Direction, ExtId, FetchKey, FetchState, NeighborPage, NeighborQuery, Page, PageRow, Scheme,
    Store, WorkMeta, WorkRecord,
};
use openalex::OpenAlexCitations;
use schemars::JsonSchema;
use semantic_scholar::SemanticScholarCitations;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

const OA: &str = openalex::SOURCE;
const S2: &str = semantic_scholar::SOURCE;
pub const DEFAULT_LIMIT: u32 = 10;
/// Lower than search's 50: a citation hit carries edge evidence, and 50
/// heavy hits do not fit the 64 KiB MCP budget twice over.
pub const MAX_LIMIT: u32 = 25;
const MAX_OFFSET: u32 = 100_000;
const CONTEXT_MAX_CHARS: usize = 400;
const CONTEXTS_PER_HIT: usize = 4;
/// Wire bytes for the hits before contexts are dropped, measured as the MCP
/// budget (64 KiB) counts them: the structured copy plus the JSON-escaped
/// text copy. The rest of the envelope gets the remainder.
const RESULT_SOFT_BYTES: usize = 56_000;

pub(crate) enum UpstreamError {
    NotFound,
    Status(u16),
    Failed(String),
}

impl UpstreamError {
    fn reason(&self) -> String {
        match self {
            Self::NotFound => "not_found".into(),
            Self::Status(429) => "rate_limited".into(),
            Self::Status(status) => format!("http_{status}"),
            Self::Failed(message) => {
                let safe = crate::error::sanitize_message(message, &[]);
                safe.chars().take(200).collect()
            }
        }
    }
}

pub(crate) type Fetched<T> = std::result::Result<T, UpstreamError>;

/// `$XDG_DATA_HOME/paperbridge/citegraph.sqlite` (default
/// `~/.local/share/...`), matching the Paperseed corpus root resolution.
pub fn default_store_path() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".local").join("share"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    data.join("paperbridge").join("citegraph.sqlite")
}

#[derive(Debug, Clone)]
pub struct CitationConfig {
    pub store_path: PathBuf,
    pub openalex_base: String,
    pub openalex_mailto: Option<String>,
    /// Wired when OpenAlex key configuration lands.
    pub openalex_api_key: Option<String>,
    pub semantic_scholar_base: String,
    pub semantic_scholar_api_key: Option<String>,
    pub budget: CallBudget,
    /// `(openalex, semantic_scholar)`; `None` shares the process-wide buckets.
    pub rates: Option<(Rate, Rate)>,
}

impl CitationConfig {
    pub fn new(store_path: PathBuf) -> Self {
        Self {
            store_path,
            openalex_base: "https://api.openalex.org".into(),
            openalex_mailto: None,
            openalex_api_key: None,
            semantic_scholar_base: "https://api.semanticscholar.org/graph/v1".into(),
            semantic_scholar_api_key: None,
            budget: CallBudget::default(),
            rates: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CitationDirection {
    /// Works the paper cites.
    Refs,
    /// Works citing the paper.
    CitedBy,
    /// Works OpenAlex considers related (not citations).
    Related,
}

impl From<CitationDirection> for Direction {
    fn from(d: CitationDirection) -> Self {
        match d {
            CitationDirection::Refs => Direction::Refs,
            CitationDirection::CitedBy => Direction::CitedBy,
            CitationDirection::Related => Direction::Related,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CitationsRequest {
    pub id: String,
    pub direction: CitationDirection,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub refresh: bool,
    pub include_contexts: bool,
}

/// Per-source evidence for one citation edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct EdgeSourceInfo {
    /// Canonical source wire name (`openalex`, `semantic_scholar`).
    pub source: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_influential: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct CitationContext {
    pub source: String,
    pub text: String,
}

/// A compact hit plus why it is listed and which sources report the edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct CitationHit {
    #[serde(flatten)]
    pub hit: PaperHit,
    pub why: Vec<String>,
    pub edge_sources: Vec<EdgeSourceInfo>,
    /// The fetch this edge came from did not reach the end of the list.
    pub truncated: bool,
    /// `hit_id`s of other versions (preprint / published) of this work.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<String>,
    /// Citation contexts (only with `include=contexts`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<CitationContext>,
}

/// How far each source's latest fetch got.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct CoverageInfo {
    pub source: String,
    pub generation: i64,
    pub fetched: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_reported: Option<u64>,
    /// The list was read to its end.
    pub complete: bool,
    /// The source cannot page further (e.g. Semantic Scholar's 10,000 cap).
    pub capped: bool,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct CitationDiagnostics {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources_ok: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources_skipped: Vec<SourceDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources_failed: Vec<SourceDiagnostic>,
    /// Upstream requests spent by this call, per source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub budget: Vec<BudgetUse>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// List envelope for `get_citations` / `papers refs|cited-by|related`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct CitationsResult {
    /// `hit_id` of the paper whose neighbors are listed.
    pub hit_id: String,
    pub direction: CitationDirection,
    /// Distinct cached neighbors; null unless every fetch reached the end.
    pub total_count: Option<u32>,
    pub count_kind: CountKind,
    pub offset: u32,
    pub limit: u32,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
    /// Some source's list was not read to its end; hits may be missing.
    pub truncated: bool,
    pub hits: Vec<CitationHit>,
    pub coverage: Vec<CoverageInfo>,
    pub diagnostics: CitationDiagnostics,
}

struct Call {
    ledger: BudgetLedger,
    diagnostics: CitationDiagnostics,
    conflicts: u32,
}

impl Call {
    fn ok(&mut self, source: &str) {
        if !self.diagnostics.sources_ok.iter().any(|s| s == source) {
            self.diagnostics.sources_ok.push(source.into());
        }
    }
    fn fail(&mut self, source: &str, reason: String) {
        self.diagnostics.sources_failed.push(SourceDiagnostic {
            source: source.into(),
            reason,
        });
    }
    fn skip(&mut self, source: &str, reason: &str) {
        if !self
            .diagnostics
            .sources_skipped
            .iter()
            .any(|s| s.source == source && s.reason == reason)
        {
            self.diagnostics.sources_skipped.push(SourceDiagnostic {
                source: source.into(),
                reason: reason.into(),
            });
        }
    }
    fn note(&mut self, note: &str) {
        if !self.diagnostics.notes.iter().any(|n| n == note) {
            self.diagnostics.notes.push(note.into());
        }
    }
}

struct Inner {
    config: CitationConfig,
    store: tokio::sync::OnceCell<Arc<Mutex<Store>>>,
    openalex: OpenAlexCitations,
    semantic_scholar: Option<SemanticScholarCitations>,
    buckets: (TokenBucket, TokenBucket),
}

/// Citation graph service. Cheap to clone; opens the store on first use.
#[derive(Clone)]
pub struct CitationService {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for CitationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CitationService")
            .field("store_path", &self.inner.config.store_path)
            .finish()
    }
}

impl CitationService {
    pub fn new(config: CitationConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("paperbridge/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ZoteroMcpError::Http(format!("failed to build HTTP client: {e}")))?;
        let openalex = OpenAlexCitations::new(
            client.clone(),
            &config.openalex_base,
            config.openalex_mailto.clone(),
            config.openalex_api_key.clone(),
        );
        let semantic_scholar = config
            .semantic_scholar_api_key
            .clone()
            .filter(|k| !k.trim().is_empty())
            .map(|key| SemanticScholarCitations::new(client, &config.semantic_scholar_base, key));
        let buckets = match config.rates {
            Some((oa, s2)) => (TokenBucket::new(oa), TokenBucket::new(s2)),
            None => bucket::default_buckets(),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                store: tokio::sync::OnceCell::new(),
                openalex,
                semantic_scholar,
                buckets,
            }),
        })
    }

    pub fn store_path(&self) -> &Path {
        &self.inner.config.store_path
    }

    async fn store(&self) -> Result<Arc<Mutex<Store>>> {
        let path = self.inner.config.store_path.clone();
        self.inner
            .store
            .get_or_try_init(|| async move {
                let opened = tokio::task::spawn_blocking({
                    let path = path.clone();
                    move || Store::open(&path)
                })
                .await
                .map_err(|e| ZoteroMcpError::Config(format!("citation store task failed: {e}")))?;
                opened
                    .map(|s| Arc::new(Mutex::new(s)))
                    .map_err(|e| store_error(&path, e))
            })
            .await
            .cloned()
    }

    /// Run `f` on the store on the blocking pool. One connection per
    /// process; the mutex serializes this process's writers.
    async fn db<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Store) -> citegraph::Result<T> + Send + 'static,
    {
        let store = self.store().await?;
        let path = self.inner.config.store_path.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = store.lock().unwrap_or_else(PoisonError::into_inner);
            f(&mut guard)
        })
        .await
        .map_err(|e| ZoteroMcpError::Config(format!("citation store task failed: {e}")))?
        .map_err(|e| store_error(&path, e))
    }

    /// Spend budget and wait for a rate token; false when the budget is out.
    async fn gate(&self, call: &mut Call, source: &'static str) -> bool {
        if !call.ledger.spend(source) {
            call.note(&format!("{source}_budget_exhausted"));
            return false;
        }
        let bucket = if source == OA {
            &self.inner.buckets.0
        } else {
            &self.inner.buckets.1
        };
        bucket.acquire().await;
        true
    }

    pub async fn get_citations(&self, req: CitationsRequest) -> Result<CitationsResult> {
        let limit = req.limit.unwrap_or(DEFAULT_LIMIT);
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "citation limit must be between 1 and {MAX_LIMIT}."
            )));
        }
        let offset = req.offset.unwrap_or(0);
        if offset > MAX_OFFSET {
            return Err(ZoteroMcpError::InvalidInput(format!(
                "citation offset must be at most {MAX_OFFSET}."
            )));
        }
        let subject = parse_citation_id(&req.id)?;
        let direction: Direction = req.direction.into();
        let need = (offset + limit) as u64;
        let mut call = Call {
            ledger: BudgetLedger::new(&self.inner.config.budget),
            diagnostics: CitationDiagnostics::default(),
            conflicts: 0,
        };

        let openalex_ok = self
            .fill_openalex(&mut call, &subject, direction, need, req.refresh)
            .await?;
        if direction == Direction::Related {
            if req.include_contexts {
                call.note("contexts_unavailable_for_related");
            }
        } else if !openalex_ok || req.include_contexts {
            self.fill_semantic_scholar(&mut call, &subject, direction, need, req.refresh)
                .await?;
        }

        let query = NeighborQuery {
            offset: offset as usize,
            limit: limit as usize,
            include_stale: false,
            include_contexts: req.include_contexts,
        };
        let mut page = self.read_page(&subject, direction, query).await?;
        if self.hydrate(&mut call, &page).await? {
            page = self.read_page(&subject, direction, query).await?;
        }
        let (subject_ids, coverage) = {
            let id = subject.clone();
            self.db(move |s| Ok((s.class_ids(&id)?, s.coverage(&id, direction)?)))
                .await?
        };
        if call.conflicts > 0 {
            call.note("identity_conflicts_withheld");
        }
        call.diagnostics.budget = call.ledger.report();
        Ok(build_result(
            &subject,
            &subject_ids,
            req.direction,
            offset,
            limit,
            page,
            coverage,
            call.diagnostics,
        ))
    }

    async fn read_page(
        &self,
        subject: &ExtId,
        direction: Direction,
        query: NeighborQuery,
    ) -> Result<NeighborPage> {
        let id = subject.clone();
        self.db(move |s| s.neighbors(&id, direction, query)).await
    }

    /// The subject's ids, then its versions' ids (so a preprint subject
    /// finds the source id recorded on its published version).
    async fn class_ids(&self, id: &ExtId) -> Result<Vec<ExtId>> {
        let id = id.clone();
        self.db(move |s| s.linked_ids(&id)).await
    }

    async fn fetch_state(&self, key: FetchKey) -> Result<Option<FetchState>> {
        self.db(move |s| s.fetch_state(&key)).await
    }

    async fn begin(
        &self,
        subject: WorkRecord,
        direction: Direction,
        source: &'static str,
        refresh: bool,
    ) -> Result<FetchState> {
        self.db(move |s| s.begin_fetch(&subject, direction, source, refresh))
            .await
    }

    /// Commit a page; with `state == None` this begins a new generation
    /// first (a refresh that only starts once its first page arrived, so a
    /// failed refresh leaves the previous generation current).
    async fn commit(
        &self,
        call: &mut Call,
        subject: WorkRecord,
        direction: Direction,
        source: &'static str,
        state: Option<FetchState>,
        page: Page,
    ) -> Result<FetchState> {
        let out = self
            .db(move |s| {
                let state = match state {
                    Some(state) => state,
                    None => s.begin_fetch(&subject, direction, source, true)?,
                };
                let out = s.commit_page(&state, &page)?;
                if out.replay {
                    // Another writer advanced this generation: continue from
                    // its committed position.
                    let latest = s.begin_fetch(&subject, direction, source, false)?;
                    return Ok((latest, out.identity_conflicts));
                }
                Ok((out.state, out.identity_conflicts))
            })
            .await?;
        call.conflicts += out.1;
        Ok(out.0)
    }

    /// Best id to look `subject` up by in a source: the source's own id,
    /// then DOI, arXiv, PMID.
    fn lookup_id(class: &[ExtId], subject: &ExtId, native: Scheme) -> Option<ExtId> {
        let usable = |i: &ExtId| {
            i.scheme == native || matches!(i.scheme, Scheme::Doi | Scheme::Arxiv | Scheme::Pmid)
        };
        [native, Scheme::Doi, Scheme::Arxiv, Scheme::Pmid]
            .into_iter()
            .find_map(|scheme| class.iter().find(|i| i.scheme == scheme).cloned())
            .or_else(|| usable(subject).then(|| subject.clone()))
    }

    /// Fill from OpenAlex. True when OpenAlex data for the direction is
    /// cached or was (at least partly) fetched.
    async fn fill_openalex(
        &self,
        call: &mut Call,
        subject: &ExtId,
        direction: Direction,
        need: u64,
        refresh: bool,
    ) -> Result<bool> {
        let class = self.class_ids(subject).await?;
        let mut work_id = class.iter().find(|i| i.scheme == Scheme::Openalex).cloned();
        if let Some(w) = &work_id
            && !refresh
            && let Some(state) = self
                .fetch_state(FetchKey {
                    subject: w.clone(),
                    direction,
                    source: OA.into(),
                })
                .await?
            && (!state.resumable() || (direction == Direction::CitedBy && state.fetched >= need))
        {
            call.ok(OA);
            return Ok(true);
        }

        if direction != Direction::CitedBy || work_id.is_none() {
            let Some(lookup) = Self::lookup_id(&class, subject, Scheme::Openalex) else {
                call.skip(OA, "no_openalex_identity");
                return Ok(false);
            };
            if !self.gate(call, OA).await {
                return Ok(false);
            }
            let work = match self.inner.openalex.work(&lookup).await {
                Ok(work) => work,
                Err(e) => {
                    call.fail(OA, e.reason());
                    return Ok(false);
                }
            };
            call.ok(OA);
            work_id = Some(work.record.primary.clone());
            if direction != Direction::CitedBy {
                let ids = if direction == Direction::Refs {
                    work.referenced
                } else {
                    work.related
                };
                let page = Page {
                    total_reported: Some(ids.len() as u64),
                    rows: ids
                        .into_iter()
                        .map(|id| PageRow::new(WorkRecord::new(id)))
                        .collect(),
                    exhausted: true,
                    ..Page::default()
                };
                let state = if refresh {
                    None
                } else {
                    Some(
                        self.begin(work.record.clone(), direction, OA, false)
                            .await?,
                    )
                };
                self.commit(call, work.record, direction, OA, state, page)
                    .await?;
                return Ok(true);
            }
            let record = work.record;
            self.db(move |s| s.observe(&[record], OA)).await?;
        }
        let Some(work_id) = work_id else {
            return Ok(false);
        };

        // Cited-by: page through `cites:` until the requested window is
        // cached, the list ends, or the budget runs out.
        let subject_rec = WorkRecord::new(work_id.clone());
        let mut state = if refresh {
            None
        } else {
            Some(
                self.begin(subject_rec.clone(), direction, OA, false)
                    .await?,
            )
        };
        let mut restarted = false;
        loop {
            if let Some(s) = &state
                && (!s.resumable() || s.fetched >= need)
            {
                break;
            }
            if !self.gate(call, OA).await {
                break;
            }
            let cursor = state.as_ref().and_then(|s| s.cursor.clone());
            let fetched = state.as_ref().map_or(0, |s| s.fetched);
            let per_page = need
                .saturating_sub(fetched)
                .clamp(25, u64::from(openalex::MAX_PER_PAGE)) as u32;
            match self
                .inner
                .openalex
                .cites_page(&work_id, cursor.as_deref(), per_page)
                .await
            {
                Ok(p) => {
                    call.ok(OA);
                    let exhausted = p.next_cursor.is_none() || p.records.is_empty();
                    let page = Page {
                        rows: p.records.into_iter().map(PageRow::new).collect(),
                        next_cursor: p.next_cursor,
                        total_reported: p.total,
                        exhausted,
                        ..Page::default()
                    };
                    state = Some(
                        self.commit(call, subject_rec.clone(), direction, OA, state, page)
                            .await?,
                    );
                }
                // A stored cursor can expire; restart the list in a new
                // generation (upserts make the overlap harmless).
                Err(UpstreamError::Status(400)) if cursor.is_some() && !restarted => {
                    restarted = true;
                    state = None;
                    call.note("openalex_cursor_restarted");
                }
                Err(e) => {
                    call.fail(OA, e.reason());
                    return Ok(state.is_some_and(|s| s.pages > 0));
                }
            }
        }
        Ok(true)
    }

    /// Fill from Semantic Scholar (fallback, and contexts/intents source).
    async fn fill_semantic_scholar(
        &self,
        call: &mut Call,
        subject: &ExtId,
        direction: Direction,
        need: u64,
        refresh: bool,
    ) -> Result<bool> {
        let Some(s2) = self.inner.semantic_scholar.clone() else {
            call.skip(S2, "missing_api_key");
            return Ok(false);
        };
        let class = self.class_ids(subject).await?;
        let paper = match class.iter().find(|i| i.scheme == Scheme::SemanticScholar) {
            Some(p) => p.clone(),
            None => {
                let Some(lookup) = Self::lookup_id(&class, subject, Scheme::SemanticScholar) else {
                    call.skip(S2, "no_semantic_scholar_identity");
                    return Ok(false);
                };
                if !self.gate(call, S2).await {
                    return Ok(false);
                }
                let record = match s2.paper(&lookup).await {
                    Ok(r) => r,
                    Err(e) => {
                        call.fail(S2, e.reason());
                        return Ok(false);
                    }
                };
                let primary = record.primary.clone();
                self.db(move |s| s.observe(&[record], S2)).await?;
                primary
            }
        };
        let subject_rec = WorkRecord::new(paper.clone());
        let mut state = if refresh {
            None
        } else {
            Some(
                self.begin(subject_rec.clone(), direction, S2, false)
                    .await?,
            )
        };
        loop {
            if let Some(s) = &state
                && (!s.resumable() || s.fetched >= need)
            {
                break;
            }
            let offset: u32 = state
                .as_ref()
                .and_then(|s| s.cursor.as_deref())
                .and_then(|c| c.parse().ok())
                .unwrap_or(0);
            let fetched = state.as_ref().map_or(0, |s| s.fetched);
            if offset >= semantic_scholar::OFFSET_CAP {
                let page = Page {
                    hard_cap: true,
                    ..Page::default()
                };
                self.commit(call, subject_rec.clone(), direction, S2, state, page)
                    .await?;
                call.note("semantic_scholar_offset_cap");
                break;
            }
            if !self.gate(call, S2).await {
                break;
            }
            let limit = (need.saturating_sub(fetched).max(20) as u32)
                .min(semantic_scholar::MAX_LIMIT)
                .min(semantic_scholar::OFFSET_CAP - offset);
            match s2.edges(&paper, direction, offset, limit).await {
                Ok(p) => {
                    call.ok(S2);
                    let hard_cap = p.next.is_some_and(|n| n >= semantic_scholar::OFFSET_CAP);
                    if hard_cap {
                        call.note("semantic_scholar_offset_cap");
                    }
                    let page = Page {
                        rows: p.rows,
                        unresolved: p.unresolved,
                        next_cursor: p.next.map(|n| n.to_string()),
                        total_reported: None,
                        exhausted: p.next.is_none(),
                        hard_cap,
                    };
                    state = Some(
                        self.commit(call, subject_rec.clone(), direction, S2, state, page)
                            .await?,
                    );
                }
                Err(e) => {
                    call.fail(S2, e.reason());
                    return Ok(state.is_some_and(|s| s.pages > 0));
                }
            }
        }
        Ok(true)
    }

    /// Fetch OpenAlex metadata for listed neighbors that have none.
    async fn hydrate(&self, call: &mut Call, page: &NeighborPage) -> Result<bool> {
        let missing: Vec<ExtId> = page
            .items
            .iter()
            .filter(|n| !n.has_meta)
            .filter_map(|n| n.ids.iter().find(|i| i.scheme == Scheme::Openalex).cloned())
            .collect();
        let mut changed = false;
        for batch in missing.chunks(openalex::HYDRATE_BATCH) {
            if !self.gate(call, OA).await {
                call.note("metadata_incomplete");
                break;
            }
            match self.inner.openalex.hydrate(batch).await {
                Ok(records) => {
                    if !records.is_empty() {
                        self.db(move |s| s.observe(&records, OA)).await?;
                        changed = true;
                    }
                }
                Err(e) => {
                    call.fail(OA, e.reason());
                    call.note("metadata_incomplete");
                    break;
                }
            }
        }
        Ok(changed)
    }
}

/// Percent-encode an id for a URL path, keeping the `:` and `/` that
/// OpenAlex and Semantic Scholar expect literally in `doi:10.x/y`.
pub(crate) fn path_segment(key: &str) -> String {
    urlencoding::encode(key)
        .replace("%3A", ":")
        .replace("%2F", "/")
}

fn store_error(path: &Path, e: citegraph::Error) -> ZoteroMcpError {
    ZoteroMcpError::Config(format!("citation store {}: {e}", path.display()))
}

/// Parse a `hit_id` (`doi:`, `arxiv:`, `pmid:`, `url:` of an OpenAlex or
/// Semantic Scholar record) or a bare DOI / arXiv id / DOI URL.
pub fn parse_citation_id(raw: &str) -> Result<ExtId> {
    let raw = raw.trim();
    let unsupported = |what: &str| {
        ZoteroMcpError::InvalidInput(format!(
            "citation lookup needs a DOI, arXiv, PMID, OpenAlex or Semantic Scholar identity; {what}. Use a doi:/arxiv: hit_id from papers search or open_paper metadata."
        ))
    };
    if raw.is_empty() {
        return Err(unsupported("the id is empty"));
    }
    let parsed = if let Some(rest) = raw.strip_prefix("doi:") {
        ExtId::doi(rest)
    } else if let Some(rest) = raw.strip_prefix("arxiv:") {
        ExtId::arxiv(rest)
    } else if let Some(rest) = raw.strip_prefix("pmid:") {
        ExtId::pmid(rest)
    } else if let Some(rest) = raw.strip_prefix("url:") {
        parse_url_id(rest)
    } else if let Some((prefix, _)) = raw.split_once(':')
        && ["zotero", "paperseed", "research", "isbn", "title"].contains(&prefix)
    {
        return Err(unsupported(&format!("'{prefix}:' hit_ids carry none")));
    } else {
        parse_url_id(raw)
            .or_else(|| ExtId::doi(raw))
            .or_else(|| ExtId::arxiv(raw))
            .or_else(|| ExtId::openalex(raw).filter(|_| !raw.contains('/')))
    };
    parsed.ok_or_else(|| unsupported(&format!("'{raw}' is not one")))
}

fn parse_url_id(raw: &str) -> Option<ExtId> {
    let url = url::Url::parse(raw).ok()?;
    let host = url.host_str()?.trim_start_matches("www.");
    let last = url.path_segments()?.rfind(|s| !s.is_empty())?.to_string();
    match host {
        "openalex.org" | "api.openalex.org" => ExtId::openalex(&last),
        "semanticscholar.org" | "api.semanticscholar.org" => ExtId::semantic_scholar(&last),
        "doi.org" | "dx.doi.org" => ExtId::doi(raw),
        "arxiv.org" | "export.arxiv.org" => ExtId::arxiv(raw),
        _ => None,
    }
}

fn source_of(name: &str) -> PaperSource {
    if name == S2 {
        PaperSource::SemanticScholar
    } else {
        PaperSource::OpenAlex
    }
}

/// Mint a compact hit from a work's ids and metadata with the existing
/// `hit_id` rules (arXiv, then DOI, then a reversible record URL).
fn mint_hit(ids: &[ExtId], meta: &WorkMeta, source: PaperSource) -> PaperHit {
    let pick = |scheme: Scheme| {
        ids.iter()
            .find(|i| i.scheme == scheme)
            .map(|i| i.value.clone())
    };
    let url = pick(Scheme::Openalex)
        .map(|w| format!("https://openalex.org/{w}"))
        .or_else(|| {
            pick(Scheme::SemanticScholar)
                .map(|p| format!("https://www.semanticscholar.org/paper/{p}"))
        });
    let mut hit = PaperHit::new(
        source,
        meta.title.clone().unwrap_or_default(),
        meta.authors.clone(),
        meta.year.map(|y| y.to_string()),
        pick(Scheme::Doi),
        pick(Scheme::Arxiv),
        pick(Scheme::Pmid),
        None,
        url,
        None,
        None,
        meta.venue.clone(),
        None,
    );
    enrich_hit_identity(&mut hit);
    apply_detail(&mut hit, SearchDetail::Compact, None);
    if pick(Scheme::Doi).is_none() && pick(Scheme::Arxiv).is_none() {
        // A record URL (OpenAlex / Semantic Scholar page) is not a paper.
        hit.next.retain(|n| n != "open_paper");
    }
    hit.next.push("get_citations".into());
    // Compact wire projection, as `SearchPapersResult::agent_output`.
    hit.doi = None;
    hit.arxiv_id = None;
    hit.pmid = None;
    hit.url = None;
    hit.venue = None;
    hit
}

fn bounded(text: &str) -> (String, bool) {
    if text.chars().count() <= CONTEXT_MAX_CHARS {
        return (text.to_string(), false);
    }
    let mut out: String = text.chars().take(CONTEXT_MAX_CHARS).collect();
    out.push('…');
    (out, true)
}

#[allow(clippy::too_many_arguments)]
fn build_result(
    subject: &ExtId,
    subject_ids: &[ExtId],
    direction: CitationDirection,
    offset: u32,
    limit: u32,
    page: NeighborPage,
    coverage: Vec<FetchState>,
    mut diagnostics: CitationDiagnostics,
) -> CitationsResult {
    let subject_ids = if subject_ids.is_empty() {
        std::slice::from_ref(subject)
    } else {
        subject_ids
    };
    let subject_hit = mint_hit(subject_ids, &WorkMeta::default(), PaperSource::OpenAlex)
        .hit_id
        .unwrap_or_else(|| subject.key());
    let complete_by_source: Vec<(String, bool)> = coverage
        .iter()
        .map(|c| (c.key.source.clone(), c.complete))
        .collect();
    let all_complete = !coverage.is_empty() && coverage.iter().all(|c| c.complete);
    let more_upstream = coverage.iter().any(|c| {
        c.resumable() && c.cursor.is_some() && c.total_reported.is_none_or(|t| c.fetched < t)
    });
    let why_edge = match direction {
        CitationDirection::Refs => "cited by subject".to_string(),
        CitationDirection::CitedBy => "cites subject".to_string(),
        CitationDirection::Related => "related to subject".to_string(),
    };

    let mut trimmed = false;
    let mut hits: Vec<CitationHit> = page
        .items
        .into_iter()
        .map(|n| {
            let first_source = n.sources.first().map_or(OA, |s| s.source.as_str());
            let hit = mint_hit(&n.ids, &n.meta, source_of(first_source));
            let mut why = vec![why_edge.clone()];
            if n.sources.iter().any(|s| s.is_influential == Some(true)) {
                why.push("influential (semantic_scholar)".into());
            }
            if n.sources.len() > 1 {
                why.push(format!("{} sources", n.sources.len()));
            }
            let truncated = n.sources.iter().any(|s| {
                complete_by_source
                    .iter()
                    .any(|(src, complete)| src == &s.source && !complete)
            });
            let versions = if n.versions.is_empty() {
                Vec::new()
            } else {
                mint_hit(&n.versions, &WorkMeta::default(), PaperSource::OpenAlex)
                    .hit_id
                    .into_iter()
                    .collect()
            };
            let mut contexts = Vec::new();
            for (source, text) in n.contexts.into_iter() {
                if contexts.len() == CONTEXTS_PER_HIT {
                    trimmed = true;
                    break;
                }
                let (text, cut) = bounded(&text);
                trimmed |= cut;
                contexts.push(CitationContext { source, text });
            }
            CitationHit {
                hit,
                why,
                edge_sources: n
                    .sources
                    .into_iter()
                    .map(|s| EdgeSourceInfo {
                        source: s.source,
                        intents: s.intents,
                        is_influential: s.is_influential,
                    })
                    .collect(),
                truncated,
                versions,
                contexts,
            }
        })
        .collect();
    // Keep the whole result inside the MCP budget: drop contexts from the
    // last hits first.
    let size = |hits: &[CitationHit]| {
        serde_json::to_string(hits)
            .and_then(|json| Ok(json.len() + serde_json::to_string(&json)?.len()))
            .unwrap_or(usize::MAX)
    };
    let mut index = hits.len();
    while size(&hits) > RESULT_SOFT_BYTES && index > 0 {
        index -= 1;
        if !hits[index].contexts.is_empty() {
            hits[index].contexts.clear();
            trimmed = true;
        }
    }
    if trimmed {
        diagnostics.notes.push("contexts_trimmed".into());
    }

    let total = page.total;
    let shown_end = offset as usize + hits.len();
    // Merges and self-loops can shorten a page, so upstream resumability,
    // not page fullness, decides whether more may exist.
    let has_more = shown_end < total || more_upstream;
    CitationsResult {
        hit_id: subject_hit,
        direction,
        total_count: all_complete.then(|| u32::try_from(total).unwrap_or(u32::MAX)),
        count_kind: if all_complete {
            CountKind::Exact
        } else {
            CountKind::Unknown
        },
        offset,
        limit,
        has_more,
        next_offset: has_more.then_some(offset + limit),
        truncated: !all_complete,
        hits,
        coverage: coverage
            .into_iter()
            .map(|c| CoverageInfo {
                source: c.key.source,
                generation: c.generation,
                fetched: c.fetched,
                total_reported: c.total_reported,
                complete: c.complete,
                capped: c.truncated,
                started_at: c.started_at,
                finished_at: c.finished_at,
            })
            .collect(),
        diagnostics,
    }
}

/// A heavy result: 50 hits with long-ish metadata, two edge sources and
/// escape-heavy contexts. Pathological metadata alone can still exceed the
/// budget; MCP then returns its bounded `response_too_large` recovery, as
/// for search. Contexts must never be what pushes a result over.
#[cfg(test)]
pub(crate) fn worst_case_result() -> CitationsResult {
    let nasty = "\"\\\u{1}<>&".repeat(300);
    let neighbor = |n: u32| citegraph::Neighbor {
        ids: vec![
            ExtId::doi(&format!("10.1234/journal.{n:06}")).unwrap(),
            ExtId::openalex(&format!("W{n}")).unwrap(),
        ],
        meta: WorkMeta {
            title: Some("T".repeat(150)),
            year: Some(2020),
            venue: Some("v".repeat(70)),
            authors: (0..10).map(|_| "A".repeat(30)).collect(),
        },
        has_meta: true,
        sources: ["openalex", "semantic_scholar"]
            .into_iter()
            .map(|s| citegraph::EdgeSource {
                source: s.into(),
                intents: vec!["background".into(), "methodology".into(), "result".into()],
                is_influential: Some(true),
                stale: false,
                first_seen: String::new(),
                last_seen: String::new(),
            })
            .collect(),
        contexts: (0..10)
            .map(|i| ("semantic_scholar".into(), format!("{i}{nasty}")))
            .collect(),
        versions: vec![ExtId::arxiv("1706.03762").unwrap()],
    };
    build_result(
        &ExtId::doi("10.1/seed").unwrap(),
        &[],
        CitationDirection::CitedBy,
        0,
        MAX_LIMIT,
        NeighborPage {
            total: 50,
            items: (0..MAX_LIMIT).map(neighbor).collect(),
        },
        Vec::new(),
        CitationDiagnostics::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hit_ids_and_bare_ids() {
        let cases = [
            ("doi:10.1234/ABC", Scheme::Doi),
            ("10.1234/abc", Scheme::Doi),
            ("https://doi.org/10.1234/abc", Scheme::Doi),
            ("arxiv:1706.03762", Scheme::Arxiv),
            ("doi:10.48550/arXiv.1706.03762", Scheme::Arxiv),
            ("1706.03762v5", Scheme::Arxiv),
            ("pmid:123", Scheme::Pmid),
            ("url:https://openalex.org/W2741809807", Scheme::Openalex),
            ("W2741809807", Scheme::Openalex),
            (
                "url:https://www.semanticscholar.org/paper/204e3073870fae3d05bcbc2f6a8e263d9b72e776",
                Scheme::SemanticScholar,
            ),
        ];
        for (raw, scheme) in cases {
            assert_eq!(parse_citation_id(raw).unwrap().scheme, scheme, "{raw}");
        }
        for bad in [
            "zotero:ABCD1234",
            "isbn:9780131103627",
            "",
            "url:https://example.org/x",
            "hello",
        ] {
            assert!(
                matches!(parse_citation_id(bad), Err(ZoteroMcpError::InvalidInput(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn minted_ids_follow_hit_id_rules() {
        let ids = vec![
            ExtId::doi("10.1/x").unwrap(),
            ExtId::openalex("W1").unwrap(),
        ];
        let hit = mint_hit(&ids, &WorkMeta::default(), PaperSource::OpenAlex);
        assert_eq!(hit.hit_id.as_deref(), Some("doi:10.1/x"));
        let hit = mint_hit(&ids[1..], &WorkMeta::default(), PaperSource::OpenAlex);
        assert_eq!(hit.hit_id.as_deref(), Some("url:https://openalex.org/W1"));
        let back = parse_citation_id(hit.hit_id.as_deref().unwrap()).unwrap();
        assert_eq!(back, ids[1]);
        assert!(hit.next.contains(&"get_citations".to_string()));
    }

    #[test]
    fn contexts_are_bounded_to_fit_the_mcp_budget() {
        let neighbor = |n: u32| citegraph::Neighbor {
            ids: vec![ExtId::doi(&format!("10.1/{n}")).unwrap()],
            meta: WorkMeta {
                title: Some("t".repeat(200)),
                ..WorkMeta::default()
            },
            has_meta: true,
            sources: Vec::new(),
            contexts: (0..10)
                .map(|i| {
                    (
                        "semantic_scholar".to_string(),
                        format!("{i}{}", "x".repeat(2000)),
                    )
                })
                .collect(),
            versions: Vec::new(),
        };
        let page = NeighborPage {
            total: 50,
            items: (0..50).map(neighbor).collect(),
        };
        let subject = ExtId::doi("10.1/seed").unwrap();
        let out = build_result(
            &subject,
            &[],
            CitationDirection::Refs,
            0,
            50,
            page,
            Vec::new(),
            CitationDiagnostics::default(),
        );
        let bytes = serde_json::to_vec(&out).unwrap().len();
        assert!(bytes * 2 < 60_000, "{bytes}");
        assert!(out.hits[0].contexts.len() <= CONTEXTS_PER_HIT);
        assert!(
            out.diagnostics
                .notes
                .contains(&"contexts_trimmed".to_string())
        );
    }

    #[test]
    fn store_path_is_under_paperbridge_data_dir() {
        let path = default_store_path();
        assert!(path.ends_with("paperbridge/citegraph.sqlite"));
    }
}
