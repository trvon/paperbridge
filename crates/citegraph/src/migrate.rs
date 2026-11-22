//! Forward-only schema migrations keyed by `PRAGMA user_version`.
//!
//! `MIGRATIONS[i]` moves the schema from version `i` to `i + 1`. Never edit
//! a shipped entry; append a new one. Review tables (stage 3) arrive as a
//! later migration.

use crate::{Error, Result};
use rusqlite::{Connection, TransactionBehavior};

const V1: &str = r#"
CREATE TABLE meta(
  key TEXT PRIMARY KEY,
  value INTEGER NOT NULL
) WITHOUT ROWID;
INSERT INTO meta(key, value) VALUES ('seq', 0);

CREATE TABLE works(
  work_id INTEGER PRIMARY KEY,
  title TEXT,
  year INTEGER,
  venue TEXT,
  authors TEXT,
  meta_source TEXT,
  meta_fetched_at TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE work_ids(
  scheme TEXT NOT NULL,
  value TEXT NOT NULL,
  work_id INTEGER NOT NULL REFERENCES works(work_id),
  source TEXT NOT NULL,
  seen_at TEXT NOT NULL,
  PRIMARY KEY(scheme, value)
) WITHOUT ROWID;
CREATE INDEX work_ids_by_work ON work_ids(work_id);

CREATE TABLE merge_log(
  id INTEGER PRIMARY KEY,
  winner INTEGER NOT NULL REFERENCES works(work_id),
  loser INTEGER NOT NULL REFERENCES works(work_id),
  reason TEXT NOT NULL,
  evidence TEXT,
  at TEXT NOT NULL,
  undone_at TEXT,
  CHECK (winner <> loser)
);
-- A work has at most one active parent: the merge relation is a forest.
CREATE UNIQUE INDEX merge_log_active_loser ON merge_log(loser) WHERE undone_at IS NULL;

CREATE TABLE work_versions(
  work INTEGER NOT NULL REFERENCES works(work_id),
  version_of INTEGER NOT NULL REFERENCES works(work_id),
  kind TEXT NOT NULL,
  source TEXT NOT NULL,
  PRIMARY KEY(work, version_of),
  CHECK (work <> version_of)
) WITHOUT ROWID;

CREATE TABLE edge_sources(
  citing INTEGER NOT NULL REFERENCES works(work_id),
  cited INTEGER NOT NULL REFERENCES works(work_id),
  source TEXT NOT NULL,
  first_seen TEXT NOT NULL,
  last_seen TEXT NOT NULL,
  last_seen_seq INTEGER NOT NULL,
  intents TEXT,
  is_influential INTEGER,
  PRIMARY KEY(citing, cited, source)
);
CREATE INDEX edge_sources_by_cited ON edge_sources(cited);

CREATE TABLE related_works(
  work INTEGER NOT NULL REFERENCES works(work_id),
  related INTEGER NOT NULL REFERENCES works(work_id),
  source TEXT NOT NULL,
  first_seen TEXT NOT NULL,
  last_seen TEXT NOT NULL,
  last_seen_seq INTEGER NOT NULL,
  PRIMARY KEY(work, related, source)
);

CREATE TABLE contexts(
  citing INTEGER NOT NULL,
  cited INTEGER NOT NULL,
  source TEXT NOT NULL,
  text_hash TEXT NOT NULL,
  text TEXT NOT NULL,
  first_seen TEXT NOT NULL,
  PRIMARY KEY(citing, cited, source, text_hash)
) WITHOUT ROWID;

CREATE TABLE unresolved_refs(
  work INTEGER NOT NULL REFERENCES works(work_id),
  direction TEXT NOT NULL,
  source TEXT NOT NULL,
  raw_hash TEXT NOT NULL,
  raw TEXT NOT NULL,
  seen_at TEXT NOT NULL,
  PRIMARY KEY(work, direction, source, raw_hash)
) WITHOUT ROWID;

CREATE TABLE fetch_log(
  work INTEGER NOT NULL REFERENCES works(work_id),
  direction TEXT NOT NULL,
  source TEXT NOT NULL,
  generation INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  started_seq INTEGER NOT NULL,
  finished_at TEXT,
  cursor TEXT,
  pages INTEGER NOT NULL DEFAULT 0,
  total_reported INTEGER,
  fetched INTEGER NOT NULL DEFAULT 0,
  truncated INTEGER NOT NULL DEFAULT 0,
  complete INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(work, direction, source, generation)
) WITHOUT ROWID;
"#;

const MIGRATIONS: &[&str] = &[V1];

pub(crate) const LATEST: u32 = MIGRATIONS.len() as u32;

/// Bring `conn` to [`LATEST`]. Runs under `BEGIN IMMEDIATE` and re-reads
/// `user_version` inside the transaction, so two processes opening a new
/// store at once apply each migration exactly once.
pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    if user_version(conn)? == LATEST {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let found = user_version(&tx)?;
    if found > LATEST {
        return Err(Error::NewerSchema {
            found,
            supported: LATEST,
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index as u32 + 1)?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn user_version(conn: &Connection) -> Result<u32> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_fresh_store_once_and_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), LATEST);
        migrate(&mut conn).unwrap();
        let seq: i64 = conn
            .query_row("SELECT value FROM meta WHERE key = 'seq'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seq, 0);
    }

    #[test]
    fn refuses_newer_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", LATEST + 1)
            .unwrap();
        assert!(matches!(
            migrate(&mut conn),
            Err(Error::NewerSchema { found, .. }) if found == LATEST + 1
        ));
    }

    #[test]
    fn concurrent_first_open_applies_migrations_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.sqlite");
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || crate::Store::open(&path).map(|_| ()))
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        assert_eq!(user_version(&conn).unwrap(), LATEST);
    }
}
