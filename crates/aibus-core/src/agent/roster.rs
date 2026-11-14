//! The roster (01 section 1.5): every non-built-in definition's name and description. The same
//! text is counted against the 15,000 token budget at startup and embedded in the `delegate`
//! tool description.

use super::{AgentDef, Scope};

/// Past this many tokens startup warns, and still loads everything.
pub const BUDGET_TOKENS: u64 = 15_000;

/// One `- name: description` line per definition that is not a built-in, in the order given.
pub fn text<'a>(defs: impl IntoIterator<Item = &'a AgentDef>) -> String {
    let mut out = String::new();
    for d in defs
        .into_iter()
        .filter(|d| d.source.scope != Scope::Builtin)
    {
        out.push_str(&format!("- {}: {}\n", d.name, d.description.trim()));
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
