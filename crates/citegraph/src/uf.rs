//! Identity resolution over active `merge_log` rows (`undone_at IS NULL`).
//!
//! Each merge row records the two *evidence base works* it joined
//! (`base_a`, `base_b`): the works the co-reported external ids map to in
//! `work_ids`. A work's class is its connected component over active rows,
//! and the class root is the smallest work id in it. Resolution is
//! recomputed from the rows that remain active (design rule 9), so undoing
//! any merge, in any order, leaves every other merge's evidence intact.
//!
//! Rows are only inserted between two different classes, so active rows
//! always form a forest; [`UnionFind::load`] reports a cycle as corruption.
//!
//! Loaded inside each transaction and updated in memory as that
//! transaction merges, so resolution is never carried across transactions.

use crate::{Error, Result};
use rusqlite::Connection;
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub(crate) struct UnionFind {
    parent: HashMap<i64, i64>,
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

    fn root_of(&self, work: i64) -> i64 {
        let mut current = work;
        while let Some(&next) = self.parent.get(&current) {
            if next == current {
                break;
            }
            current = next;
        }
        current
    }

    /// Class root of `work`: the smallest work id in its component.
    pub(crate) fn find(&self, work: i64) -> i64 {
        self.root_of(work)
    }

    #[cfg(test)]
    pub(crate) fn is_root(&self, work: i64) -> bool {
        self.find(work) == work
    }

    /// Join the components of `a` and `b`; false if already joined.
    pub(crate) fn link(&mut self, a: i64, b: i64) -> bool {
        let (ra, rb) = (self.root_of(a), self.root_of(b));
        if ra == rb {
            return false;
        }
        let (keep, child) = (ra.min(rb), ra.max(rb));
        self.parent.insert(child, keep);
        self.parent.entry(keep).or_insert(keep);
        // Compress the two paths walked.
        for w in [a, b] {
            let mut current = w;
            while let Some(&next) = self.parent.get(&current) {
                if next == current {
                    break;
                }
                self.parent.insert(current, keep);
                current = next;
            }
        }
        self.edges += 1;
        true
    }

    /// Every work in `root`'s class, including `root`, sorted.
    pub(crate) fn members(&self, root: i64) -> Vec<i64> {
        let root = self.find(root);
        let mut out: Vec<i64> = self
            .parent
            .keys()
            .copied()
            .filter(|&w| self.find(w) == root)
            .collect();
        if !out.contains(&root) {
            out.push(root);
        }
        out.sort_unstable();
        out
    }

    #[cfg(test)]
    pub(crate) fn edge_count(&self) -> usize {
        self.edges
    }
}
