//! The built-in model tables, by the name a preset gives them (`model_table` in `presets.json`).
//! The files live in `catalog/` at the repo root and nowhere else; `catalog-sync` refreshes them.

/// The table text for `name`: a built-in one, or `empty` for a server that lists its own models.
pub fn table(name: &str) -> Option<&'static str> {
    Some(match name {
        "anthropic" => include_str!("../../../catalog/anthropic.json"),
        "cerebras" => include_str!("../../../catalog/cerebras.json"),
        "deepinfra" => include_str!("../../../catalog/deepinfra.json"),
        "deepseek" => include_str!("../../../catalog/deepseek.json"),
        "fireworks" => include_str!("../../../catalog/fireworks.json"),
        "gemini" => include_str!("../../../catalog/gemini.json"),
        "gemini-openai" => include_str!("../../../catalog/gemini-openai.json"),
        "groq" => include_str!("../../../catalog/groq.json"),
        "kimi" => include_str!("../../../catalog/kimi.json"),
        "mistral" => include_str!("../../../catalog/mistral.json"),
        "openai" => include_str!("../../../catalog/openai.json"),
        "openrouter" => include_str!("../../../catalog/openrouter.json"),
        "perplexity" => include_str!("../../../catalog/perplexity.json"),
        "together" => include_str!("../../../catalog/together.json"),
        "xai" => include_str!("../../../catalog/xai.json"),
        "empty" => r#"{"models": []}"#,
        _ => return None,
    })
}
