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
    /// Ids withheld because they contradicted a strong id already held by
    /// the work, or merges vetoed by the strong-id rule.
    pub identity_conflicts: u32,
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

    /// Record works (for example hydrated metadata) in one transaction.
    /// Returns the number of identity conflicts.
    pub fn observe(&mut self, records: &[WorkRecord], source: &str) -> Result<u32> {
        let tx = self.write()?;
        let mut w = Writer::new(&tx)?;
        for record in records {
            w.observe(record, source)?;
        }
        let conflicts = w.conflicts;
        tx.commit()?;
        Ok(conflicts)
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
                let seq = w.bump_seq()?;
                let now = now_rfc3339();
                tx.execute(
                    "INSERT INTO fetch_log(work, direction, source, generation, started_at, started_seq)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![base, direction.as_str(), source, generation, now, seq],
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
    /// `BEGIN IMMEDIATE` transaction that (1) re-resolves the subject and
    /// every page id through `work_ids` *inside* the transaction, minting
    /// works for unknown ids, because a merge, undo or another writer may
    /// have landed while the page was in flight; (2) stores edges by base
    /// work (the `work_ids` mapping, never the union-find root) so undoing a
    /// merge cannot strand them; (3) advances the cursor only if `state` is
    /// still the committed position (`pages` equals the snapshot; the
    /// spec's `cursor' = max(cursor, page)`), so a replayed page never moves
    /// the cursor back and changes no ids, works or edges.
    pub fn commit_page(&mut self, state: &FetchState, page: &Page) -> Result<CommitOutcome> {
        let key = &state.key;
        let tx = self.write()?;
        let mut w = Writer::new(&tx)?;
        let subject = w.observe(&WorkRecord::new(key.subject.clone()), &key.source)?;
        let current = generation(&tx, subject, key, state.generation)?
            .ok_or_else(|| no_fetch(key, state.generation))?;
        let replay = current.pages != state.pages || !current.resumable();
        let seq = w.bump_seq()?;
        let now = now_rfc3339();

        for row in &page.rows {
            let other = w.observe(&row.work, &key.source)?;
            match key.direction {
                Direction::Refs => w.upsert_edge(subject, other, key, row, seq, &now)?,
                Direction::CitedBy => w.upsert_edge(other, subject, key, row, seq, &now)?,
                Direction::Related => {
                    tx.execute(
                        "INSERT INTO related_works(work, related, source, first_seen, last_seen, last_seen_seq)
                         VALUES (?1, ?2, ?3, ?4, ?4, ?5)
                         ON CONFLICT(work, related, source) DO UPDATE SET
                           last_seen = excluded.last_seen, last_seen_seq = excluded.last_seen_seq",
                        params![subject, other, key.source, now, seq],
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
        let identity_conflicts = w.conflicts;
        tx.commit()?;
        Ok(CommitOutcome {
            state,
            replay,
            identity_conflicts,
        })
    }

    /// All external ids of `id`'s resolved class (empty if unknown).
    pub fn class_ids(&mut self, id: &ExtId) -> Result<Vec<ExtId>> {
        let tx = self.conn.transaction()?;
        let uf = UnionFind::load(&tx)?;
        let Some(base) = base_of(&tx, id)? else {
            return Ok(Vec::new());
        };
        class_ids(&tx, &uf, uf.find(base)?)
    }

    /// External ids of `id`'s class, followed by those of its other
    /// versions (preprint / published).
    pub fn linked_ids(&mut self, id: &ExtId) -> Result<Vec<ExtId>> {
        let tx = self.conn.transaction()?;
        let uf = UnionFind::load(&tx)?;
        let Some(base) = base_of(&tx, id)? else {
            return Ok(Vec::new());
        };
        let root = uf.find(base)?;
        let mut out = class_ids(&tx, &uf, root)?;
        out.extend(version_ids(&tx, &uf, root)?);
        Ok(out)
    }

    /// Latest fetch generation per source for `id`'s class (and its
    /// versions) and direction.
    pub fn coverage(&mut self, id: &ExtId, direction: Direction) -> Result<Vec<FetchState>> {
        let tx = self.conn.transaction()?;
        let uf = UnionFind::load(&tx)?;
        let Some(base) = base_of(&tx, id)? else {
            return Ok(Vec::new());
        };
        let (_, members) = subject_scope(&tx, &uf, uf.find(base)?)?;
        let mut latest: BTreeMap<String, FetchState> = BTreeMap::new();
        for &work in &members {
            let mut stmt = tx.prepare_cached(
                "SELECT source, MAX(generation) FROM fetch_log WHERE work = ?1 AND direction = ?2
                 GROUP BY source",
            )?;
            let found = stmt
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
                        .is_none_or(|cur| state.started_at > cur.started_at);
                    if newer {
                        latest.insert(source, state);
                    }
                }
            }
        }
        Ok(latest.into_values().collect())
    }

    /// Resolved neighbors of `id` in `direction`, deduplicated across
    /// sources and merges, with resolved self-loops dropped. Ordered by
    /// first insertion.
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
        let (roots, members) = subject_scope(&tx, &uf, uf.find(base)?)?;
        let raw = edge_rows(&tx, &members, direction)?;

        struct Group {
            order: i64,
            sources: BTreeMap<String, EdgeSource>,
            pairs: Vec<(i64, i64, String)>,
        }
        let mut groups: HashMap<i64, Group> = HashMap::new();
        for row in raw {
            let other_root = uf.find(row.other)?;
            if roots.contains(&other_root) {
                continue;
            }
            let group = groups.entry(other_root).or_insert_with(|| Group {
                order: row.rowid,
                sources: BTreeMap::new(),
                pairs: Vec::new(),
            });
            group.order = group.order.min(row.rowid);
            group
                .pairs
                .push((row.citing, row.cited, row.source.clone()));
            let entry = group
                .sources
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
        let mut ordered: Vec<(i64, Group)> = groups
            .into_iter()
            .filter(|(_, g)| query.include_stale || g.sources.values().any(|s| !s.stale))
            .collect();
        ordered.sort_by_key(|(r, g)| (g.order, *r));
        let total = ordered.len();
        let mut items = Vec::new();
        for (other_root, group) in ordered.into_iter().skip(query.offset).take(query.limit) {
            let (meta, has_meta) = class_meta(&tx, &uf, other_root)?;
            let mut contexts = Vec::new();
            if query.include_contexts {
                let mut stmt = tx.prepare_cached(
                    "SELECT source, text FROM contexts WHERE citing = ?1 AND cited = ?2 AND source = ?3
                     ORDER BY first_seen, text_hash",
                )?;
                for (citing, cited, source) in &group.pairs {
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
            items.push(Neighbor {
                ids: class_ids(&tx, &uf, other_root)?,
                meta,
                has_meta,
                sources: group.sources.into_values().collect(),
                contexts,
                versions: version_ids(&tx, &uf, other_root)?,
            });
        }
        Ok(NeighborPage { total, items })
    }

    /// Active and undone merges, oldest first.
    pub fn merges(&mut self) -> Result<Vec<MergeInfo>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, reason, evidence, at, seq, undone_at, undone_seq FROM merge_log ORDER BY id",
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
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Undo an active merge by setting `undone_at`. Returns false if the
    /// merge does not exist or is already undone.
    pub fn undo_merge(&mut self, merge_id: i64) -> Result<bool> {
        let tx = self.write()?;
        let seq = bump_seq(&tx)?;
        let changed = tx.execute(
            "UPDATE merge_log SET undone_at = ?2, undone_seq = ?3 WHERE id = ?1 AND undone_at IS NULL",
            params![merge_id, now_rfc3339(), seq],
        )?;
        tx.commit()?;
        Ok(changed == 1)
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

const FETCH_COLUMNS: &str = "generation, cursor, pages, fetched, total_reported, complete, truncated, started_at, finished_at";

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
        finished_at: r.get(8)?,
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
    for member in uf.members(root)? {
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
    let mut members = uf.members(root)?;
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
    for member in uf.members(root)? {
        for other in stmt.query_map([member], |r| r.get::<_, i64>(0))? {
            let other_root = uf.find(other?)?;
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
        members.extend(uf.members(r)?);
    }
    Ok((roots, members))
}

struct RawEdge {
    rowid: i64,
    citing: i64,
    cited: i64,
    other: i64,
    source: String,
    first_seen: String,
    last_seen: String,
    intents: Vec<String>,
    is_influential: Option<bool>,
    stale: bool,
}

/// Edge rows touching `members` on the subject side, with staleness: a row
/// is stale when a complete, untruncated fetch of the same base work,
/// direction and source started after the row was last seen (that fetch
/// did not return it).
fn edge_rows(conn: &Connection, members: &[i64], direction: Direction) -> Result<Vec<RawEdge>> {
    let (table, subject_col, other_col) = match direction {
        Direction::Refs => ("edge_sources", "citing", "cited"),
        Direction::CitedBy => ("edge_sources", "cited", "citing"),
        Direction::Related => ("related_works", "work", "related"),
    };
    let (extra, stale) = if direction == Direction::Related {
        (
            "NULL, NULL",
            "EXISTS (SELECT 1 FROM fetch_log f WHERE f.work = e.work AND f.direction = 'related'
               AND f.source = e.source AND f.complete = 1 AND f.truncated = 0
               AND f.started_seq > e.last_seen_seq)",
        )
    } else {
        (
            "e.intents, e.is_influential",
            "EXISTS (SELECT 1 FROM fetch_log f WHERE f.source = e.source AND f.complete = 1
               AND f.truncated = 0 AND f.started_seq > e.last_seen_seq
               AND ((f.direction = 'refs' AND f.work = e.citing)
                 OR (f.direction = 'cited_by' AND f.work = e.cited)))",
        )
    };
    let (citing_col, cited_col) = if direction == Direction::Related {
        ("work", "related")
    } else {
        ("citing", "cited")
    };
    let sql = format!(
        "SELECT e.rowid, e.{citing_col}, e.{cited_col}, e.{other_col}, e.source, e.first_seen,
                e.last_seen, {extra}, {stale}
         FROM {table} e WHERE e.{subject_col} = ?1"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut out = Vec::new();
    for &m in members {
        let rows = stmt.query_map([m], |r| {
            let intents: Option<String> = r.get(7)?;
            Ok(RawEdge {
                rowid: r.get(0)?,
                citing: r.get(1)?,
                cited: r.get(2)?,
                other: r.get(3)?,
                source: r.get(4)?,
                first_seen: r.get(5)?,
                last_seen: r.get(6)?,
                intents: intents
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default(),
                is_influential: r.get(8)?,
                stale: r.get(9)?,
            })
        })?;
        for row in rows {
            out.push(row?);
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

/// Writes inside one transaction, with union-find loaded in that
/// transaction and kept current as it merges.
struct Writer<'t> {
    tx: &'t Transaction<'t>,
    uf: UnionFind,
    conflicts: u32,
}

impl<'t> Writer<'t> {
    fn new(tx: &'t Transaction<'t>) -> Result<Self> {
        Ok(Self {
            uf: UnionFind::load(tx)?,
            tx,
            conflicts: 0,
        })
    }

    fn bump_seq(&mut self) -> Result<i64> {
        bump_seq(self.tx)
    }

    /// An undone merge links the two classes: undo is a decision, so the
    /// same evidence must not silently redo it.
    fn undone_between(&self, a: i64, b: i64) -> Result<bool> {
        let mut stmt = self
            .tx
            .prepare_cached("SELECT winner, loser FROM merge_log WHERE undone_at IS NOT NULL")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (winner, loser) in rows {
            let (w, l) = (self.uf.find(winner)?, self.uf.find(loser)?);
            if (w == a && l == b) || (w == b && l == a) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn new_work(&mut self) -> Result<i64> {
        self.tx
            .execute("INSERT INTO works(created_at) VALUES (?1)", [now_rfc3339()])?;
        Ok(self.tx.last_insert_rowid())
    }

    fn insert_id(&mut self, id: &ExtId, work: i64, source: &str) -> Result<()> {
        self.tx.execute(
            "INSERT INTO work_ids(scheme, value, work_id, source, seen_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(scheme, value) DO NOTHING",
            params![id.scheme.as_str(), id.value, work, source, now_rfc3339()],
        )?;
        Ok(())
    }

    fn strong_ids(&self, root: i64) -> Result<StrongIds> {
        let mut out = StrongIds::default();
        for member in self.uf.members(root)? {
            for id in ids_of_work(self.tx, member)? {
                out.push(&id);
            }
        }
        Ok(out)
    }

    /// Resolve one record to the base work of its primary id: mint a work
    /// for wholly unknown records, attach new ids, and merge roots the
    /// record shows to be the same work unless a strong id vetoes it. An
    /// arXiv id reported next to a publisher DOI is a separate version, not
    /// a merge.
    fn observe(&mut self, record: &WorkRecord, source: &str) -> Result<i64> {
        let mut ids = record.ids.clone();
        if !ids.contains(&record.primary) {
            ids.push(record.primary.clone());
        }
        let has_doi = ids.iter().any(|i| i.scheme == Scheme::Doi);
        let (preprint, main): (Vec<ExtId>, Vec<ExtId>) = if has_doi {
            ids.into_iter().partition(|i| i.scheme == Scheme::Arxiv)
        } else {
            (Vec::new(), ids)
        };
        let (primary_group, other_group) = if preprint.contains(&record.primary) {
            (preprint, main)
        } else {
            (main, preprint)
        };
        let primary = self.resolve_group(&primary_group, &record.primary, source)?;
        if !other_group.is_empty() {
            let other_primary = other_group[0].clone();
            let other = self.resolve_group(&other_group, &other_primary, source)?;
            let (pre, publ) = if record.primary.scheme == Scheme::Arxiv {
                (primary, other)
            } else {
                (other, primary)
            };
            if self.uf.find(pre)? != self.uf.find(publ)? {
                self.tx.execute(
                    "INSERT OR IGNORE INTO work_versions(work, version_of, kind, source)
                     VALUES (?1, ?2, 'preprint', ?3)",
                    params![pre, publ, source],
                )?;
            }
        }
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

    fn resolve_group(&mut self, ids: &[ExtId], primary: &ExtId, source: &str) -> Result<i64> {
        // Primary first so its root anchors the group.
        let mut ordered: Vec<&ExtId> = Vec::with_capacity(ids.len());
        if ids.contains(primary) {
            ordered.push(primary);
        }
        ordered.extend(ids.iter().filter(|i| *i != primary));

        let mut known = Vec::new();
        let mut unknown = Vec::new();
        for id in ordered {
            match base_of(self.tx, id)? {
                Some(base) => known.push(base),
                None => unknown.push(id.clone()),
            }
        }
        let Some(&first) = known.first() else {
            let work = self.new_work()?;
            for id in &unknown {
                self.insert_id(id, work, source)?;
            }
            // Read the mapping back rather than trusting the insert.
            return base_of(self.tx, primary)?.ok_or_else(|| {
                Error::Corrupt(format!("{} has no work after insert", primary.key()))
            });
        };

        let mut anchor = self.uf.find(first)?;
        let evidence = serde_json::to_string(&ids.iter().map(ExtId::key).collect::<Vec<_>>())
            .map_err(|e| Error::Corrupt(e.to_string()))?;
        for &base in &known[1..] {
            let root = self.uf.find(base)?;
            if root == anchor {
                continue;
            }
            if self.strong_ids(anchor)?.conflicts(&self.strong_ids(root)?)
                || self.undone_between(anchor, root)?
            {
                self.conflicts += 1;
                continue;
            }
            let (winner, loser) = (anchor.min(root), anchor.max(root));
            let seq = self.bump_seq()?;
            self.tx.execute(
                "INSERT INTO merge_log(winner, loser, reason, evidence, at, seq)
                 VALUES (?1, ?2, 'strong_id', ?3, ?4, ?5)",
                params![
                    winner,
                    loser,
                    format!("{source}:{evidence}"),
                    now_rfc3339(),
                    seq
                ],
            )?;
            self.uf.link(winner, loser)?;
            anchor = winner;
        }

        // New ids attach to the primary's base when known, else the anchor.
        let target = base_of(self.tx, primary)?.unwrap_or(anchor);
        let mut held = self.strong_ids(anchor)?;
        for id in &unknown {
            if id.scheme.is_strong() && held.rejects(id) {
                self.conflicts += 1;
                if id == primary {
                    // The record's own key contradicts the class: it is a
                    // different work.
                    let work = self.new_work()?;
                    self.insert_id(id, work, source)?;
                }
                continue;
            }
            self.insert_id(id, target, source)?;
            held.push(id);
        }
        base_of(self.tx, primary)?.ok_or_else(|| {
            Error::Corrupt(format!("{} has no work after resolution", primary.key()))
        })
    }

    fn upsert_edge(
        &mut self,
        citing: i64,
        cited: i64,
        key: &FetchKey,
        row: &PageRow,
        seq: i64,
        now: &str,
    ) -> Result<()> {
        let intents = (!row.intents.is_empty())
            .then(|| serde_json::to_string(&row.intents))
            .transpose()
            .map_err(|e| Error::Corrupt(e.to_string()))?;
        self.tx.execute(
            "INSERT INTO edge_sources(citing, cited, source, first_seen, last_seen, last_seen_seq,
                                      intents, is_influential)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7)
             ON CONFLICT(citing, cited, source) DO UPDATE SET
               last_seen = excluded.last_seen,
               last_seen_seq = MAX(last_seen_seq, excluded.last_seen_seq),
               intents = COALESCE(excluded.intents, intents),
               is_influential = COALESCE(excluded.is_influential, is_influential)",
            params![
                citing,
                cited,
                key.source,
                now,
                seq,
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
