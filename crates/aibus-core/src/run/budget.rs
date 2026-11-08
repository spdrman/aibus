//! The three limits that end a run (01 section 1.1): turns, tokens and money. A definition may
//! set its own; whatever it leaves out comes from `[limits]`.

use crate::config::Limits;

/// `max_turns`, `max_tokens_total` and `max_cost_usd`, already resolved for one run.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Budgets {
    pub max_turns: u32,
    /// Input plus output tokens.
    pub max_tokens_total: u64,
    pub max_cost_usd: f64,
}

impl From<&Limits> for Budgets {
    fn from(l: &Limits) -> Self {
        Self {
            max_turns: l.max_turns,
            max_tokens_total: l.max_tokens_total,
            max_cost_usd: l.max_cost_usd,
        }
    }
}

impl Default for Budgets {
    fn default() -> Self {
        (&Limits::default()).into()
    }
}
