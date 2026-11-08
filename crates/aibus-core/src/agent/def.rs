//! Parsing one agent definition (01 section 1.1).
//!
//! The frontmatter is read with `yaml-rust2` into a JSON map, so `--agents '<json>'` and a
//! Markdown file go through the same field handling. Keys are normalised to snake_case first,
//! which is how the Claude Code camelCase spellings are accepted.

use std::collections::BTreeMap;

use aibus_provider::{AccountName, ModelId};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use smol_str::SmolStr;
use yaml_rust2::{Yaml, YamlLoader};

use super::{
    AgentDef, CheckpointSpec, DefSource, Effort, HookSet, HookSpec, ModelRef, PermissionMode, Role,
    RouterSpec, ToolPattern, ToolPolicy,
};
use crate::ids::{AgentName, InvalidAgentName};

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum DefError {
    #[error("no frontmatter: the file must start with a `---` line and close it with another")]
    NoFrontmatter,
    #[error("frontmatter is not valid YAML: {0}")]
    Yaml(String),
    #[error("frontmatter is not a mapping")]
    NotAMapping,
    #[error("missing required field `{0}`")]
    Missing(&'static str),
    #[error("field `{field}`: expected {want}")]
    Field { field: String, want: &'static str },
    #[error(transparent)]
    Name(#[from] InvalidAgentName),
    #[error("--agents: {0}")]
    Cli(String),
}

const ADVISOR_CHECKPOINTS: [&str; 3] = ["before_plan", "same_error_x2", "before_done"];
const DEFAULT_ROUTER_THRESHOLD: f32 = 0.85;
const MAX_SKILLS: usize = 32;

/// Parse a definition file: YAML frontmatter, then the system prompt. Returns the definition
/// and its warnings (unknown keys and similar); errors are for anything that stops it loading.
pub fn parse(text: &str, source: DefSource) -> Result<(AgentDef, Vec<String>), DefError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut offset = 0;
    let mut split = None;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let fence = line.trim_end() == "---";
        match (i, fence) {
            (0, false) => return Err(DefError::NoFrontmatter),
            (0, true) => {}
            (_, true) => {
                split = Some((
                    &text[text.find('\n').map_or(0, |n| n + 1)..offset],
                    &text[offset + line.len()..],
                ));
                break;
            }
            _ => {}
        }
        offset += line.len();
    }
    let (yaml, body) = split.ok_or(DefError::NoFrontmatter)?;

    let docs = YamlLoader::load_from_str(yaml).map_err(|e| DefError::Yaml(e.to_string()))?;
    let fields = match docs.first() {
        None | Some(Yaml::Null) => Map::new(),
        Some(y) => match yaml_to_json(y)? {
            Value::Object(m) => m,
            _ => return Err(DefError::NotAMapping),
        },
    };
    from_fields(fields, body, source)
}

/// Build a definition from already-parsed fields. `--agents` JSON enters here too.
pub(crate) fn from_fields(
    raw: Map<String, Value>,
    body: &str,
    source: DefSource,
) -> Result<(AgentDef, Vec<String>), DefError> {
    let mut warnings = Vec::new();
    let mut m: BTreeMap<String, Value> = BTreeMap::new();
    for (k, v) in raw {
        let key = canonical_key(&k);
        if m.contains_key(&key) {
            // The map has lost the file order, so the snake_case spelling wins.
            warnings.push(format!(
                "duplicate key `{key}`, keeping the snake_case spelling"
            ));
            if k != key {
                continue;
            }
        }
        m.insert(key, v);
    }

    let name = match m.remove("name") {
        Some(Value::String(s)) => AgentName::new(&s)?,
        Some(_) => return Err(want("name", "a string")),
        None => return Err(DefError::Missing("name")),
    };
    let description = match m.remove("description") {
        Some(Value::String(s)) if !s.trim().is_empty() => s,
        Some(Value::String(_)) | None => return Err(DefError::Missing("description")),
        Some(_) => return Err(want("description", "a string")),
    };

    let role = choice(&mut m, "role", "lead, worker, advisor or router")?.unwrap_or(Role::Worker);
    let allow = match m.remove("tools") {
        Some(v) => Some(patterns("tools", v)?),
        None => None,
    };
    let deny = match m.remove("disallowed_tools") {
        Some(v) => {
            let mut deny = patterns("disallowed_tools", v)?;
            // A specifier on a deny entry still removes the whole tool.
            for p in &mut deny {
                p.spec = None;
            }
            deny.dedup_by(|a, b| a.tool == b.tool);
            deny
        }
        None => Vec::new(),
    };

    let model = match string(&mut m, "model")? {
        None => ModelRef::Inherit,
        Some(s) if s == "inherit" => ModelRef::Inherit,
        // `sonnet`, `opus`, `fast`: a bare lowercase word is an alias. Anything with a digit or
        // punctuation (`claude-sonnet-4-5`, `gpt-5`, `o3`) is a provider-native id.
        Some(s)
            if s.starts_with(|c: char| c.is_ascii_lowercase())
                && s.chars().all(|c| c.is_ascii_lowercase() || c == '_') =>
        {
            ModelRef::Alias(s.into())
        }
        Some(s) => ModelRef::Full(ModelId(s.into())),
    };
    let account = string(&mut m, "account")?.map(|s| AccountName(s.into()));
    let permission_mode = match string(&mut m, "permission_mode")? {
        None => PermissionMode::Default,
        Some(s) => {
            let s = canonical_key(&s);
            let s = if s == "bypass_permissions" {
                "bypass".into()
            } else {
                s
            };
            from_str_value(&s).ok_or_else(|| {
                want(
                    "permission_mode",
                    "default, accept_edits, dont_ask, bypass or plan",
                )
            })?
        }
    };
    let max_turns = match uint(&mut m, "max_turns")? {
        Some(n) => Some(u32::try_from(n).map_err(|_| want("max_turns", "a u32"))?),
        None => None,
    };
    let max_tokens_total = uint(&mut m, "max_tokens_total")?;
    let max_cost_usd = match m.remove("max_cost_usd") {
        None => None,
        Some(Value::Number(n)) => Some(
            n.as_f64()
                .filter(|f| *f >= 0.0)
                .ok_or_else(|| want("max_cost_usd", "a non-negative number"))?,
        ),
        Some(_) => return Err(want("max_cost_usd", "a non-negative number")),
    };
    let effort = match string(&mut m, "effort")? {
        None => None,
        Some(s) => Some(match s.to_ascii_lowercase().as_str() {
            "low" => Effort::Low,
            "medium" => Effort::Medium,
            "high" => Effort::High,
            "max" => Effort::Max,
            _ => return Err(want("effort", "low, medium, high or max")),
        }),
    };
    let skills = match m.remove("skills") {
        None => Vec::new(),
        Some(v) => {
            let mut seen = Vec::new();
            for s in string_list("skills", v)? {
                if !seen.contains(&s) && seen.len() < MAX_SKILLS {
                    seen.push(s);
                }
            }
            seen
        }
    };
    let hooks = match m.remove("hooks") {
        Some(v) => hooks(v)?,
        None => HookSet::new(),
    };
    let memory = choice(&mut m, "memory", "user, project or local")?;
    let background = boolean(&mut m, "background")?.unwrap_or(false);
    let isolation = choice(&mut m, "isolation", "none or worktree")?.unwrap_or_default();
    let omit_project_rules = boolean(&mut m, "omit_project_rules")?.unwrap_or(false);
    let color = choice(
        &mut m,
        "color",
        "red, blue, green, yellow, purple, orange, pink or cyan",
    )?;
    let checkpoints = match m.remove("checkpoints") {
        Some(v) => checkpoints(v)?,
        None if role == Role::Advisor => ADVISOR_CHECKPOINTS
            .iter()
            .map(|n| CheckpointSpec {
                name: (*n).into(),
                on: None,
                matcher: None,
            })
            .collect(),
        None => Vec::new(),
    };
    let router = match m.remove("router") {
        Some(v) => Some(router(v)?),
        None if role == Role::Router => Some(RouterSpec {
            threshold: DEFAULT_ROUTER_THRESHOLD,
            questions: Vec::new(),
        }),
        None => None,
    };
    let initial_prompt = string(&mut m, "initial_prompt")?;
    let auto_compact_at = match uint(&mut m, "auto_compact_at")? {
        Some(n) => Some(u32::try_from(n).map_err(|_| want("auto_compact_at", "a u32"))?),
        None => None,
    };

    warnings.extend(m.into_keys().map(|k| format!("unknown key `{k}`")));

    Ok((
        AgentDef {
            name,
            description,
            role,
            tools: ToolPolicy { allow, deny },
            model,
            account,
            permission_mode,
            max_turns,
            max_tokens_total,
            max_cost_usd,
            effort,
            skills,
            hooks,
            memory,
            background,
            isolation,
            omit_project_rules,
            color,
            checkpoints,
            router,
            initial_prompt,
            auto_compact_at,
            system_prompt: body.trim().to_string(),
            source,
        },
        warnings,
    ))
}

fn want(field: &str, want: &'static str) -> DefError {
    DefError::Field {
        field: field.into(),
        want,
    }
}

/// `maxTurns` and `max_turns` are the same key. Also folds the one renamed Claude Code key.
fn canonical_key(k: &str) -> String {
    let mut out = String::with_capacity(k.len() + 2);
    for c in k.chars() {
        if c.is_ascii_uppercase() {
            if !out.is_empty() {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    match out.as_str() {
        "omit_claude_md" => "omit_project_rules".into(),
        "timeout" => "timeout_secs".into(),
        _ => out,
    }
}

fn from_str_value<T: DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(Value::String(s.to_string())).ok()
}

fn string(m: &mut BTreeMap<String, Value>, key: &'static str) -> Result<Option<String>, DefError> {
    match m.remove(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(want(key, "a string")),
    }
}

fn uint(m: &mut BTreeMap<String, Value>, key: &'static str) -> Result<Option<u64>, DefError> {
    match m.remove(key) {
        None => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .map(Some)
            .ok_or_else(|| want(key, "a non-negative integer")),
        Some(_) => Err(want(key, "a non-negative integer")),
    }
}

fn boolean(m: &mut BTreeMap<String, Value>, key: &'static str) -> Result<Option<bool>, DefError> {
    match m.remove(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(b)),
        Some(_) => Err(want(key, "true or false")),
    }
}

/// A snake_case enum field, through the type's serde names.
fn choice<T: DeserializeOwned>(
    m: &mut BTreeMap<String, Value>,
    key: &'static str,
    one_of: &'static str,
) -> Result<Option<T>, DefError> {
    match string(m, key)? {
        None => Ok(None),
        Some(s) => from_str_value(&s.to_ascii_lowercase())
            .map(Some)
            .ok_or_else(|| want(key, one_of)),
    }
}

/// A list of strings, or one comma separated string. Commas inside parentheses do not split.
fn string_list(field: &'static str, v: Value) -> Result<Vec<String>, DefError> {
    let items = match v {
        Value::String(s) => {
            let mut parts = Vec::new();
            let (mut depth, mut start) = (0usize, 0);
            for (i, c) in s.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth = depth.saturating_sub(1),
                    ',' if depth == 0 => {
                        parts.push(s[start..i].to_string());
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            parts.push(s[start..].to_string());
            parts
        }
        Value::Array(a) => a
            .into_iter()
            .map(|e| match e {
                Value::String(s) => Ok(s),
                _ => Err(want(field, "a list of strings or a comma separated string")),
            })
            .collect::<Result<_, _>>()?,
        _ => return Err(want(field, "a list of strings or a comma separated string")),
    };
    Ok(items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// `bash(git *)` becomes `{ tool: "bash", spec: Some("git *") }`. Tool names are lowercased
/// (`Bash` from a Claude Code file), except `mcp` patterns, whose server names are case
/// sensitive.
fn patterns(field: &'static str, v: Value) -> Result<Vec<ToolPattern>, DefError> {
    string_list(field, v)?
        .into_iter()
        .map(|e| {
            let (tool, spec) = match e.split_once('(') {
                Some((t, rest)) => {
                    let spec = rest
                        .strip_suffix(')')
                        .ok_or_else(|| want(field, "tool or tool(specifier)"))?;
                    (t.trim(), Some(spec.trim()).filter(|s| !s.is_empty()))
                }
                None => (e.as_str(), None),
            };
            if tool.is_empty() || tool.contains(')') {
                return Err(want(field, "tool or tool(specifier)"));
            }
            let tool = if tool.starts_with("mcp") {
                tool.to_string()
            } else {
                tool.to_ascii_lowercase()
            };
            Ok(ToolPattern {
                tool: tool.into(),
                spec: spec.map(SmolStr::from),
            })
        })
        .collect()
}

fn hooks(v: Value) -> Result<HookSet, DefError> {
    let Value::Object(events) = v else {
        return Err(want("hooks", "a map of event to hook list"));
    };
    let mut set = HookSet::new();
    for (event, list) in events {
        let Value::Array(list) = list else {
            return Err(want("hooks", "a list of hooks under each event"));
        };
        let mut specs = Vec::new();
        for h in list {
            let Value::Object(h) = h else {
                return Err(want("hooks", "hook entries with a command"));
            };
            let h: BTreeMap<String, Value> =
                h.into_iter().map(|(k, v)| (canonical_key(&k), v)).collect();
            let Some(Value::String(command)) = h.get("command") else {
                return Err(want("hooks", "hook entries with a command"));
            };
            specs.push(HookSpec {
                matcher: match h.get("matcher") {
                    None => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(_) => return Err(want("hooks", "a string matcher")),
                },
                command: command.clone(),
                timeout_secs: match h.get("timeout_secs") {
                    None => None,
                    Some(v) => Some(
                        v.as_u64()
                            .and_then(|n| u32::try_from(n).ok())
                            .ok_or_else(|| want("hooks", "a u32 timeout"))?,
                    ),
                },
            });
        }
        set.insert(canonical_key(&event).into(), specs);
    }
    Ok(set)
}

fn checkpoints(v: Value) -> Result<Vec<CheckpointSpec>, DefError> {
    const WANT: &str = "a list of checkpoint names or {name, on, matcher} maps";
    let Value::Array(list) = v else {
        return Err(want("checkpoints", WANT));
    };
    list.into_iter()
        .map(|c| match c {
            Value::String(name) => Ok(CheckpointSpec {
                name: name.into(),
                on: None,
                matcher: None,
            }),
            Value::Object(o) => {
                let get = |k: &str| match o.get(k) {
                    None => Ok(None),
                    Some(Value::String(s)) => Ok(Some(s.clone())),
                    Some(_) => Err(want("checkpoints", WANT)),
                };
                Ok(CheckpointSpec {
                    name: get("name")?
                        .ok_or_else(|| want("checkpoints", WANT))?
                        .into(),
                    on: get("on")?.map(SmolStr::from),
                    matcher: get("matcher")?,
                })
            }
            _ => Err(want("checkpoints", WANT)),
        })
        .collect()
}

fn router(v: Value) -> Result<RouterSpec, DefError> {
    let Value::Object(o) = v else {
        return Err(want("router", "a map with threshold and questions"));
    };
    let threshold = match o.get("threshold") {
        None => DEFAULT_ROUTER_THRESHOLD,
        Some(t) => {
            t.as_f64()
                .filter(|t| (0.0..=1.0).contains(t))
                .ok_or_else(|| want("router", "a threshold between 0 and 1"))? as f32
        }
    };
    let questions = match o.get("questions") {
        None => Vec::new(),
        Some(q) => string_list("router", q.clone())?
            .into_iter()
            .map(SmolStr::from)
            .collect(),
    };
    Ok(RouterSpec {
        threshold,
        questions,
    })
}

fn yaml_to_json(y: &Yaml) -> Result<Value, DefError> {
    Ok(match y {
        Yaml::Null => Value::Null,
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Integer(i) => Value::from(*i),
        Yaml::String(s) => Value::String(s.clone()),
        Yaml::Real(r) => r
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::String(r.clone()), Value::Number),
        Yaml::Array(a) => Value::Array(a.iter().map(yaml_to_json).collect::<Result<_, _>>()?),
        Yaml::Hash(h) => {
            let mut m = Map::new();
            for (k, v) in h {
                let Yaml::String(k) = k else {
                    return Err(DefError::Yaml("mapping keys must be strings".into()));
                };
                m.insert(k.clone(), yaml_to_json(v)?);
            }
            Value::Object(m)
        }
        Yaml::Alias(_) | Yaml::BadValue => {
            return Err(DefError::Yaml("unresolved alias or bad value".into()))
        }
    })
}
