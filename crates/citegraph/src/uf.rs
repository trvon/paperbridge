//! Identity resolution over active `merge_log` rows (`undone_at IS NULL`).
//!
//! Each merge row records the two *evidence base works* it joined
//! (`base_a`, `base_b`): the works the co-reported external ids map to in
//! `work_ids`. A work's class is its connected component over active rows,
//! and the class root is the smallest work id in it. Resolution is
//! recomputed from the rows that remain active (design rule 9), so undoing
//! any merge leaves every other merge's evidence intact.
//!
//! Rows are only inserted between two different classes, so active rows
//! always form a forest; [`UnionFind::load`] reports a cycle as corruption.
//!
//! Classes are kept as explicit member lists (union by size, relabelling
//! the smaller side), so `find` is O(1) and `members` is O(class size):
//! the cost of a join does not grow with the size of the store.
//!
//! Loaded inside each transaction and updated in memory as that
//! transaction merges, so resolution is never carried across transactions.

use crate::{Error, Result};
use rusqlite::Connection;
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub(crate) struct UnionFind {
    /// work -> class representative (only works in a multi-work class).
    rep: HashMap<i64, i64>,
    /// representative -> (members sorted, smallest member = root).
    classes: HashMap<i64, Vec<i64>>,
    edges: usize,
}

impl UnionFind {
    pub(crate) fn load(conn: &Connection) -> Result<Self> {
        let mut stmt =
            conn.prepare_cached("SELECT base_a, base_b FROM merge_log WHERE undone_at IS NULL")?;
        let mut uf = Self::default();
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (a, b) = row?;
            if !uf.link(a, b) {
                return Err(Error::Corrupt(format!(
                    "active merges form a cycle through works {a} and {b}"
                )));
            }
        }
        Ok(uf)
    }

    fn rep_of(&self, work: i64) -> i64 {
        self.rep.get(&work).copied().unwrap_or(work)
    }

    /// Class root of `work`: the smallest work id in its component.
    pub(crate) fn find(&self, work: i64) -> i64 {
        self.classes
            .get(&self.rep_of(work))
            .and_then(|m| m.first().copied())
            .unwrap_or(work)
    }

    #[cfg(test)]
    pub(crate) fn is_root(&self, work: i64) -> bool {
        self.find(work) == work
    }

    /// Join the components of `a` and `b`; false if already joined.
    pub(crate) fn link(&mut self, a: i64, b: i64) -> bool {
        let (ra, rb) = (self.rep_of(a), self.rep_of(b));
        if ra == rb {
            return false;
        }
        let size = |r: i64, classes: &HashMap<i64, Vec<i64>>| classes.get(&r).map_or(1, Vec::len);
        let (keep, gone) = if size(ra, &self.classes) >= size(rb, &self.classes) {
            (ra, rb)
        } else {
            (rb, ra)
        };
        let moved = self.classes.remove(&gone).unwrap_or_else(|| vec![gone]);
        for &w in &moved {
            self.rep.insert(w, keep);
        }
        self.rep.insert(keep, keep);
        let target = self.classes.entry(keep).or_insert_with(|| vec![keep]);
        target.extend(moved);
        target.sort_unstable();
        self.edges += 1;
        true
    }

    /// Every work in `work`'s class, including its root, sorted.
    pub(crate) fn members(&self, work: i64) -> Vec<i64> {
        self.classes
            .get(&self.rep_of(work))
            .cloned()
            .unwrap_or_else(|| vec![work])
    }

    #[cfg(test)]
    pub(crate) fn edge_count(&self) -> usize {
        self.edges
    }
}

#[cfg(test)]
mod tests {
    use super::UnionFind;

    #[test]
    fn classes_track_min_root_and_members() {
        let mut uf = UnionFind::default();
        assert!(uf.link(5, 9));
        assert!(uf.link(9, 3));
        assert!(uf.link(7, 8));
        assert!(!uf.link(3, 5), "already joined");
        assert_eq!(uf.find(9), 3);
        assert_eq!(uf.members(5), vec![3, 5, 9]);
        assert!(uf.link(8, 5));
        assert_eq!(uf.find(7), 3);
        assert_eq!(uf.members(7), vec![3, 5, 7, 8, 9]);
        assert_eq!(uf.find(42), 42);
        assert_eq!(uf.members(42), vec![42]);
        assert_eq!(uf.edge_count(), 4);
    }
}
