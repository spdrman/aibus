//! The router, the fork layer (01 section 7.2): one short constrained completion per question,
//! its `p`, and the helpers the three question sites use.

use std::path::Path;
use std::sync::OnceLock;

use aibus_provider::{
    CacheHint, ChatRequest, ContentBlock, Message, MessageRole, RequestMetadata, ToolChoice,
};
use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::assist::{complete, RouterSeat};
use crate::msg::{PSource, RouterAnswer, RouterQuestion, Verdict};

/// A router answer is this long at most (the whole of a `{"choice": ..., "p": ...}` object).
pub const MAX_OUTPUT_TOKENS: u32 = 16;
/// What `retry_or_stop` offers.
pub const RETRY_CHOICES: [&str; 3] = ["retry once", "switch tool", "stop + ask"];
/// Candidate files a `which_file` question lists at most.
const MAX_CANDIDATES: usize = 8;
/// Files a candidate search looks into at most, and the largest it reads.
const MAX_FILES_SCANNED: usize = 5_000;
const MAX_FILE_BYTES: u64 = 512 * 1024;

const FORMAT: &str = "Answer with one JSON object and nothing else: \
{\"choice\": \"<one of the choices, copied exactly>\", \"p\": <your confidence, 0 to 1>}.";

/// Asks the router. `Err` is why there is no answer (the provider failed, or the reply was not
/// one of the choices); the caller carries on as if the router were off.
pub async fn ask(
    router: &RouterSeat,
    q: &RouterQuestion,
    cancel: &CancellationToken,
) -> Result<RouterAnswer, String> {
    let seat = &router.seat;
    let choices: String = q.choices.iter().map(|c| format!("\n- {c}")).collect();
    let prompt = format!(
        "question: {}\n{}\nchoices:{choices}",
        q.kind.as_str(),
        q.context
    );
    let req = ChatRequest {
        model: seat.model.clone(),
        system: format!("{}\n\n{FORMAT}", router.system),
        messages: vec![Message::text(MessageRole::User, &prompt)],
        tools: Vec::new(),
        tool_choice: ToolChoice::None,
        max_output_tokens: MAX_OUTPUT_TOKENS,
        temperature: Some(0.0),
        effort: None,
        thinking: None,
        cache_hint: CacheHint::None,
        logprobs: seat.provider.capabilities_for(&seat.model).logprobs,
        metadata: RequestMetadata::default(),
    };
    let began = tokio::time::Instant::now();
    let (resp, logprobs) = complete(seat, req, cancel).await.map_err(|e| e.message)?;
    let latency = began.elapsed();
    let text: String = resp
        .message
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    let (choice, raw, said) =
        parse(&text, &q.choices).ok_or_else(|| format!("not an answer: {text:?}"))?;
    let (p, source) = match joint_p(&raw, &logprobs) {
        Some(p) => (p, PSource::Logprob),
        None => (said.unwrap_or(0.0), PSource::SelfReport),
    };
    Ok(RouterAnswer {
        choice,
        p,
        verdict: if p >= router.threshold {
            Verdict::Sharp
        } else {
            Verdict::Split
        },
        latency,
        source,
    })
}

/// The choice (as the question spelled it), as the model wrote it, and its self-reported `p`.
/// A question with no choices takes any string; a number is a 1-based index into the choices.
fn parse(text: &str, choices: &[String]) -> Option<(String, String, Option<f32>)> {
    let json = text.get(text.find('{')?..=text.rfind('}')?)?;
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let raw = match v.get("choice")? {
        serde_json::Value::String(s) => s.trim().to_owned(),
        serde_json::Value::Number(n) => {
            let at = usize::try_from(n.as_u64()?).ok()?.checked_sub(1)?;
            choices.get(at)?.clone()
        }
        _ => return None,
    };
    let choice = if choices.is_empty() {
        raw.clone()
    } else {
        choices
            .iter()
            .find(|c| c.eq_ignore_ascii_case(&raw))?
            .clone()
    };
    let p = v.get("p").and_then(serde_json::Value::as_f64);
    Some((choice, raw, p.map(|p| p.clamp(0.0, 1.0) as f32)))
}

/// The probability of the choice itself: the product of the probabilities of the tokens that
/// spell it. `None` when the provider gave no logprobs, or none of them cover the choice.
fn joint_p(raw: &str, logprobs: &[(String, f32)]) -> Option<f32> {
    let whole: String = logprobs.iter().map(|(t, _)| t.as_str()).collect();
    // Skip the `"choice"` key if the tokens carry it.
    let from = whole.find("choice").map_or(0, |i| i + "choice".len());
    let start = from + whole.get(from..)?.find(raw)?;
    let end = start + raw.len();
    let (mut at, mut sum, mut any) = (0, 0.0_f32, false);
    for (token, logprob) in logprobs {
        let (s, e) = (at, at + token.len());
        at = e;
        if s < end && e > start {
            sum += logprob;
            any = true;
        }
    }
    any.then(|| sum.exp().clamp(0.0, 1.0))
}

fn regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a fixed pattern"))
}

/// True when the lead's text ends on something it means to do next ("Now I'll run the tests."),
/// as opposed to an answer.
pub fn ends_with_intention(text: &str) -> bool {
    static PHRASE: OnceLock<Regex> = OnceLock::new();
    let last = text.trim_end().lines().next_back().unwrap_or("").trim();
    if last.is_empty() || last.chars().count() > 300 {
        return false;
    }
    last.ends_with(':')
        || regex(
            &PHRASE,
            r"(?i)\b(i'll|i will|i'm going to|i am going to|let me|let's|next,? i|now i)\b",
        )
        .is_match(last)
}

/// The first symbol in `text` that appears in more than one file under `cwd`, and those files
/// (relative, sorted, at most eight). Symbols are backticked words, or identifiers that look
/// like code (`snake_case`, `CamelCase`). `None` when nothing is ambiguous.
pub fn which_file_candidates(cwd: &Path, text: &str) -> Option<(String, Vec<String>)> {
    static TICKED: OnceLock<Regex> = OnceLock::new();
    static WORD: OnceLock<Regex> = OnceLock::new();
    let mut symbols: Vec<String> = Vec::new();
    let ticked = regex(&TICKED, r"`([A-Za-z_][A-Za-z0-9_]{2,})`")
        .captures_iter(text)
        .map(|c| c[1].to_owned());
    let words = regex(&WORD, r"\b[A-Za-z_][A-Za-z0-9_]{3,}\b")
        .find_iter(text)
        .map(|m| m.as_str())
        .filter(|w| {
            w.contains('_')
                || (w.chars().any(|c| c.is_ascii_uppercase())
                    && w.chars().any(|c| c.is_ascii_lowercase()))
        })
        .map(str::to_owned);
    for s in ticked.chain(words) {
        if !symbols.contains(&s) {
            symbols.push(s);
        }
        if symbols.len() == 3 {
            break;
        }
    }
    if symbols.is_empty() {
        return None;
    }
    let patterns: Vec<(String, Regex)> = symbols
        .into_iter()
        .filter_map(|s| {
            Some((
                s.clone(),
                Regex::new(&format!(r"\b{}\b", regex::escape(&s))).ok()?,
            ))
        })
        .collect();
    let mut found: Vec<Vec<String>> = vec![Vec::new(); patterns.len()];
    let walk = ignore::WalkBuilder::new(cwd).max_depth(Some(8)).build();
    for entry in walk.filter_map(Result::ok).take(MAX_FILES_SCANNED) {
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(cwd)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        for ((_, re), files) in patterns.iter().zip(found.iter_mut()) {
            if re.is_match(&content) {
                files.push(rel.clone());
            }
        }
    }
    patterns
        .into_iter()
        .zip(found)
        .find_map(|((symbol, _), mut files)| {
            (files.len() > 1).then(|| {
                files.sort();
                files.truncate(MAX_CANDIDATES);
                (symbol, files)
            })
        })
}

/// The head of a file under `cwd` (at most `max` bytes, cut at a character), for pre-reading.
pub fn read_head(cwd: &Path, rel: &str, max: usize) -> Option<String> {
    let full = cwd.join(rel);
    let text = std::fs::read_to_string(full).ok()?;
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Some(text[..end].to_owned())
}

#[cfg(test)]
mod tests;
