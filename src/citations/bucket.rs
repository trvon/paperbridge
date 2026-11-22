//! Per-source request rate and per-call request budgets.
//!
//! The request router bounds concurrency and honours cooldowns; it does not
//! bound rate. Citation expansion issues many requests to the same source,
//! so each source also gets a token bucket shared by the whole process.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// Sustained requests per second and burst size for one source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub per_second: f64,
    pub burst: u32,
}

impl Rate {
    /// OpenAlex polite-pool guidance: at most 10 requests per second.
    pub const OPENALEX: Self = Self {
        per_second: 10.0,
        burst: 10,
    };
    /// Semantic Scholar's default per-key limit: one request per second.
    pub const SEMANTIC_SCHOLAR: Self = Self {
        per_second: 1.0,
        burst: 1,
    };
}

#[derive(Debug)]
struct BucketState {
    tokens: f64,
    updated: Instant,
}

/// A token bucket. `acquire` waits until a token is available.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    rate: Rate,
    state: Arc<Mutex<BucketState>>,
}

impl TokenBucket {
    pub fn new(rate: Rate) -> Self {
        Self {
            rate: Rate {
                per_second: rate.per_second.max(0.001),
                burst: rate.burst.max(1),
            },
            state: Arc::new(Mutex::new(BucketState {
                tokens: f64::from(rate.burst.max(1)),
                updated: Instant::now(),
            })),
        }
    }

    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut state = self.state.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(state.updated).as_secs_f64();
                state.tokens =
                    (state.tokens + elapsed * self.rate.per_second).min(f64::from(self.rate.burst));
                state.updated = now;
                if state.tokens >= 1.0 {
                    state.tokens -= 1.0;
                    return;
                }
                Duration::from_secs_f64((1.0 - state.tokens) / self.rate.per_second)
            };
            tokio::time::sleep(wait).await;
        }
    }
}

/// Process-wide buckets for the two citation sources.
pub(crate) fn default_buckets() -> (TokenBucket, TokenBucket) {
    static BUCKETS: OnceLock<(TokenBucket, TokenBucket)> = OnceLock::new();
    BUCKETS
        .get_or_init(|| {
            (
                TokenBucket::new(Rate::OPENALEX),
                TokenBucket::new(Rate::SEMANTIC_SCHOLAR),
            )
        })
        .clone()
}

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

    #[tokio::test(start_paused = true)]
    async fn bucket_spaces_requests_after_burst() {
        let bucket = TokenBucket::new(Rate {
            per_second: 2.0,
            burst: 2,
        });
        let start = Instant::now();
        for _ in 0..4 {
            bucket.acquire().await;
        }
        let elapsed = start.elapsed();
        // Two from the burst, then two at 2/s.
        assert!(elapsed >= Duration::from_millis(990), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(1100), "{elapsed:?}");
    }

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
