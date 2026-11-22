//! Union-find over active `merge_log` rows (`undone_at IS NULL`).
//!
//! Loaded inside each transaction and updated in memory as that
//! transaction merges, so resolution is never carried across transactions.

use crate::{Error, Result};
use rusqlite::Connection;
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub(crate) struct UnionFind {
    /// loser -> winner for every active merge.
    parent: HashMap<i64, i64>,
}

impl UnionFind {
    pub(crate) fn load(conn: &Connection) -> Result<Self> {
        let mut stmt =
            conn.prepare_cached("SELECT loser, winner FROM merge_log WHERE undone_at IS NULL")?;
        let mut parent = HashMap::new();
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (loser, winner) = row?;
            if parent.insert(loser, winner).is_some() {
                return Err(Error::Corrupt(format!(
                    "work {loser} has two active merge parents"
                )));
            }
        }
        Ok(Self { parent })
    }

    pub(crate) fn is_root(&self, work: i64) -> bool {
        !self.parent.contains_key(&work)
    }

    /// Root of `work`. Bounded by the number of active merges, so a cycle
    /// (which the merge rules never create) is reported, not looped on.
    pub(crate) fn find(&self, work: i64) -> Result<i64> {
        let mut current = work;
        for _ in 0..=self.parent.len() {
            match self.parent.get(&current) {
                Some(&next) => current = next,
                None => return Ok(current),
            }
        }
        Err(Error::Corrupt(format!(
            "merge cycle reachable from work {work}"
        )))
    }

    /// Every work whose root is `root`, including `root`.
    pub(crate) fn members(&self, root: i64) -> Result<Vec<i64>> {
        let mut out = vec![root];
        for &loser in self.parent.keys() {
            if self.find(loser)? == root {
                out.push(loser);
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    /// Record a merge of two distinct roots. Callers insert the row first.
    pub(crate) fn link(&mut self, winner: i64, loser: i64) -> Result<()> {
        if winner == loser || !self.is_root(winner) || !self.is_root(loser) {
            return Err(Error::Corrupt(format!(
                "merge {loser} -> {winner} is not between two distinct roots"
            )));
        }
        self.parent.insert(loser, winner);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn active(&self) -> impl Iterator<Item = (i64, i64)> + '_ {
        self.parent.iter().map(|(&l, &w)| (l, w))
    }
}
