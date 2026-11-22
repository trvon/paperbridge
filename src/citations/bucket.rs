//! Per-source request rate, daily budgets and per-call request budgets.
//!
//! The request router bounds concurrency and honours cooldowns; it does not
//! bound rate. Rate tokens and the daily count live in the citation store
//! (`Store::take_request`), so the CLI and a running MCP server share one
//! limit per source. Every attempt, including a retry after 429/503, takes
//! a token and spends the per-call budget.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use citegraph::RateLimit;

/// OpenAlex polite-pool guidance (10 requests/s) and a conservative daily
/// cap; re-check against the configured key's quota.
pub const OPENALEX_LIMIT: RateLimit = RateLimit {
    per_second: 10.0,
    burst: 10,
    daily: 20_000,
};
/// Semantic Scholar's default per-key rate (1 request/s).
pub const SEMANTIC_SCHOLAR_LIMIT: RateLimit = RateLimit {
    per_second: 1.0,
    burst: 1,
    daily: 5_000,
};

/// Requests one citation call may send per source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallBudget {
    pub openalex: u32,
    pub semantic_scholar: u32,
}

impl Default for CallBudget {
    fn default() -> Self {
        Self {
            openalex: 12,
            semantic_scholar: 6,
        }
    }
}

/// Requests spent against a budget for one source, reported in diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BudgetUse {
    pub source: String,
    pub limit: u32,
    pub used: u32,
    /// A request was refused because the budget was spent.
    pub exhausted: bool,
}

#[derive(Debug)]
pub(crate) struct BudgetLedger {
    uses: BTreeMap<&'static str, BudgetUse>,
}

impl BudgetLedger {
    pub(crate) fn new(budget: &CallBudget) -> Self {
        let mut uses = BTreeMap::new();
        for (source, limit) in [
            ("openalex", budget.openalex),
            ("semantic_scholar", budget.semantic_scholar),
        ] {
            uses.insert(
                source,
                BudgetUse {
                    source: source.to_string(),
                    limit,
                    used: 0,
                    exhausted: false,
                },
            );
        }
        Self { uses }
    }

    /// Spend one request; false (and marked exhausted) when none are left.
    pub(crate) fn spend(&mut self, source: &'static str) -> bool {
        let Some(entry) = self.uses.get_mut(source) else {
            return false;
        };
        if entry.used >= entry.limit {
            entry.exhausted = true;
            return false;
        }
        entry.used += 1;
        true
    }

    pub(crate) fn report(&self) -> Vec<BudgetUse> {
        self.uses
            .values()
            .filter(|u| u.used > 0 || u.exhausted)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_refuses_past_limit_and_reports() {
        let mut ledger = BudgetLedger::new(&CallBudget {
            openalex: 1,
            semantic_scholar: 0,
        });
        assert!(ledger.spend("openalex"));
        assert!(!ledger.spend("openalex"));
        assert!(!ledger.spend("semantic_scholar"));
        let report = ledger.report();
        assert_eq!(report.len(), 2);
        assert!(report.iter().all(|u| u.exhausted));
    }
}
