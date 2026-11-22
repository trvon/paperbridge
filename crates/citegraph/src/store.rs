//! The SQLite store: identity resolution, merges, edges and the fetch log.

use crate::ids::{ExtId, Scheme, StrongIds};
use crate::time::now_rfc3339;
use crate::uf::UnionFind;
use crate::{Error, Result, migrate};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Which neighbor list a fetch fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Works the subject cites.
    Refs,
    /// Works citing the subject.
    CitedBy,
    /// Works a source considers related (not a citation).
    Related,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Refs => "refs",
            Self::CitedBy => "cited_by",
            Self::Related => "related",
        }
    }
}

/// Descriptive metadata for a work; every field is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkMeta {
    pub title: Option<String>,
    pub year: Option<i32>,
    pub venue: Option<String>,
    pub authors: Vec<String>,
}

/// One upstream record: the ids it reports for a single work, plus metadata.
///
/// `primary` is the source-native id (an OpenAlex `W…`, a Semantic Scholar
/// `paperId`) and decides which work the record's edges attach to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRecord {
    pub primary: ExtId,
    pub ids: Vec<ExtId>,
    pub meta: Option<WorkMeta>,
}

impl WorkRecord {
    pub fn new(primary: ExtId) -> Self {
        Self {
            ids: vec![primary.clone()],
            primary,
            meta: None,
        }
    }

    pub fn with_id(mut self, id: Option<ExtId>) -> Self {
        if let Some(id) = id
            && !self.ids.contains(&id)
        {
            self.ids.push(id);
        }
        self
    }

    pub fn with_meta(mut self, meta: WorkMeta) -> Self {
        self.meta = Some(meta);
        self
    }
}

/// One neighbor in a fetched page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRow {
    pub work: WorkRecord,
    pub intents: Vec<String>,
    pub is_influential: Option<bool>,
    pub contexts: Vec<String>,
}

impl PageRow {
    pub fn new(work: WorkRecord) -> Self {
        Self {
            work,
            intents: Vec::new(),
            is_influential: None,
            contexts: Vec::new(),
        }
    }
}

/// One fetched page, committed atomically with its cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    pub rows: Vec<PageRow>,
    /// Raw JSON of neighbors that carried no usable id.
    pub unresolved: Vec<String>,
    /// Cursor for the next page; `None` when the source has no more.
    pub next_cursor: Option<String>,
    pub total_reported: Option<u64>,
    /// The source has no further pages: the generation is complete.
    pub exhausted: bool,
    /// The source cannot page further although more exist (for example
    /// Semantic Scholar's offset + limit cap). Never marks rows stale.
    pub hard_cap: bool,
}

/// A fetch log key. `subject` is the id the fetch was made with, normally
/// the source-native id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchKey {
    pub subject: ExtId,
    pub direction: Direction,
    pub source: String,
}

/// One `fetch_log` generation as last committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchState {
    pub key: FetchKey,
    pub generation: i64,
    pub cursor: Option<String>,
    pub pages: i64,
    pub fetched: u64,
    pub total_reported: Option<u64>,
    pub complete: bool,
    pub truncated: bool,
    pub started_at: String,
    /// Store sequence number when the generation began; orders generations
    /// across processes and stamps the rows it sees.
    pub started_seq: i64,
    pub finished_at: Option<String>,
}

impl FetchState {
    /// More pages can be fetched in this generation.
    pub fn resumable(&self) -> bool {
        !self.complete && !self.truncated
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitOutcome {
    pub state: FetchState,
    /// The page had already been committed (stale cursor read or retry);
    /// edges were upserted but the cursor did not move.
    pub replay: bool,
    /// The generation is finished, or a newer generation of the same fetch
    /// already completed: the commit wrote nothing.
    pub superseded: bool,
    /// Merges vetoed by the strong-id, version or undo rules.
    pub identity_conflicts: u32,
    /// Version splits not applied because the merge was not the outermost
    /// of its class (rule 12).
    pub pending_splits: u32,
}

/// Result of [`Store::observe_report`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObserveOutcome {
    pub conflicts: u32,
    /// Version splits not applied because the merge was not the outermost
    /// of its class (rule 12); reported, never forced.
    pub pending_splits: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct NeighborQuery {
    pub offset: usize,
    pub limit: usize,
    pub include_stale: bool,
    pub include_contexts: bool,
}

/// Per-source evidence for one neighbor edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EdgeSource {
    pub source: String,
    pub intents: Vec<String>,
    pub is_influential: Option<bool>,
    pub stale: bool,
    pub first_seen: String,
    pub last_seen: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neighbor {
    /// Every external id of the neighbor's resolved class, sorted.
    pub ids: Vec<ExtId>,
    pub meta: WorkMeta,
    /// Some member of the class has fetched metadata.
    pub has_meta: bool,
    pub sources: Vec<EdgeSource>,
    /// `(source, text)` citation contexts, when requested.
    pub contexts: Vec<(String, String)>,
    /// Ids of other versions (preprint/published) of the neighbor.
    pub versions: Vec<ExtId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeighborPage {
    /// Distinct resolved neighbors after stale filtering.
    pub total: usize,
    pub items: Vec<Neighbor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MergeInfo {
    pub id: i64,
    pub reason: String,
    pub evidence: Option<String>,
    pub at: String,
    /// Store sequence number of the merge; review decisions (stage 3)
    /// compare against it to tell post-merge decisions apart.
    pub seq: i64,
    pub undone_at: Option<String>,
    pub undone_seq: Option<i64>,
    /// `user` or `version_split`.
    pub undo_reason: Option<String>,
}

/// Token bucket settings for one source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimit {
    pub per_second: f64,
    pub burst: u32,
    /// Requests allowed per UTC day across all processes.
    pub daily: u32,
}

/// Result of [`Store::take_request`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateGrant {
    Granted,
    /// No token yet; retry after this many milliseconds.
    Wait(u64),
    /// The source's daily budget is spent.
    DailyExhausted,
}

pub struct Store {
    conn: Connection,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("path", &self.conn.path())
            .finish()
    }
}

impl Store {
    /// Open (creating if needed) the store at `path`: WAL, 5 s busy
    /// timeout, schema migrated to the latest version.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        set_wal(&conn)?;
        Self::init(conn)
    }

    /// A private in-memory store (tests, dry runs). Not WAL.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        migrate::migrate(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn journal_mode(&self) -> Result<String> {
        Ok(self
            .conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))?)
    }

    fn write(&mut self) -> Result<Transaction<'_>> {
        Ok(self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }

    /// Record works (for example hydrated metadata) in one `BEGIN
    /// IMMEDIATE` transaction. Returns the number of vetoed merges.
    pub fn observe(&mut self, records: &[WorkRecord], source: &str) -> Result<u32> {
        Ok(self.observe_report(records, source)?.conflicts)
    }

    /// [`Store::observe`], also reporting version splits left pending.
    pub fn observe_report(
        &mut self,
        records: &[WorkRecord],
        source: &str,
    ) -> Result<ObserveOutcome> {
        let tx = self.write()?;
        let mut w = Writer::new(&tx)?;
        for record in records {
            w.observe(record, source)?;
        }
        let out = ObserveOutcome {
            conflicts: w.conflicts,
            pending_splits: w.pending_splits,
        };
        tx.commit()?;
        Ok(out)
    }

    /// Latest generation for `key`, if any.
    pub fn fetch_state(&mut self, key: &FetchKey) -> Result<Option<FetchState>> {
        let tx = self.conn.transaction()?;
        let Some(base) = base_of(&tx, &key.subject)? else {
            return Ok(None);
        };
        latest_generation(&tx, base, key)
    }

    /// Start or continue a fetch. Continues the latest generation unless
    /// `refresh` is set or there is none, in which case a new generation
    /// begins with an empty cursor. Commits before returning: no
    /// transaction is open while the caller awaits HTTP.
    pub fn begin_fetch(
        &mut self,
        subject: &WorkRecord,
        direction: Direction,
        source: &str,
        refresh: bool,
    ) -> Result<FetchState> {
        let key = FetchKey {
            subject: subject.primary.clone(),
            direction,
            source: source.to_string(),
        };
        let tx = self.write()?;
        let mut w = Writer::new(&tx)?;
        let base = w.observe(subject, source)?;
        let latest = latest_generation(&tx, base, &key)?;
        let state = match latest {
            Some(state) if !refresh => state,
            other => {
                let generation = other.map_or(1, |s| s.generation + 1);
                let seq = bump_seq(&tx)?;
                tx.execute(
                    "INSERT INTO fetch_log(work, direction, source, generation, started_at, started_seq)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![base, direction.as_str(), source, generation, now_rfc3339(), seq],
                )?;
                latest_generation(&tx, base, &key)?.ok_or_else(|| no_fetch(&key, generation))?
            }
        };
        tx.commit()?;
        Ok(state)
    }

    /// Commit one fetched page and its cursor atomically.
    ///
    /// This is `CommitPage` in `formal/tla/MergeFetchDecide.tla`: one
    /// `BEGIN IMMEDIATE` transaction that
    /// 1. re-resolves the subject and every page id through `work_ids`
    ///    *inside* the transaction (`INSERT … ON CONFLICT DO NOTHING`, then
    ///    read back), because another worker may have minted a work for an
    ///    id, or a merge or undo may have landed, while the page was in
    ///    flight;
    /// 2. stores edges by base work (the `work_ids` mapping, never the class
    ///    root), so undoing a merge cannot strand them;
    /// 3. advances the cursor only if `state` is still the committed
    ///    position (`pages` equals the snapshot; the spec's
    ///    `cursor' = max(cursor, page)`), so a replay never moves it back;
    /// 4. stamps seen rows with the generation's `started_seq`, never the
    ///    commit time, and writes nothing for a generation older than the
    ///    latest complete one (or to a finished generation), so late
    ///    commits cannot hide staleness.
    pub fn commit_page(&mut self, state: &FetchState, page: &Page) -> Result<CommitOutcome> {
        let key = &state.key;
        let tx = self.write()?;
        let mut w = Writer::new(&tx)?;
        let subject = w.observe(&WorkRecord::new(key.subject.clone()), &key.source)?;
        let current = generation(&tx, subject, key, state.generation)?
            .ok_or_else(|| no_fetch(key, state.generation))?;
        // A finished generation (complete or capped) takes no more pages, and
        // one older than the newest complete generation is superseded.
        let superseded = !current.resumable()
            || newest_complete(&tx, subject, key)?.is_some_and(|g| g > state.generation);
        if superseded {
            tx.commit()?;
            return Ok(CommitOutcome {
                state: current,
                replay: true,
                superseded: true,
                identity_conflicts: 0,
                pending_splits: 0,
            });
        }
        let replay = current.pages != state.pages;
        let stamp = current.started_seq;
        let now = now_rfc3339();

        for row in &page.rows {
            let other = w.observe(&row.work, &key.source)?;
            match key.direction {
                Direction::Refs => w.upsert_edge(subject, other, key, row, stamp, &now)?,
                Direction::CitedBy => w.upsert_edge(other, subject, key, row, stamp, &now)?,
                Direction::Related => {
                    tx.execute(
                        "INSERT INTO related_works(work, related, source, first_seen, last_seen,
                                                   first_seen_seq, last_seen_seq)
                         VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?5)
                         ON CONFLICT(work, related, source) DO UPDATE SET
                           last_seen = excluded.last_seen,
                           last_seen_seq = MAX(last_seen_seq, excluded.last_seen_seq)",
                        params![subject, other, key.source, now, stamp],
                    )?;
                }
            }
        }
        for raw in &page.unresolved {
            tx.execute(
                "INSERT OR IGNORE INTO unresolved_refs(work, direction, source, raw_hash, raw, seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    subject,
                    key.direction.as_str(),
                    key.source,
                    fnv_hex(raw),
                    raw,
                    now
                ],
            )?;
        }

        if !replay {
            let fetched = (page.rows.len() + page.unresolved.len()) as i64;
            let complete = page.exhausted && !page.hard_cap;
            let finished = complete || page.hard_cap;
            tx.execute(
                "UPDATE fetch_log SET
                   cursor = ?5,
                   pages = pages + 1,
                   fetched = fetched + ?6,
                   total_reported = COALESCE(?7, total_reported),
                   complete = ?8,
                   truncated = ?9,
                   finished_at = CASE WHEN ?10 THEN ?11 ELSE finished_at END
                 WHERE work = ?1 AND direction = ?2 AND source = ?3 AND generation = ?4",
                params![
                    subject,
                    key.direction.as_str(),
                    key.source,
                    state.generation,
                    if finished {
                        None
                    } else {
                        page.next_cursor.clone()
                    },
                    fetched,
                    page.total_reported.map(|t| t as i64),
                    complete,
                    page.hard_cap,
                    finished,
                    now,
                ],
            )?;
        }
        let state = generation(&tx, subject, key, state.generation)?
            .ok_or_else(|| no_fetch(key, state.generation))?;
        let (identity_conflicts, pending_splits) = (w.conflicts, w.pending_splits);
        tx.commit()?;
        Ok(CommitOutcome {
            state,
            replay,
            superseded: false,
            identity_conflicts,
            pending_splits,
        })
    }

    /// All external ids of `id`'s resolved class (empty if unknown).
    pub fn class_ids(&mut self, id: &ExtId) -> Result<Vec<ExtId>> {
        let tx = self.conn.transaction()?;
        let uf = UnionFind::load(&tx)?;
        let Some(base) = base_of(&tx, id)? else {
            return Ok(Vec::new());
        };
        class_ids(&tx, &uf, uf.find(base))
    }

    /// External ids of `id`'s class, followed by those of its other
    /// versions (preprint / published).
    pub fn linked_ids(&mut self, id: &ExtId) -> Result<Vec<ExtId>> {
        let tx = self.conn.transaction()?;
        let uf = UnionFind::load(&tx)?;
        let Some(base) = base_of(&tx, id)? else {
            return Ok(Vec::new());
        };
        let root = uf.find(base);
        let mut out = class_ids(&tx, &uf, root)?;
        out.extend(version_ids(&tx, &uf, root)?);
        Ok(out)
    }

    /// Latest fetch generation per source for `id`'s class (and its
    /// versions) and direction, latest by `started_seq`.
    pub fn coverage(&mut self, id: &ExtId, direction: Direction) -> Result<Vec<FetchState>> {
        let tx = self.conn.transaction()?;
        let uf = UnionFind::load(&tx)?;
        let Some(base) = base_of(&tx, id)? else {
            return Ok(Vec::new());
        };
        let (_, members) = subject_scope(&tx, &uf, uf.find(base))?;
        let mut latest: BTreeMap<String, FetchState> = BTreeMap::new();
        for &work in &members {
            let found = tx
                .prepare_cached(
                    "SELECT source, MAX(generation) FROM fetch_log WHERE work = ?1 AND direction = ?2
                     GROUP BY source",
                )?
                .query_map(params![work, direction.as_str()], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (source, generation_no) in found {
                let key = FetchKey {
                    subject: id.clone(),
                    direction,
                    source: source.clone(),
                };
                if let Some(state) = generation(&tx, work, &key, generation_no)? {
                    // Several bases of one class can hold fetches for a
                    // source; report the most recently started.
                    let newer = latest
                        .get(&source)
                        .is_none_or(|cur| state.started_seq > cur.started_seq);
                    if newer {
                        latest.insert(source, state);
                    }
                }
            }
        }
        Ok(latest.into_values().collect())
    }

    /// Distinct resolved, non-stale neighbors of `id` in `direction`.
    pub fn neighbor_total(&mut self, id: &ExtId, direction: Direction) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let uf = UnionFind::load(&tx)?;
        let Some(base) = base_of(&tx, id)? else {
            return Ok(0);
        };
        let (roots, members) = subject_scope(&tx, &uf, uf.find(base))?;
        Ok(neighbor_order(&tx, &uf, &roots, &members, direction, false)?.len())
    }

    /// Resolved neighbors of `id` in `direction`, deduplicated across
    /// sources, merges and versions, with resolved self-loops dropped and
    /// stale rows excluded unless asked. Ordered by first insertion.
    ///
    /// The ordering pass reads only `(other, MIN(rowid))` aggregated in SQL;
    /// full rows, metadata and contexts are read for the requested page only.
    pub fn neighbors(
        &mut self,
        id: &ExtId,
        direction: Direction,
        query: NeighborQuery,
    ) -> Result<NeighborPage> {
        let tx = self.conn.transaction()?;
        let uf = UnionFind::load(&tx)?;
        let Some(base) = base_of(&tx, id)? else {
            return Ok(NeighborPage {
                total: 0,
                items: Vec::new(),
            });
        };
        let (roots, members) = subject_scope(&tx, &uf, uf.find(base))?;
        let order = neighbor_order(&tx, &uf, &roots, &members, direction, query.include_stale)?;
        let total = order.len();
        let mut items = Vec::new();
        for other_root in order.into_iter().skip(query.offset).take(query.limit) {
            let others = uf.members(other_root);
            let rows = edge_rows_between(&tx, &members, &others, direction)?;
            let mut sources: BTreeMap<String, EdgeSource> = BTreeMap::new();
            let mut pairs = Vec::new();
            for row in rows {
                if !query.include_stale && row.stale {
                    continue;
                }
                pairs.push((row.citing, row.cited, row.source.clone()));
                let entry = sources
                    .entry(row.source.clone())
                    .or_insert_with(|| EdgeSource {
                        source: row.source.clone(),
                        intents: Vec::new(),
                        is_influential: None,
                        stale: true,
                        first_seen: row.first_seen.clone(),
                        last_seen: row.last_seen.clone(),
                    });
                entry.stale &= row.stale;
                for intent in row.intents {
                    if !entry.intents.contains(&intent) {
                        entry.intents.push(intent);
                    }
                }
                entry.is_influential = match (entry.is_influential, row.is_influential) {
                    (Some(a), Some(b)) => Some(a || b),
                    (a, b) => a.or(b),
                };
                entry.first_seen = entry.first_seen.clone().min(row.first_seen);
                entry.last_seen = entry.last_seen.clone().max(row.last_seen);
            }
            let mut contexts = Vec::new();
            if query.include_contexts && direction != Direction::Related {
                let mut stmt = tx.prepare_cached(
                    "SELECT source, text FROM contexts WHERE citing = ?1 AND cited = ?2 AND source = ?3
                     ORDER BY first_seen, text_hash",
                )?;
                for (citing, cited, source) in &pairs {
                    for row in stmt.query_map(params![citing, cited, source], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })? {
                        let row = row?;
                        if !contexts.contains(&row) {
                            contexts.push(row);
                        }
                    }
                }
            }
            let (meta, has_meta) = class_meta(&tx, &uf, other_root)?;
            items.push(Neighbor {
                ids: class_ids(&tx, &uf, other_root)?,
                meta,
                has_meta,
                sources: sources.into_values().collect(),
                contexts,
                versions: version_ids(&tx, &uf, other_root)?,
            });
        }
        Ok(NeighborPage { total, items })
    }

    /// Active and undone merges, oldest first.
    pub fn merges(&mut self) -> Result<Vec<MergeInfo>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, reason, evidence, at, seq, undone_at, undone_seq, undo_reason
             FROM merge_log ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(MergeInfo {
                id: r.get(0)?,
                reason: r.get(1)?,
                evidence: r.get(2)?,
                at: r.get(3)?,
                seq: r.get(4)?,
                undone_at: r.get(5)?,
                undone_seq: r.get(6)?,
                undo_reason: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Undo an active merge (sets `undone_at`) in one `BEGIN IMMEDIATE`
    /// transaction. Returns false if the merge does not exist or is already
    /// undone.
    ///
    /// Only the outermost merge of a class may be undone: the latest active
    /// merge (by `seq`) among those joining works now in its class. Its
    /// winner is necessarily the current class root (design rule 9), and
    /// undoing in this order restores exactly the classes that existed
    /// before it, so review state logged per merge can be restored without a
    /// spurious `needs_decision`. An inner merge fails with
    /// [`Error::UndoBlocked`] naming the merge to undo first.
    ///
    /// Classes are recomputed from the merges that remain active. Undo is
    /// sticky for this evidence pair: observing the same two works together
    /// again does not redo it; [`Store::redo_merge`] does.
    pub fn undo_merge(&mut self, merge_id: i64) -> Result<bool> {
        let tx = self.write()?;
        let row: Option<(i64, i64)> = tx
            .query_row(
                "SELECT base_a, winner FROM merge_log WHERE id = ?1 AND undone_at IS NULL",
                [merge_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((base_a, winner)) = row else {
            return Ok(false);
        };
        let uf = UnionFind::load(&tx)?;
        if let Some(blocking) = outermost_in_class(&tx, &uf, base_a)?.filter(|&id| id != merge_id) {
            return Err(Error::UndoBlocked {
                merge: merge_id,
                blocking,
            });
        }
        if uf.find(winner) != winner {
            return Err(Error::Corrupt(format!(
                "outermost merge {merge_id} has a winner that is not its class root"
            )));
        }
        let seq = bump_seq(&tx)?;
        tx.execute(
            "UPDATE merge_log SET undone_at = ?2, undone_seq = ?3, undo_reason = 'user'
             WHERE id = ?1 AND undone_at IS NULL",
            params![merge_id, now_rfc3339(), seq],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Re-apply an undone merge as a new row (append-only). Returns the new
    /// merge id, or `None` when the works are already in one class or a
    /// strong-id or version veto now forbids joining them.
    pub fn redo_merge(&mut self, merge_id: i64) -> Result<Option<i64>> {
        let tx = self.write()?;
        let row: Option<(i64, i64, String)> = tx
            .query_row(
                "SELECT base_a, base_b, COALESCE(evidence, '') FROM merge_log
                 WHERE id = ?1 AND undone_at IS NOT NULL",
                [merge_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let mut w = Writer::new(&tx)?;
        let new_id = match row {
            Some((a, b, evidence)) => w.join(a, b, "redo", &evidence, false)?,
            None => None,
        };
        tx.commit()?;
        Ok(new_id)
    }

    /// Take one request token for `source` at `now_ms` (Unix ms), shared by
    /// every process on this store. Counts the request against the UTC
    /// day's budget when granted.
    pub fn take_request(
        &mut self,
        source: &str,
        limit: RateLimit,
        now_ms: i64,
    ) -> Result<RateGrant> {
        let tx = self.write()?;
        let day = crate::time::utc_day(now_ms);
        let used: i64 = tx
            .query_row(
                "SELECT requests FROM source_usage WHERE day = ?1 AND source = ?2",
                params![day, source],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if used >= i64::from(limit.daily) {
            tx.commit()?;
            return Ok(RateGrant::DailyExhausted);
        }
        let burst = f64::from(limit.burst.max(1));
        let rate = limit.per_second.max(0.001);
        let (tokens, updated): (f64, i64) = tx
            .query_row(
                "SELECT tokens, updated_ms FROM source_rate WHERE source = ?1",
                [source],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((burst, now_ms));
        let elapsed = (now_ms - updated).max(0) as f64 / 1000.0;
        let tokens = (tokens + elapsed * rate).min(burst);
        let (tokens, grant) = if tokens >= 1.0 {
            (tokens - 1.0, RateGrant::Granted)
        } else {
            let wait = ((1.0 - tokens) / rate * 1000.0).ceil().max(1.0) as u64;
            (tokens, RateGrant::Wait(wait))
        };
        tx.execute(
            "INSERT INTO source_rate(source, tokens, updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(source) DO UPDATE SET tokens = excluded.tokens,
               updated_ms = MAX(updated_ms, excluded.updated_ms)",
            params![source, tokens, now_ms],
        )?;
        if grant == RateGrant::Granted {
            tx.execute(
                "INSERT INTO source_usage(day, source, requests) VALUES (?1, ?2, 1)
                 ON CONFLICT(day, source) DO UPDATE SET requests = requests + 1",
                params![day, source],
            )?;
        }
        tx.commit()?;
        Ok(grant)
    }

    /// Requests counted for `source` on the UTC day of `now_ms`.
    pub fn requests_today(&mut self, source: &str, now_ms: i64) -> Result<u64> {
        let day = crate::time::utc_day(now_ms);
        let used: Option<i64> = self
            .conn
            .query_row(
                "SELECT requests FROM source_usage WHERE day = ?1 AND source = ?2",
                params![day, source],
                |r| r.get(0),
            )
            .optional()?;
        Ok(used.unwrap_or(0).max(0) as u64)
    }

    #[cfg(test)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

fn set_wal(conn: &Connection) -> Result<()> {
    // Switching to WAL needs a brief exclusive lock; a racing first open can
    // see SQLITE_BUSY without the busy handler, so retry within the timeout.
    let deadline = std::time::Instant::now() + BUSY_TIMEOUT;
    loop {
        match conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0)) {
            Ok(_) => return Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// The latest active merge (by `seq`) among those joining works now in
/// `work`'s class: the only one rules 9 and 12 allow removing.
fn outermost_in_class(conn: &Connection, uf: &UnionFind, work: i64) -> Result<Option<i64>> {
    let root = uf.find(work);
    let mut stmt = conn.prepare_cached(
        "SELECT id, base_a FROM merge_log WHERE undone_at IS NULL ORDER BY seq DESC, id DESC",
    )?;
    for r in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (id, a) = r?;
        if uf.find(a) == root {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn bump_seq(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "UPDATE meta SET value = value + 1 WHERE key = 'seq' RETURNING value",
        [],
        |r| r.get(0),
    )?)
}

fn no_fetch(key: &FetchKey, generation: i64) -> Error {
    Error::NoFetch {
        subject: key.subject.key(),
        direction: key.direction.as_str(),
        origin: key.source.clone(),
        generation,
    }
}

fn base_of(conn: &Connection, id: &ExtId) -> Result<Option<i64>> {
    Ok(conn
        .prepare_cached("SELECT work_id FROM work_ids WHERE scheme = ?1 AND value = ?2")?
        .query_row(params![id.scheme.as_str(), id.value], |r| r.get(0))
        .optional()?)
}

const FETCH_COLUMNS: &str = "generation, cursor, pages, fetched, total_reported, complete, truncated, started_at, started_seq, finished_at";

fn fetch_state_from_row(key: &FetchKey, r: &rusqlite::Row<'_>) -> rusqlite::Result<FetchState> {
    Ok(FetchState {
        key: key.clone(),
        generation: r.get(0)?,
        cursor: r.get(1)?,
        pages: r.get(2)?,
        fetched: r.get::<_, i64>(3)?.max(0) as u64,
        total_reported: r.get::<_, Option<i64>>(4)?.map(|t| t.max(0) as u64),
        complete: r.get(5)?,
        truncated: r.get(6)?,
        started_at: r.get(7)?,
        started_seq: r.get(8)?,
        finished_at: r.get(9)?,
    })
}

fn latest_generation(conn: &Connection, work: i64, key: &FetchKey) -> Result<Option<FetchState>> {
    let sql = format!(
        "SELECT {FETCH_COLUMNS} FROM fetch_log WHERE work = ?1 AND direction = ?2 AND source = ?3
         ORDER BY generation DESC LIMIT 1"
    );
    Ok(conn
        .prepare_cached(&sql)?
        .query_row(params![work, key.direction.as_str(), key.source], |r| {
            fetch_state_from_row(key, r)
        })
        .optional()?)
}

fn newest_complete(conn: &Connection, work: i64, key: &FetchKey) -> Result<Option<i64>> {
    Ok(conn
        .prepare_cached(
            "SELECT MAX(generation) FROM fetch_log
             WHERE work = ?1 AND direction = ?2 AND source = ?3 AND complete = 1",
        )?
        .query_row(params![work, key.direction.as_str(), key.source], |r| {
            r.get::<_, Option<i64>>(0)
        })?)
}

fn generation(
    conn: &Connection,
    work: i64,
    key: &FetchKey,
    generation: i64,
) -> Result<Option<FetchState>> {
    let sql = format!(
        "SELECT {FETCH_COLUMNS} FROM fetch_log
         WHERE work = ?1 AND direction = ?2 AND source = ?3 AND generation = ?4"
    );
    Ok(conn
        .prepare_cached(&sql)?
        .query_row(
            params![work, key.direction.as_str(), key.source, generation],
            |r| fetch_state_from_row(key, r),
        )
        .optional()?)
}

fn ids_of_work(conn: &Connection, work: i64) -> Result<Vec<ExtId>> {
    let mut stmt = conn.prepare_cached("SELECT scheme, value FROM work_ids WHERE work_id = ?1")?;
    let rows = stmt.query_map([work], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (scheme, value) = row?;
        if let Some(scheme) = Scheme::parse(&scheme) {
            out.push(ExtId { scheme, value });
        }
    }
    Ok(out)
}

fn class_ids(conn: &Connection, uf: &UnionFind, root: i64) -> Result<Vec<ExtId>> {
    let mut out = Vec::new();
    for member in uf.members(root) {
        out.extend(ids_of_work(conn, member)?);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn class_meta(conn: &Connection, uf: &UnionFind, root: i64) -> Result<(WorkMeta, bool)> {
    let mut stmt = conn.prepare_cached(
        "SELECT title, year, venue, authors, meta_fetched_at FROM works WHERE work_id = ?1",
    )?;
    let mut meta = WorkMeta::default();
    let mut has_meta = false;
    // Root first, then other members in id order.
    let mut members = uf.members(root);
    members.sort_by_key(|&m| (m != root, m));
    for member in members {
        let row = stmt
            .query_row([member], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<i32>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })
            .optional()?;
        let Some((title, year, venue, authors, fetched)) = row else {
            continue;
        };
        has_meta |= fetched.is_some();
        meta.title = meta.title.or(title);
        meta.year = meta.year.or(year);
        meta.venue = meta.venue.or(venue);
        if meta.authors.is_empty()
            && let Some(authors) = authors
        {
            meta.authors = serde_json::from_str(&authors).unwrap_or_default();
        }
    }
    Ok((meta, has_meta))
}

/// Roots of classes linked to `root` as other versions (one hop).
fn version_roots(conn: &Connection, uf: &UnionFind, root: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT version_of FROM work_versions WHERE work = ?1
         UNION SELECT work FROM work_versions WHERE version_of = ?1",
    )?;
    let mut roots = Vec::new();
    for member in uf.members(root) {
        for other in stmt.query_map([member], |r| r.get::<_, i64>(0))? {
            let other_root = uf.find(other?);
            if other_root != root && !roots.contains(&other_root) {
                roots.push(other_root);
            }
        }
    }
    Ok(roots)
}

fn version_ids(conn: &Connection, uf: &UnionFind, root: i64) -> Result<Vec<ExtId>> {
    let mut out = Vec::new();
    for r in version_roots(conn, uf, root)? {
        out.extend(class_ids(conn, uf, r)?);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// The subject's root plus its versions' roots, and all their members.
/// Display aggregates across versions; storage keeps them apart.
fn subject_scope(conn: &Connection, uf: &UnionFind, root: i64) -> Result<(Vec<i64>, Vec<i64>)> {
    let mut roots = vec![root];
    roots.extend(version_roots(conn, uf, root)?);
    let mut members = Vec::new();
    for &r in &roots {
        members.extend(uf.members(r));
    }
    Ok((roots, members))
}

/// `(table, subject column, other column)` for a direction.
fn edge_columns(direction: Direction) -> (&'static str, &'static str, &'static str) {
    match direction {
        Direction::Refs => ("edge_sources", "citing", "cited"),
        Direction::CitedBy => ("edge_sources", "cited", "citing"),
        Direction::Related => ("related_works", "work", "related"),
    }
}

/// SQL truth value: row `e` is stale for this direction's view. Scoped to
/// the same base work, direction and source: a complete, untruncated fetch
/// that started after the row was last seen by that direction (or first
/// stored) did not return it.
fn stale_sql(direction: Direction) -> &'static str {
    match direction {
        Direction::Refs => {
            "EXISTS (SELECT 1 FROM fetch_log f WHERE f.work = e.citing AND f.direction = 'refs'
               AND f.source = e.source AND f.complete = 1 AND f.truncated = 0
               AND f.started_seq > COALESCE(e.refs_seen_seq, e.first_seen_seq))"
        }
        Direction::CitedBy => {
            "EXISTS (SELECT 1 FROM fetch_log f WHERE f.work = e.cited AND f.direction = 'cited_by'
               AND f.source = e.source AND f.complete = 1 AND f.truncated = 0
               AND f.started_seq > COALESCE(e.cited_by_seen_seq, e.first_seen_seq))"
        }
        Direction::Related => {
            "EXISTS (SELECT 1 FROM fetch_log f WHERE f.work = e.work AND f.direction = 'related'
               AND f.source = e.source AND f.complete = 1 AND f.truncated = 0
               AND f.started_seq > e.last_seen_seq)"
        }
    }
}

/// Neighbor class roots in first-insertion order, excluding the subject's
/// scope (resolved self-loops).
fn neighbor_order(
    conn: &Connection,
    uf: &UnionFind,
    roots: &[i64],
    members: &[i64],
    direction: Direction,
    include_stale: bool,
) -> Result<Vec<i64>> {
    let (table, subject_col, other_col) = edge_columns(direction);
    let filter = if include_stale {
        String::new()
    } else {
        format!("AND NOT {}", stale_sql(direction))
    };
    let sql = format!(
        "SELECT e.{other_col}, MIN(e.rowid) FROM {table} e
         WHERE e.{subject_col} = ?1 {filter} GROUP BY e.{other_col}"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut first: HashMap<i64, i64> = HashMap::new();
    for &m in members {
        for row in stmt.query_map([m], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (other, rowid) = row?;
            let root = uf.find(other);
            if roots.contains(&root) {
                continue;
            }
            first
                .entry(root)
                .and_modify(|r| *r = (*r).min(rowid))
                .or_insert(rowid);
        }
    }
    let mut ordered: Vec<(i64, i64)> = first.into_iter().map(|(root, row)| (row, root)).collect();
    ordered.sort_unstable();
    Ok(ordered.into_iter().map(|(_, root)| root).collect())
}

struct RawEdge {
    citing: i64,
    cited: i64,
    source: String,
    first_seen: String,
    last_seen: String,
    intents: Vec<String>,
    is_influential: Option<bool>,
    stale: bool,
}

/// Edge rows between the subject's `members` and one neighbor class.
fn edge_rows_between(
    conn: &Connection,
    members: &[i64],
    others: &[i64],
    direction: Direction,
) -> Result<Vec<RawEdge>> {
    let (table, subject_col, other_col) = edge_columns(direction);
    let (citing_col, cited_col, extra) = if direction == Direction::Related {
        ("work", "related", "NULL, NULL")
    } else {
        ("citing", "cited", "e.intents, e.is_influential")
    };
    let sql = format!(
        "SELECT e.{citing_col}, e.{cited_col}, e.source, e.first_seen, e.last_seen, {extra}, {stale}
         FROM {table} e WHERE e.{subject_col} = ?1 AND e.{other_col} = ?2 ORDER BY e.rowid",
        stale = stale_sql(direction)
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut out = Vec::new();
    for &m in members {
        for &o in others {
            let rows = stmt.query_map(params![m, o], |r| {
                let intents: Option<String> = r.get(5)?;
                Ok(RawEdge {
                    citing: r.get(0)?,
                    cited: r.get(1)?,
                    source: r.get(2)?,
                    first_seen: r.get(3)?,
                    last_seen: r.get(4)?,
                    intents: intents
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    is_influential: r.get(6)?,
                    stale: r.get(7)?,
                })
            })?;
            for row in rows {
                out.push(row?);
            }
        }
    }
    Ok(out)
}

/// FNV-1a 64-bit, hex. Stable content key for contexts and raw refs.
fn fnv_hex(s: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    let hash = s
        .bytes()
        .fold(OFFSET, |h, b| (h ^ u64::from(b)).wrapping_mul(PRIME));
    format!("{hash:016x}")
}

/// Writes inside one transaction, with resolution loaded in that
/// transaction and kept current as it merges.
struct Writer<'t> {
    tx: &'t Transaction<'t>,
    uf: UnionFind,
    conflicts: u32,
    pending_splits: u32,
}

impl<'t> Writer<'t> {
    fn new(tx: &'t Transaction<'t>) -> Result<Self> {
        Ok(Self {
            uf: UnionFind::load(tx)?,
            tx,
            conflicts: 0,
            pending_splits: 0,
        })
    }

    /// Base work of `id`, minting a work for it when unknown. Every
    /// external id gets its own base work (as in the spec); co-reported ids
    /// are joined by logged merges, so any join can be undone.
    fn base(&mut self, id: &ExtId, source: &str) -> Result<i64> {
        if let Some(base) = base_of(self.tx, id)? {
            return Ok(base);
        }
        self.tx
            .execute("INSERT INTO works(created_at) VALUES (?1)", [now_rfc3339()])?;
        let work = self.tx.last_insert_rowid();
        self.tx.execute(
            "INSERT INTO work_ids(scheme, value, work_id, source, seen_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(scheme, value) DO NOTHING",
            params![id.scheme.as_str(), id.value, work, source, now_rfc3339()],
        )?;
        // Read the mapping back rather than trusting the insert.
        base_of(self.tx, id)?
            .ok_or_else(|| Error::Corrupt(format!("{} has no work after insert", id.key())))
    }

    fn class_strong_ids(&self, root: i64) -> Result<StrongIds> {
        let mut out = StrongIds::default();
        for member in self.uf.members(root) {
            for id in ids_of_work(self.tx, member)? {
                out.push(&id);
            }
        }
        Ok(out)
    }

    /// The most recent row for this evidence pair was undone by a user.
    fn undone_pair(&self, a: i64, b: i64) -> Result<bool> {
        let reason: Option<Option<String>> = self
            .tx
            .prepare_cached(
                "SELECT CASE WHEN undone_at IS NULL THEN NULL ELSE undo_reason END FROM merge_log
                 WHERE (base_a = ?1 AND base_b = ?2) OR (base_a = ?2 AND base_b = ?1)
                 ORDER BY id DESC LIMIT 1",
            )?
            .query_row(params![a, b], |r| r.get(0))
            .optional()?;
        Ok(matches!(reason, Some(Some(r)) if r == "user"))
    }

    /// Join the classes of evidence works `a` and `b` with a logged merge,
    /// unless they are already one class or a veto applies: contradicting
    /// DOI/arXiv/PMID, a class that would hold both an arXiv id and a
    /// publisher DOI (versions are never merged), or (when `sticky`) a user
    /// undo of this same evidence pair. The roots are computed here, inside
    /// the merge transaction.
    fn join(
        &mut self,
        a: i64,
        b: i64,
        reason: &str,
        evidence: &str,
        sticky: bool,
    ) -> Result<Option<i64>> {
        let (ra, rb) = (self.uf.find(a), self.uf.find(b));
        if ra == rb {
            return Ok(None);
        }
        let (ia, ib) = (self.class_strong_ids(ra)?, self.class_strong_ids(rb)?);
        if ia.conflicts(&ib) || ia.mixes_versions(&ib) || (sticky && self.undone_pair(a, b)?) {
            self.conflicts += 1;
            return Ok(None);
        }
        let seq = bump_seq(self.tx)?;
        self.tx.execute(
            "INSERT INTO merge_log(winner, loser, base_a, base_b, reason, evidence, at, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                ra.min(rb),
                ra.max(rb),
                a,
                b,
                reason,
                evidence,
                now_rfc3339(),
                seq
            ],
        )?;
        let id = self.tx.last_insert_rowid();
        self.uf.link(a, b);
        Ok(Some(id))
    }

    /// Undo an active merge that directly joins `a` and `b` because a later
    /// record shows them to be two versions of one paper (not sticky). Rule
    /// 12: only when that merge is the outermost of its class, as for a user
    /// undo; otherwise write nothing and count the split as pending (the
    /// mixed-version veto then keeps the two sides from growing together).
    fn split_versions(&mut self, a: i64, b: i64) -> Result<()> {
        let row: Option<i64> = self
            .tx
            .prepare_cached(
                "SELECT id FROM merge_log WHERE undone_at IS NULL
                 AND ((base_a = ?1 AND base_b = ?2) OR (base_a = ?2 AND base_b = ?1))",
            )?
            .query_row(params![a, b], |r| r.get(0))
            .optional()?;
        let Some(merge_id) = row else {
            return Ok(());
        };
        if outermost_in_class(self.tx, &self.uf, a)? != Some(merge_id) {
            self.pending_splits += 1;
            return Ok(());
        }
        self.tx.execute(
            "UPDATE merge_log SET undone_at = ?2, undone_seq = ?3, undo_reason = 'version_split'
             WHERE id = ?1",
            params![merge_id, now_rfc3339(), bump_seq(self.tx)?],
        )?;
        self.uf = UnionFind::load(self.tx)?;
        Ok(())
    }

    /// Resolve one record to the base work of its primary id. Every id gets
    /// a base work; the record's ids are joined to the primary by logged
    /// merges. When the record carries both an arXiv id and a publisher DOI
    /// (Semantic Scholar folds versions), the arXiv ids form the preprint
    /// and everything else the published version; the two are linked in
    /// `work_versions`, and an earlier direct merge between them is split,
    /// so the result does not depend on arrival order.
    fn observe(&mut self, record: &WorkRecord, source: &str) -> Result<i64> {
        let mut ids = record.ids.clone();
        if !ids.contains(&record.primary) {
            ids.push(record.primary.clone());
        }
        let has_doi = ids.iter().any(|i| i.scheme == Scheme::Doi);
        let has_arxiv = ids.iter().any(|i| i.scheme == Scheme::Arxiv);
        let evidence = serde_json::to_string(&ids.iter().map(ExtId::key).collect::<Vec<_>>())
            .map_err(|e| Error::Corrupt(e.to_string()))?;
        let evidence = format!("{source}:{evidence}");

        let mut bases = Vec::with_capacity(ids.len());
        for id in &ids {
            bases.push((id.clone(), self.base(id, source)?));
        }
        let groups: Vec<Vec<(ExtId, i64)>> = if has_doi && has_arxiv {
            let (pre, publ): (Vec<_>, Vec<_>) = bases
                .into_iter()
                .partition(|(i, _)| i.scheme == Scheme::Arxiv);
            for &(_, p) in &pre {
                for &(_, q) in &publ {
                    self.split_versions(p, q)?;
                }
            }
            vec![pre, publ]
        } else {
            vec![bases]
        };

        let mut anchors = Vec::new();
        for group in &groups {
            let anchor = group
                .iter()
                .find(|(i, _)| *i == record.primary)
                .or_else(|| group.first())
                .map(|&(_, b)| b);
            let Some(anchor) = anchor else { continue };
            for &(_, base) in group {
                if base != anchor {
                    self.join(anchor, base, "strong_id", &evidence, true)?;
                }
            }
            anchors.push(anchor);
        }
        if let [pre, publ] = anchors[..]
            && self.uf.find(pre) != self.uf.find(publ)
        {
            self.tx.execute(
                "INSERT OR IGNORE INTO work_versions(work, version_of, kind, source)
                 VALUES (?1, ?2, 'preprint', ?3)",
                params![pre, publ, source],
            )?;
        }

        let primary = base_of(self.tx, &record.primary)?.ok_or_else(|| {
            Error::Corrupt(format!(
                "{} has no work after resolution",
                record.primary.key()
            ))
        })?;
        if let Some(meta) = &record.meta {
            let authors = (!meta.authors.is_empty())
                .then(|| serde_json::to_string(&meta.authors))
                .transpose()
                .map_err(|e| Error::Corrupt(e.to_string()))?;
            self.tx.execute(
                "UPDATE works SET title = COALESCE(?2, title), year = COALESCE(?3, year),
                   venue = COALESCE(?4, venue), authors = COALESCE(?5, authors),
                   meta_source = ?6, meta_fetched_at = ?7
                 WHERE work_id = ?1",
                params![
                    primary,
                    meta.title,
                    meta.year,
                    meta.venue,
                    authors,
                    source,
                    now_rfc3339()
                ],
            )?;
        }
        Ok(primary)
    }

    fn upsert_edge(
        &mut self,
        citing: i64,
        cited: i64,
        key: &FetchKey,
        row: &PageRow,
        stamp: i64,
        now: &str,
    ) -> Result<()> {
        let intents = (!row.intents.is_empty())
            .then(|| serde_json::to_string(&row.intents))
            .transpose()
            .map_err(|e| Error::Corrupt(e.to_string()))?;
        let (refs, cited_by) = match key.direction {
            Direction::Refs => (Some(stamp), None),
            _ => (None, Some(stamp)),
        };
        self.tx.execute(
            "INSERT INTO edge_sources(citing, cited, source, first_seen, last_seen, first_seen_seq,
                                      refs_seen_seq, cited_by_seen_seq, intents, is_influential)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(citing, cited, source) DO UPDATE SET
               last_seen = excluded.last_seen,
               refs_seen_seq = MAX(COALESCE(refs_seen_seq, excluded.refs_seen_seq),
                                   COALESCE(excluded.refs_seen_seq, refs_seen_seq)),
               cited_by_seen_seq = MAX(COALESCE(cited_by_seen_seq, excluded.cited_by_seen_seq),
                                       COALESCE(excluded.cited_by_seen_seq, cited_by_seen_seq)),
               intents = COALESCE(excluded.intents, intents),
               is_influential = COALESCE(excluded.is_influential, is_influential)",
            params![
                citing,
                cited,
                key.source,
                now,
                stamp,
                refs,
                cited_by,
                intents,
                row.is_influential
            ],
        )?;
        for text in &row.contexts {
            self.tx.execute(
                "INSERT OR IGNORE INTO contexts(citing, cited, source, text_hash, text, first_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![citing, cited, key.source, fnv_hex(text), text, now],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
