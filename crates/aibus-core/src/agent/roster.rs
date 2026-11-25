//! The roster (01 section 1.5): every non-built-in definition's name and description. The same
//! text is counted against the 15,000 token budget at startup and embedded in the `delegate`
//! tool description.

use super::{AgentDef, Scope};
use crate::ids::AgentName;

/// Past this many tokens startup warns, and still loads everything.
pub const BUDGET_TOKENS: u64 = 15_000;

/// The most runs the roster suffix counts.
pub const RECENT_RUNS: u32 = 20;

/// One `- name: description` line per definition that is not a built-in, in the order given.
pub fn text<'a>(defs: impl IntoIterator<Item = &'a AgentDef>) -> String {
    text_with(defs, |_| None)
}

/// As `text`, with ` (last 18 runs: 15 completed)` after the description of an agent that
/// `recent` gives `(runs, completed)` for. The suffix is made from the two numbers alone, clamped
/// to `RECENT_RUNS`, so nothing a past run said can reach the lead through the roster.
pub fn text_with<'a>(
    defs: impl IntoIterator<Item = &'a AgentDef>,
    recent: impl Fn(&AgentName) -> Option<(u32, u32)>,
) -> String {
    let mut out = String::new();
    for d in defs.into_iter().filter(|d| d.source.scope != Scope::Builtin) {
        out.push_str(&format!("- {}: {}", d.name, d.description.trim()));
        if let Some((runs, completed)) = recent(&d.name).filter(|(runs, _)| *runs > 0) {
            let runs = runs.min(RECENT_RUNS);
            out.push_str(&format!(
                " (last {runs} runs: {} completed)",
                completed.min(runs)
            ));
        }
        out.push('\n');
    }
    out
}

/// The fallback when the lead's provider has no tokenizer: 4 characters per token, rounded up.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// The startup warning for a roster of `tokens`, if it is over budget.
pub fn warning(tokens: u64) -> Option<String> {
    (tokens > BUDGET_TOKENS).then(|| {
        format!(
            "agent roster is {tokens} tokens, over the {BUDGET_TOKENS} token budget; \
             shorten descriptions (detail belongs in the body)"
        )
    })
}
