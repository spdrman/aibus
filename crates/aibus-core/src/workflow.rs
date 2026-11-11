//! Workflows (01 section 8.4): a named chain of delegations in `.aibus/workflows/<name>.toml`.
//!
//! This file is the format and the chain logic: parsing, the `{{...}}` templates, where the files
//! live, and `execute`, which walks the steps against a `StepHost`. The host is what actually
//! starts a run and runs a gate; the `workflow` tool is the real one and a test brings its own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::ids::AgentName;
use crate::persist::fs::{create_private_dir, write_atomic};
use crate::persist::time::rfc3339;
use crate::run::RunStatus;

/// The four chains shipped as examples (`examples/workflows/`), installed by `aibus init`.
pub const EXAMPLES: [(&str, &str); 4] = [
    ("fix", include_str!("../../../examples/workflows/fix.toml")),
    ("build", include_str!("../../../examples/workflows/build.toml")),
    ("review", include_str!("../../../examples/workflows/review.toml")),
    ("explore", include_str!("../../../examples/workflows/explore.toml")),
];

/// The most a step may be retried; a larger `retry:N` is a mistake in the file.
const MAX_RETRIES: u32 = 5;

/// How much of a failed step's text is passed to the re-run as the reason.
const REASON_BYTES: usize = 2048;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    pub steps: Vec<Step>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Step {
    pub agent: AgentName,
    pub prompt: String,
    /// A hook-style command; exit 2 blocks the chain.
    pub gate: Option<String>,
    /// `on_fail = "retry:N"`: when this step fails, the step before it and this one run again,
    /// at most `N` times.
    pub retries: u32,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorkflowError {
    #[error("not valid TOML: {0}")]
    Toml(String),
    #[error("a workflow needs at least one [[step]]")]
    NoSteps,
    #[error("step {step}: {why}")]
    Step { step: usize, why: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "step")]
    steps: Vec<RawStep>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawStep {
    agent: String,
    prompt: String,
    gate: Option<String>,
    on_fail: Option<String>,
}

/// One `{{...}}` in a prompt.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Var {
    Input,
    Prev,
    Step(String),
}

fn var(key: &str) -> Option<Var> {
    match key {
        "input" => Some(Var::Input),
        "prev.summary" => Some(Var::Prev),
        _ => {
            let agent = key.strip_prefix("steps.")?.strip_suffix(".summary")?;
            AgentName::new(agent).ok().map(|a| Var::Step(a.to_string()))
        }
    }
}

/// Calls `f` with the text before each `{{key}}` and then the key; the last call has an empty key
/// for the tail. `Err` is a `{{` that never closes.
fn scan<'a>(template: &'a str, mut f: impl FnMut(&'a str, Option<&'a str>)) -> Result<(), ()> {
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let close = after.find("}}").ok_or(())?;
        f(&rest[..open], Some(after[..close].trim()));
        rest = &after[close + 2..];
    }
    f(rest, None);
    Ok(())
}

impl Workflow {
    /// Parses `text`; `stem` is the file name without `.toml` and is the name when the file has
    /// none. Every template is checked here, so a typo fails when the file is read and not in the
    /// middle of a chain.
    pub fn parse(stem: &str, text: &str) -> Result<Self, WorkflowError> {
        let file: File = toml::from_str(text).map_err(|e| WorkflowError::Toml(e.to_string()))?;
        if file.steps.is_empty() {
            return Err(WorkflowError::NoSteps);
        }
        let mut steps: Vec<Step> = Vec::new();
        for (i, raw) in file.steps.into_iter().enumerate() {
            let n = i + 1;
            let bad = |why: String| WorkflowError::Step { step: n, why };
            let agent = AgentName::new(&raw.agent).map_err(|e| bad(e.to_string()))?;
            if raw.prompt.trim().is_empty() {
                return Err(bad("the prompt is empty".into()));
            }
            let mut seen = Vec::new();
            scan(&raw.prompt, |_, key| seen.extend(key)).map_err(|()| bad("a `{{` is never closed".into()))?;
            for key in seen {
                match var(key) {
                    None => return Err(bad(format!("unknown template `{{{{{key}}}}}`"))),
                    Some(Var::Prev) if i == 0 => {
                        return Err(bad("the first step has no `{{prev.summary}}`".into()))
                    }
                    Some(Var::Step(a)) if !steps.iter().any(|s| s.agent.as_str() == a) => {
                        return Err(bad(format!("`{key}` names a step that does not come before this one")))
                    }
                    Some(_) => {}
                }
            }
            let retries = match raw.on_fail.as_deref() {
                None => 0,
                Some(v) => v
                    .strip_prefix("retry:")
                    .and_then(|n| n.trim().parse::<u32>().ok())
                    .filter(|n| (1..=MAX_RETRIES).contains(n))
                    .ok_or_else(|| bad(format!("on_fail {v:?}: want retry:N with N from 1 to {MAX_RETRIES}")))?,
            };
            if raw.gate.as_deref().is_some_and(|g| g.trim().is_empty()) {
                return Err(bad("the gate is empty".into()));
            }
            steps.push(Step {
                agent,
                prompt: raw.prompt,
                gate: raw.gate,
                retries,
            });
        }
        Ok(Self {
            name: file.name.unwrap_or_else(|| stem.to_owned()),
            description: file.description,
            steps,
        })
    }
}

/// What `validate` found. `ok` is true when there are no errors; warnings never change it.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize)]
pub struct Validation {
    pub ok: bool,
    pub errors: Vec<Problem>,
    pub warnings: Vec<Problem>,
}

/// One finding: the 1-based step it is about (`None` for the file as a whole), the key in that
/// step when there is one, and the sentence for the author.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Problem {
    pub step: Option<usize>,
    pub field: Option<String>,
    pub message: String,
}

/// The shapes the lead's transcript uses for its own markers (09 section 6); a prompt that
/// contains one can be mistaken for the real thing when it comes back in a summary.
const MARKERS: [&str; 6] = ["[hint]", "[run ", "[mcp ", "[hook ", "[web ", "[advisor]"];

/// Everything `parse` rejects, plus the agents that `roster` does not have, and the warnings:
/// a gate (it runs a shell command, so the text is quoted), a summary no later step reads, a
/// prompt with the shape of an aibus marker, and an `on_fail` that will not do what it looks like.
/// The same function backs `aibus workflow validate` and the MCP tools.
pub fn validate(text: &str, stem: &str, roster: &[AgentName]) -> Validation {
    let mut out = Validation::default();
    let mut error = |step: Option<usize>, field: Option<&str>, message: String| {
        out.errors.push(Problem { step, field: field.map(str::to_owned), message });
    };
    match Workflow::parse(stem, text) {
        Ok(_) => {}
        Err(WorkflowError::Step { step, why }) => error(Some(step), None, why),
        Err(e) => error(None, None, e.to_string()),
    }
    // `parse` stops at the first bad step; the roster and the warnings look at every step that
    // reads as TOML, so one pass shows the author everything.
    let Ok(file) = toml::from_str::<File>(text) else {
        out.ok = out.errors.is_empty();
        return out;
    };
    let mut warn = Vec::new();
    for (i, raw) in file.steps.iter().enumerate() {
        let n = i + 1;
        let mut note = |field: Option<&str>, message: String| {
            warn.push(Problem { step: Some(n), field: field.map(str::to_owned), message });
        };
        if AgentName::new(&raw.agent).is_ok_and(|a| !roster.contains(&a)) {
            out.errors.push(Problem {
                step: Some(n),
                field: Some("agent".into()),
                message: format!("no agent named `{}` in the roster", raw.agent),
            });
        }
        if let Some(gate) = raw.gate.as_ref().filter(|g| !g.trim().is_empty()) {
            note(Some("gate"), format!("runs a shell command after this step: {gate}"));
        }
        // The last step's summary is the workflow's result, so nobody else has to read it.
        let read = |j: usize| {
            let mut keys = Vec::new();
            let _ = scan(&file.steps[j].prompt, |_, key| keys.extend(key));
            let named = format!("steps.{}.summary", raw.agent);
            // A step of the same name in between hides this one from `steps.<agent>.summary`.
            let hidden = file.steps[i + 1..j].iter().any(|s| s.agent == raw.agent);
            keys.iter().any(|k| (j == i + 1 && *k == "prev.summary") || (*k == named && !hidden))
        };
        if n < file.steps.len() && !(i + 1..file.steps.len()).any(read) {
            note(
                None,
                format!(
                    "nothing after this step reads its summary ({{{{prev.summary}}}} or {{{{steps.{}.summary}}}})",
                    raw.agent
                ),
            );
        }
        for m in MARKERS.iter().filter(|m| raw.prompt.contains(**m)) {
            note(
                Some("prompt"),
                format!("the prompt contains `{}`, which has the shape of an aibus marker", m.trim_end()),
            );
        }
        if raw.on_fail.is_some() {
            if n < file.steps.len() {
                note(Some("on_fail"), "on_fail is set on a step that is not the last one".into());
            }
            if i > 0 && file.steps[i - 1].gate.is_some() {
                note(
                    Some("on_fail"),
                    "on_fail is set and the step before it has a gate, so a retry runs that gate again".into(),
                );
            }
        }
    }
    out.ok = out.errors.is_empty();
    out.errors.sort_by_key(|p| p.step);
    out.warnings = warn;
    out
}

/// Whether `text` validates, and the line that says so under a shown or drafted workflow:
/// `valid, 3 steps, 1 warning` or `2 errors, 0 warnings`.
pub fn summarise(text: &str, stem: &str, roster: &[AgentName]) -> (bool, String) {
    let found = validate(text, stem, roster);
    let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    if found.ok {
        let steps = Workflow::parse(stem, text).map_or(0, |w| w.steps.len());
        (true, format!("valid, {}, {}", plural(steps, "step"), plural(found.warnings.len(), "warning")))
    } else {
        (false, format!("{}, {}", plural(found.errors.len(), "error"), plural(found.warnings.len(), "warning")))
    }
}

/// Where workflows are looked for, project first: `<project>/.aibus/workflows` and then
/// `<config>/workflows`. A name in the project hides the same name in the user's directory.
pub fn dirs(project: &Path, config: Option<&Path>) -> Vec<PathBuf> {
    std::iter::once(crate::persist::paths::project_dir(project).join("workflows"))
        .chain(config.map(|c| c.join("workflows")))
        .collect()
}

/// One workflow file: its name, and what reading it gave.
pub type Found = (String, PathBuf, Result<Workflow, WorkflowError>);

/// Every `*.toml` in `dirs`, by name; the first directory to have a name wins.
pub fn list(dirs: &[PathBuf]) -> Vec<Found> {
    let mut by_name: BTreeMap<String, Found> = BTreeMap::new();
    for dir in dirs {
        let Ok(read) = std::fs::read_dir(dir) else { continue };
        for entry in read.flatten() {
            let path = entry.path();
            let Some(stem) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|_| path.extension().is_some_and(|x| x == "toml"))
            else {
                continue;
            };
            by_name.entry(stem.to_owned()).or_insert_with(|| {
                let parsed = std::fs::read_to_string(&path)
                    .map_err(|e| WorkflowError::Toml(e.to_string()))
                    .and_then(|t| Workflow::parse(stem, &t));
                (stem.to_owned(), path.clone(), parsed)
            });
        }
    }
    by_name.into_values().collect()
}

/// The workflow called `name`. `Err` is what to tell the user.
pub fn load(dirs: &[PathBuf], name: &str) -> Result<Workflow, String> {
    match list(dirs).into_iter().find(|(n, _, _)| n == name) {
        None => Err(format!("no workflow named {name} (aibus workflow list)")),
        Some((_, path, Err(e))) => Err(format!("{}: {e}", path.display())),
        Some((_, _, Ok(w))) => Ok(w),
    }
}

/// `^[a-z0-9][a-z0-9_-]{0,63}$`. A name is the file stem, so this is also what keeps a path
/// separator, a dot, `~` and a leading dash out of a path (and out of the `aibus run` command
/// line). `save` and the MCP write tools both check it.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The first line of a file `save` writes.
pub const SCHEMA_LINE: &str = "# schema: aibus://schema/workflow v1";

/// The start of the second line, which `save` ends with an RFC 3339 time. The MCP write tools
/// write `# written through aibus mcp, ` and `save` takes either off a draft before adding its own.
pub const PROVENANCE: &str = "# written through aibus, ";

/// One row of `/workflow list`: what a person needs to pick a workflow.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkflowRow {
    pub name: String,
    /// `project` or `user`.
    pub scope: String,
    pub description: String,
    /// The agent of each step, in order. Empty for a file that does not parse.
    pub agents: Vec<String>,
    /// Why the file does not parse.
    pub error: Option<String>,
}

/// The rows of `dirs` (as `list` finds them); the first directory is the project's.
pub fn rows(dirs: &[PathBuf]) -> Vec<WorkflowRow> {
    list(dirs)
        .into_iter()
        .map(|(name, path, parsed)| {
            let scope = if dirs.first().is_some_and(|d| path.starts_with(d)) { "project" } else { "user" };
            let (description, agents, error) = match parsed {
                Ok(w) => (w.description, w.steps.iter().map(|s| s.agent.to_string()).collect(), None),
                Err(e) => (String::new(), Vec::new(), Some(e.to_string())),
            };
            WorkflowRow { name, scope: scope.into(), description, agents, error }
        })
        .collect()
}

/// Why `save` wrote nothing.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SaveError {
    #[error("`{0}` is not a workflow name: lowercase letters, digits, - and _, starting with a letter or digit, 64 at most")]
    Name(String),
    #[error("the draft does not validate")]
    Invalid(Vec<Problem>),
    #[error("{} exists, save it again with --force to replace it", .0.display())]
    Exists(PathBuf),
    #[error("{0}")]
    Io(String),
}

/// Writes `text` as workflow `name` into the project's workflows directory, or the user's with
/// `user`. Nothing touches the disk until the name and the draft have passed, and the path is the
/// directory plus the validated name, never the argument. The file starts with `SCHEMA_LINE` and
/// a `PROVENANCE` line made from `at` (a header the draft already had is replaced, not doubled),
/// and is written atomically with mode 0600. An existing file is only replaced with `force`.
pub fn save(
    dirs: &[PathBuf],
    user: bool,
    name: &str,
    text: &str,
    force: bool,
    roster: &[AgentName],
    at: SystemTime,
) -> Result<PathBuf, SaveError> {
    if !valid_name(name) {
        return Err(SaveError::Name(name.to_owned()));
    }
    let found = validate(text, name, roster);
    if !found.ok {
        return Err(SaveError::Invalid(found.errors));
    }
    let dir = dirs.get(usize::from(user)).ok_or_else(|| SaveError::Io("there is no user directory for workflows".into()))?;
    let path = dir.join(format!("{name}.toml"));
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(SaveError::Io(format!("{}: {e}", path.display()))),
        Ok(md) if md.file_type().is_symlink() || !md.is_file() => {
            return Err(SaveError::Io(format!("{} is not a plain file, refusing to write through it", path.display())))
        }
        Ok(_) if !force => return Err(SaveError::Exists(path)),
        Ok(_) => {}
    }
    create_private_dir(dir).map_err(|e| SaveError::Io(format!("{}: {e}", dir.display())))?;
    let body: Vec<&str> = text
        .lines()
        .skip_while(|l| l.starts_with("# schema: ") || l.starts_with("# written through aibus"))
        .collect();
    let bytes = format!("{SCHEMA_LINE}\n{PROVENANCE}{}\n{}\n", rfc3339(at), body.join("\n"));
    write_atomic(&path, bytes).map_err(|e| SaveError::Io(format!("{}: {e}", path.display())))?;
    Ok(path)
}

/// `/workflow <name> <input>` or, for a name in `known`, `/<name> <input>`: the name and the
/// input. `None` for anything else, so an ordinary message (or a path) is left alone.
pub fn parse_slash<'a>(text: &'a str, known: &[String]) -> Option<(&'a str, &'a str)> {
    let body = text.trim_start().strip_prefix('/')?;
    let (word, rest) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
    if word == "workflow" {
        let rest = rest.trim_start();
        let (name, input) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        return (!name.is_empty()).then_some((name, input.trim()));
    }
    known.iter().any(|k| k == word).then_some((word, rest.trim()))
}

/// What the templates of one step can see.
struct Scope<'a> {
    input: &'a str,
    prev: Option<&'a str>,
    steps: &'a BTreeMap<String, String>,
}

fn render(template: &str, scope: &Scope<'_>) -> String {
    let mut out = String::new();
    // The template was checked when it was parsed, so every key resolves.
    let _ = scan(template, |text, key| {
        out.push_str(text);
        match key.and_then(var) {
            Some(Var::Input) => out.push_str(scope.input),
            Some(Var::Prev) => out.push_str(scope.prev.unwrap_or_default()),
            Some(Var::Step(a)) => out.push_str(scope.steps.get(&a).map_or("", String::as_str)),
            None => {}
        }
    });
    out
}

/// What the prompt of one step reads, in the order it names them and each once: `input`,
/// `prev.summary` or `steps.<agent>.summary`. Both MCP's `workflow_explain` and `workflow_dry_run`
/// need the template syntax and only this module may know it.
pub fn reads(prompt: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let _ = scan(prompt, |_, key| {
        if let Some(k) = key.filter(|k| var(k).is_some()) {
            if !keys.iter().any(|seen| seen == k) {
                keys.push(k.to_owned());
            }
        }
    });
    keys
}

/// Every prompt of `wf` rendered for `input`, with `<summary of step N (agent)>` where a later
/// step would read an earlier one's summary. Nothing runs.
pub fn preview(wf: &Workflow, input: &str) -> Vec<String> {
    let mut steps: BTreeMap<String, String> = BTreeMap::new();
    let mut prev: Option<String> = None;
    let mut out = Vec::new();
    for (i, step) in wf.steps.iter().enumerate() {
        out.push(render(&step.prompt, &Scope { input, prev: prev.as_deref(), steps: &steps }));
        let placeholder = format!("<summary of step {} ({})>", i + 1, step.agent);
        steps.insert(step.agent.to_string(), placeholder.clone());
        prev = Some(placeholder);
    }
    out
}

/// What a finished step handed back.
#[derive(Clone, PartialEq, Debug)]
pub struct StepOutcome {
    pub status: RunStatus,
    pub summary: String,
    /// What the lead is shown: the summary under its wrapper (01 section 5.3).
    pub wrapped: String,
    /// Why a run that did not complete stopped.
    pub error: Option<String>,
}

/// Why a step could not be run at all.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HostError {
    /// The run could not be started (a limit, an agent that is gone). A failure like any other.
    Failed(String),
    /// The chain ends here without a retry: the step moved to the background.
    Stop(String),
    /// The lead's call was cancelled.
    Cancelled,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum GateVerdict {
    Pass,
    Block(String),
}

/// What a gate is told, besides the workflow's own words.
pub struct GateCall<'a> {
    pub workflow: &'a str,
    pub step: usize,
    pub agent: &'a AgentName,
    pub input: &'a str,
    pub outcome: &'a StepOutcome,
}

#[async_trait::async_trait]
pub trait StepHost: Send + Sync {
    /// Runs `agent` in the foreground under the lead and waits for its result.
    async fn run(&self, agent: &AgentName, prompt: String) -> Result<StepOutcome, HostError>;
    /// Runs the step's gate command with hook semantics.
    async fn gate(&self, command: &str, call: &GateCall<'_>) -> Result<GateVerdict, HostError>;
}

/// One run of one step, kept for the report.
#[derive(Clone, PartialEq, Debug)]
pub struct Attempt {
    /// 1-based position in the file.
    pub step: usize,
    pub agent: AgentName,
    pub status: Option<RunStatus>,
    pub wrapped: String,
    /// Why this attempt failed (a status, a gate, a run that could not start).
    pub failure: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Ending {
    Completed,
    Stopped { step: usize, reason: String },
    Cancelled,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Report {
    pub attempts: Vec<Attempt>,
    pub ending: Ending,
}

fn tail(text: &str, max: usize) -> &str {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Walks the chain. A step fails when its run does not complete or its gate blocks. A failing step
/// with `retries` left sends the chain back one step, so the pair runs again with the reason
/// added to both prompts; one without stops the chain. A cancel ends it at once.
pub async fn execute(wf: &Workflow, input: &str, host: &dyn StepHost) -> Report {
    let mut attempts = Vec::new();
    let mut summaries: BTreeMap<String, String> = BTreeMap::new();
    let mut used = vec![0u32; wf.steps.len()];
    // Why the last attempt failed, and the step whose re-run ends the note.
    let mut note: Option<(String, usize)> = None;
    let mut i = 0;
    while i < wf.steps.len() {
        let step = &wf.steps[i];
        let prev = i.checked_sub(1).and_then(|p| summaries.get(wf.steps[p].agent.as_str()));
        let mut prompt = render(
            &step.prompt,
            &Scope {
                input,
                prev: prev.map(String::as_str),
                steps: &summaries,
            },
        );
        if let Some((why, _)) = &note {
            prompt.push_str(&format!("\n\nThe previous attempt did not pass: {why}"));
        }
        let mut attempt = Attempt {
            step: i + 1,
            agent: step.agent.clone(),
            status: None,
            wrapped: String::new(),
            failure: None,
        };
        let ran = host.run(&step.agent, prompt).await;
        let mut failure = match ran {
            Err(HostError::Cancelled) => {
                attempt.failure = Some("cancelled".into());
                attempts.push(attempt);
                return Report { attempts, ending: Ending::Cancelled };
            }
            Err(HostError::Stop(reason)) => {
                attempt.failure = Some(reason.clone());
                attempts.push(attempt);
                return Report { attempts, ending: Ending::Stopped { step: i + 1, reason } };
            }
            Err(HostError::Failed(reason)) => Some(reason),
            Ok(out) => {
                attempt.status = Some(out.status);
                attempt.wrapped.clone_from(&out.wrapped);
                summaries.insert(step.agent.to_string(), out.summary.clone());
                if out.status == RunStatus::Cancelled {
                    attempt.failure = Some("cancelled".into());
                    attempts.push(attempt);
                    return Report { attempts, ending: Ending::Cancelled };
                }
                if out.status != RunStatus::Completed {
                    let detail = out.error.as_deref().unwrap_or(tail(&out.summary, REASON_BYTES));
                    Some(format!("{} ended {}: {detail}", step.agent, out.status.as_value()))
                } else if let Some(command) = &step.gate {
                    let call = GateCall { workflow: &wf.name, step: i + 1, agent: &step.agent, input, outcome: &out };
                    match host.gate(command, &call).await {
                        Ok(GateVerdict::Pass) => None,
                        Ok(GateVerdict::Block(reason)) => Some(format!("gate blocked: {reason}")),
                        Err(HostError::Cancelled) => {
                            attempt.failure = Some("cancelled".into());
                            attempts.push(attempt);
                            return Report { attempts, ending: Ending::Cancelled };
                        }
                        Err(HostError::Stop(reason) | HostError::Failed(reason)) => {
                            Some(format!("gate could not run: {reason}"))
                        }
                    }
                } else {
                    None
                }
            }
        };
        attempt.failure = failure.clone();
        attempts.push(attempt);
        match failure.take() {
            None => {
                // The pair is done once the step that failed has passed.
                if note.as_ref().is_some_and(|(_, last)| i >= *last) {
                    note = None;
                }
                i += 1;
            }
            Some(reason) if used[i] < step.retries => {
                used[i] += 1;
                note = Some((tail(&reason, REASON_BYTES).to_owned(), i));
                i = i.saturating_sub(1);
            }
            Some(reason) => {
                return Report { attempts, ending: Ending::Stopped { step: i + 1, reason } };
            }
        }
    }
    Report { attempts, ending: Ending::Completed }
}

impl Report {
    /// What the lead reads: one line for the ending, then every attempt's wrapped result.
    pub fn text(&self, wf: &Workflow) -> String {
        let steps = wf.steps.len();
        let mut out = match &self.ending {
            Ending::Completed => format!("workflow {}: completed, {steps} steps", wf.name),
            Ending::Cancelled => format!("workflow {}: cancelled", wf.name),
            Ending::Stopped { step, reason } => {
                let agent = &wf.steps[step - 1].agent;
                let left: Vec<&str> = wf.steps[*step..].iter().map(|s| s.agent.as_str()).collect();
                let rest = if left.is_empty() {
                    String::new()
                } else {
                    format!(" Not run: {}.", left.join(", "))
                };
                format!("workflow {}: stopped at step {step} of {steps} ({agent}): {reason}.{rest}", wf.name)
            }
        };
        for a in &self.attempts {
            out.push_str(&format!("\n\n--- step {} ({})", a.step, a.agent));
            if let Some(f) = &a.failure {
                out.push_str(&format!(", failed: {f}"));
            }
            out.push_str(" ---\n");
            if a.wrapped.is_empty() {
                out.push_str("(no result)");
            } else {
                out.push_str(&a.wrapped);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const FIX: &str = r#"
name = "fix"
description = "Diagnose, assess risk, fix, verify, commit"

[[step]]
agent = "sleuth"
prompt = "Diagnose: {{input}}"

[[step]]
agent = "premortem"
prompt = "Risks of this fix: {{prev.summary}}"
gate = "no_high_risk"

[[step]]
agent = "kraken"
prompt = "Implement: {{steps.sleuth.summary}}"

[[step]]
agent = "arbiter"
prompt = "Verify the fix with tests."
on_fail = "retry:2"
"#;

    #[test]
    fn the_example_of_the_spec_parses() {
        let wf = Workflow::parse("fix", FIX).unwrap();
        assert_eq!(wf.name, "fix");
        assert_eq!(wf.steps.len(), 4);
        assert_eq!(wf.steps[1].gate.as_deref(), Some("no_high_risk"));
        assert_eq!(wf.steps[3].retries, 2);
        assert_eq!(wf.steps[0].retries, 0);
    }

    #[test]
    fn mistakes_are_named_with_the_step() {
        let bad = |t: &str| Workflow::parse("x", t).unwrap_err().to_string();
        assert_eq!(bad("name = \"x\""), "a workflow needs at least one [[step]]");
        assert!(bad("[[step]]\nagent = \"A B\"\nprompt = \"p\"").starts_with("step 1: invalid agent name"));
        assert!(bad("[[step]]\nagent = \"a\"\nprompt = \"{{nope}}\"").contains("unknown template `{{nope}}`"));
        assert!(bad("[[step]]\nagent = \"a\"\nprompt = \"{{prev.summary}}\"").contains("first step"));
        assert!(bad("[[step]]\nagent = \"a\"\nprompt = \"{{steps.b.summary}}\"").contains("does not come before"));
        assert!(bad("[[step]]\nagent = \"a\"\nprompt = \"p\"\non_fail = \"retry:9\"").contains("retry:N"));
        assert!(bad("[[step]]\nagent = \"a\"\nprompt = \"p\"\non_fail = \"again\"").contains("retry:N"));
        assert!(bad("[[step]]\nagent = \"a\"\nprompt = \"{{input\"").contains("never closed"));
        assert!(bad("[[step]]\nagent = \"a\"\nprompt = \"p\"\nextra = 1").contains("unknown field"));
    }

    #[test]
    fn templates_fill_in_input_prev_and_named_steps() {
        let steps = BTreeMap::from([("a".to_owned(), "AAA".to_owned())]);
        let scope = Scope { input: "I", prev: Some("P"), steps: &steps };
        assert_eq!(render("{{input}}|{{ prev.summary }}|{{steps.a.summary}}|{{steps.z.summary}}.", &scope), "I|P|AAA|.");
    }

    #[test]
    fn slash_commands_need_a_known_name_or_the_workflow_word() {
        let known = vec!["fix".to_owned()];
        assert_eq!(parse_slash("/fix the login", &known), Some(("fix", "the login")));
        assert_eq!(parse_slash("/workflow build add a flag", &known), Some(("build", "add a flag")));
        assert_eq!(parse_slash("/fixme please", &known), None);
        assert_eq!(parse_slash("/etc/passwd explain", &known), None);
        assert_eq!(parse_slash("fix it", &known), None);
        assert_eq!(parse_slash("/workflow", &known), None);
    }

    #[test]
    fn a_saved_file_round_trips_with_its_comment_lines() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = vec![dir.path().join("project"), dir.path().join("user")];
        let text = EXAMPLES[2].1;
        let roster: Vec<AgentName> = Workflow::parse("review", text).unwrap().steps.iter().map(|s| s.agent.clone()).collect();
        let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let path = save(&dirs, false, "review", text, false, &roster, at).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        let mut lines = saved.lines();
        assert_eq!(lines.next(), Some(SCHEMA_LINE));
        assert_eq!(lines.next(), Some("# written through aibus, 2023-11-14T22:13:20.000Z"));
        assert_eq!(Workflow::parse("review", &saved), Workflow::parse("review", text));
        // Saving what was saved does not stack a second header.
        assert_eq!(save(&dirs, false, "review", &saved, false, &roster, at), Err(SaveError::Exists(path.clone())));
        save(&dirs, false, "review", &saved, true, &roster, at).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
        let scopes: Vec<_> = rows(&dirs).into_iter().map(|r| (r.name, r.scope)).collect();
        assert_eq!(scopes, [("review".to_owned(), "project".to_owned())]);
    }

    #[test]
    fn a_bad_name_or_draft_is_refused_before_the_disk_is_touched() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = vec![dir.path().join("project")];
        let at = SystemTime::UNIX_EPOCH;
        let long = "a".repeat(65);
        for name in ["", "../x", "A", "-x", "a.b", long.as_str()] {
            assert_eq!(save(&dirs, false, name, EXAMPLES[2].1, true, &[], at), Err(SaveError::Name(name.to_owned())));
        }
        assert!(matches!(save(&dirs, false, "ok", "nonsense", true, &[], at), Err(SaveError::Invalid(_))));
        assert!(!dirs[0].exists());
    }

    #[test]
    fn the_shipped_examples_parse() {
        for (name, text) in EXAMPLES {
            let wf = Workflow::parse(name, text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(wf.name, name);
        }
    }

    /// A host that answers from a script: (status, summary) per run, gate answers per call.
    struct Scripted {
        runs: Mutex<Vec<(RunStatus, String)>>,
        gates: Mutex<Vec<GateVerdict>>,
        prompts: Mutex<Vec<(String, String)>>,
    }

    impl Scripted {
        fn new(runs: Vec<(RunStatus, &str)>, gates: Vec<GateVerdict>) -> Self {
            Self {
                runs: Mutex::new(runs.into_iter().rev().map(|(s, t)| (s, t.to_owned())).collect()),
                gates: Mutex::new(gates.into_iter().rev().collect()),
                prompts: Mutex::default(),
            }
        }
    }

    #[async_trait::async_trait]
    impl StepHost for Scripted {
        async fn run(&self, agent: &AgentName, prompt: String) -> Result<StepOutcome, HostError> {
            self.prompts.lock().unwrap().push((agent.to_string(), prompt));
            let (status, summary) = self.runs.lock().unwrap().pop().expect("a scripted run");
            Ok(StepOutcome { wrapped: format!("[{agent}] {summary}"), status, summary, error: None })
        }

        async fn gate(&self, _: &str, _: &GateCall<'_>) -> Result<GateVerdict, HostError> {
            Ok(self.gates.lock().unwrap().pop().expect("a scripted gate"))
        }
    }

    fn ok(t: &str) -> (RunStatus, &str) {
        (RunStatus::Completed, t)
    }

    #[tokio::test]
    async fn a_chain_runs_in_order_with_each_step_seeing_the_ones_before() {
        let wf = Workflow::parse("fix", FIX).unwrap();
        let host = Scripted::new(vec![ok("cause"), ok("low"), ok("patched"), ok("green")], vec![GateVerdict::Pass]);
        let report = execute(&wf, "login fails", &host).await;
        assert_eq!(report.ending, Ending::Completed);
        let prompts = host.prompts.lock().unwrap().clone();
        assert_eq!(
            prompts,
            [
                ("sleuth".to_owned(), "Diagnose: login fails".to_owned()),
                ("premortem".to_owned(), "Risks of this fix: cause".to_owned()),
                ("kraken".to_owned(), "Implement: cause".to_owned()),
                ("arbiter".to_owned(), "Verify the fix with tests.".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn a_gate_that_blocks_stops_the_chain_and_names_what_was_not_run() {
        let wf = Workflow::parse("fix", FIX).unwrap();
        let host = Scripted::new(vec![ok("cause"), ok("high risk")], vec![GateVerdict::Block("high risk".into())]);
        let report = execute(&wf, "x", &host).await;
        assert_eq!(report.attempts.len(), 2);
        let Ending::Stopped { step, reason } = &report.ending else { panic!("{:?}", report.ending) };
        assert_eq!((*step, reason.as_str()), (2, "gate blocked: high risk"));
        let text = report.text(&wf);
        assert!(text.contains("stopped at step 2 of 4 (premortem)"), "{text}");
        assert!(text.contains("Not run: kraken, arbiter."), "{text}");
    }

    #[tokio::test]
    async fn on_fail_reruns_the_pair_at_most_the_number_of_times_it_says() {
        let wf = Workflow::parse("fix", FIX).unwrap();
        let failed = (RunStatus::Failed, "tests red");
        let host = Scripted::new(
            vec![ok("cause"), ok("low"), ok("p1"), failed, ok("p2"), failed, ok("p3"), failed],
            vec![GateVerdict::Pass],
        );
        let report = execute(&wf, "x", &host).await;
        // kraken and arbiter ran three times each: once, and twice again.
        let agents: Vec<_> = report.attempts.iter().map(|a| a.agent.to_string()).collect();
        assert_eq!(agents.iter().filter(|a| *a == "kraken").count(), 3, "{agents:?}");
        assert_eq!(agents.iter().filter(|a| *a == "arbiter").count(), 3, "{agents:?}");
        assert!(matches!(report.ending, Ending::Stopped { step: 4, .. }), "{:?}", report.ending);
        // The re-run was told why.
        let prompts = host.prompts.lock().unwrap().clone();
        assert!(prompts[4].1.contains("did not pass: arbiter ended failed: tests red"), "{prompts:?}");
        assert!(prompts[5].1.contains("did not pass"), "{prompts:?}");
        // The first pass saw no such note.
        assert!(!prompts[2].1.contains("did not pass"));
    }

    #[tokio::test]
    async fn a_retry_that_works_carries_on_to_the_end() {
        let wf = Workflow::parse("fix", FIX).unwrap();
        let host = Scripted::new(
            vec![ok("cause"), ok("low"), ok("p1"), (RunStatus::Failed, "red"), ok("p2"), ok("green")],
            vec![GateVerdict::Pass],
        );
        let report = execute(&wf, "x", &host).await;
        assert_eq!(report.ending, Ending::Completed);
        assert_eq!(report.attempts.len(), 6);
    }

    #[tokio::test]
    async fn a_step_without_on_fail_stops_the_chain_when_it_fails() {
        let wf = Workflow::parse("fix", FIX).unwrap();
        let host = Scripted::new(vec![(RunStatus::Partial, "half")], vec![]);
        let report = execute(&wf, "x", &host).await;
        assert!(matches!(report.ending, Ending::Stopped { step: 1, .. }));
        assert_eq!(report.attempts.len(), 1);
    }

    #[tokio::test]
    async fn a_cancelled_step_ends_the_chain_with_no_retry() {
        let wf = Workflow::parse("fix", FIX).unwrap();
        let host = Scripted::new(vec![(RunStatus::Cancelled, "")], vec![]);
        assert_eq!(execute(&wf, "x", &host).await.ending, Ending::Cancelled);
    }
}
