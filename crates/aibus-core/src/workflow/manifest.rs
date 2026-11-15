//! The reproducibility manifest (#283, spec section 4.3) and the comparison of two plans.
//!
//! `run_manifest` pins everything a run starts from: the compiled plan's hash and versions, the
//! task input (by hash only), the aibus build, the host, the runtimes, the routing decisions and
//! the repository. A value nobody knows yet is written as `{"unknown": "<why>"}` and one that does
//! not apply as `{"not_applicable": "<why>"}`, never as an empty string or a guess.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{json, Map, Value};

use super::compile::content_hash;

/// What the caller knows about the run that is about to start. Each `Err` says why a value is
/// not known.
#[derive(Clone, Debug)]
pub struct RunFacts {
    /// The task input; only its hash and length go into the manifest.
    pub input: Option<String>,
    pub aibus_version: String,
    pub os: String,
    pub arch: String,
    pub base_commit: Result<String, String>,
    pub dirty: Result<bool, String>,
    /// The names of the environment variables (never a value).
    pub env_names: Vec<String>,
    /// The per-run budgets in force (`max_turns`, ...).
    pub budgets: Value,
}

/// One difference between two plans: where, and the value on each side (`null` for absent).
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct Change {
    pub path: String,
    pub a: Value,
    pub b: Value,
}

fn known(v: impl Into<Value>) -> Value {
    json!({ "value": v.into() })
}

fn unknown(why: &str) -> Value {
    json!({ "unknown": why })
}

fn not_applicable(why: &str) -> Value {
    json!({ "not_applicable": why })
}

/// The `run-manifest.json` for a run of `plan`.
pub fn run_manifest(plan: &Value, facts: &RunFacts) -> Value {
    let empty = Vec::new();
    let steps = plan["steps"].as_array().unwrap_or(&empty);
    let pinned: Vec<Value> = steps
        .iter()
        .map(|s| {
            json!({
                "key": s["key"],
                "prompt_sha256": s["prompt"]["sha256"],
                "agent_sha256": s["agent_sha256"],
                "runtime": s["runtime"],
                "mode": s["mode"],
            })
        })
        .collect();
    let models: Vec<Value> = steps
        .iter()
        .map(|s| {
            let configured = &plan["agents"][s["agent"].as_str().unwrap_or_default()]["model"];
            json!({
                "step": s["key"],
                "configured": known(configured.clone()),
                "reported": unknown("only what the runtime reports during a run counts, and nothing has run"),
            })
        })
        .collect();
    let runtimes: Vec<Value> = plan["runtimes"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(id, info)| {
                    let (executable, version) = if id == "aibus" {
                        (known("aibus (this binary)"), known(facts.aibus_version.clone()))
                    } else {
                        (
                            unknown("found and probed when a run launches it (#282), not guessed before"),
                            unknown("probed when a run launches it (#282)"),
                        )
                    };
                    json!({
                        "id": id,
                        "status": info["status"],
                        "capabilities": info["capabilities"],
                        "executable": executable,
                        "version": version,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let (sha, bytes) = match &facts.input {
        Some(text) => (known(content_hash(text)), known(text.len())),
        None => {
            let why = "no task input was given";
            (unknown(why), unknown(why))
        }
    };
    let mut names = facts.env_names.clone();
    names.sort();
    names.dedup();
    let worktree = steps.iter().any(|s| s["isolation"] == "git_worktree");
    json!({
        "kind": "aibus.run-manifest",
        "manifest_version": 1,
        "trace_schema_version": unknown("the trace store (#282) sets it when the run is recorded"),
        "workflow": {
            "name": plan["workflow"]["name"],
            "revision": match plan["workflow"]["revision"].as_str() {
                Some(r) => known(r),
                None => not_applicable("the workflow has no revision"),
            },
            "plan_hash": plan["plan_hash"],
            "source_sha256": plan["workflow"]["source_sha256"],
        },
        "steps": pinned,
        "task_input": { "sha256": sha, "bytes": bytes },
        "aibus": { "version": known(facts.aibus_version.clone()) },
        "host": { "os": known(facts.os.clone()), "arch": known(facts.arch.clone()) },
        "runtimes": runtimes,
        "models": models,
        "jev": {
            "policy_revision": known(plan["jev"]["policy_revision"].clone()),
            "decisions": plan["jev"]["decisions"],
        },
        "repository": {
            "base_commit": match &facts.base_commit {
                Ok(c) => known(c.clone()),
                Err(why) => unknown(why),
            },
            "dirty_before": match &facts.dirty {
                Ok(d) => known(*d),
                Err(why) => unknown(why),
            },
            "dirty_after": unknown("known when the run ends"),
            "worktree_id": if worktree {
                unknown("a worktree is made when the attempt starts (#290)")
            } else {
                not_applicable("no step asks for a worktree")
            },
        },
        "environment": {
            "variable_names": names,
            "fingerprint": content_hash(&names.join("\n")),
        },
        "budgets": facts.budgets,
        "approvals": {
            "required_for": plan["approvals"]["required_for"],
            "outcomes": unknown("decided while the run goes"),
        },
        "execution": {
            "runnable_now": plan["runnable_now"],
            "attempt_id": unknown("the trace store (#282) assigns it when the attempt starts"),
            "started_at": unknown("the run has not started"),
            "ended_at": unknown("the run has not ended"),
        },
        "artifacts": unknown("hashed as the run produces them"),
        "provider_session_id": not_applicable(
            "aibus does not keep vendor session ids before #282 decides they may be kept"
        ),
    })
}

fn walk(path: &str, a: &Value, b: &Value, out: &mut Vec<Change>) {
    if a == b {
        return;
    }
    match (a.as_object(), b.as_object()) {
        (Some(ma), Some(mb)) => {
            let keys: BTreeSet<&String> = ma.keys().chain(mb.keys()).collect();
            for k in keys {
                let at = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                walk(
                    &at,
                    ma.get(k).unwrap_or(&Value::Null),
                    mb.get(k).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        _ => out.push(Change {
            path: path.to_owned(),
            a: a.clone(),
            b: b.clone(),
        }),
    }
}

/// Every difference between two plans. Steps are matched by key and edges by their ends and
/// kind, so a step moved in the file is not a change and a renamed one is.
pub fn diff(a: &Value, b: &Value) -> Vec<Change> {
    /// `plan` with its lists that have an identity turned into maps keyed by it: steps by key, edges
    /// by kind and ends, decisions by step.
    fn keyed(plan: &Value) -> Value {
        let mut v = plan.clone();
        let by = |list: &Value, key: &dyn Fn(&Value) -> String| -> Value {
            match list.as_array() {
                Some(items) => Value::Object(
                    items
                        .iter()
                        .map(|x| (key(x), x.clone()))
                        .collect::<Map<_, _>>(),
                ),
                None => list.clone(),
            }
        };
        let text = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_owned);
        if let Some(m) = v.as_object_mut() {
            if let Some(steps) = m.get("steps") {
                let steps = by(steps, &|s| text(&s["key"]));
                m.insert("steps".into(), steps);
            }
            if let Some(edges) = m.get("edges") {
                let edges = by(edges, &|e| {
                    let via = e
                        .get("via")
                        .map(|v| format!(" via {}", text(v)))
                        .unwrap_or_default();
                    format!(
                        "{} {} -> {}{via}",
                        text(&e["kind"]),
                        text(&e["from"]),
                        text(&e["to"])
                    )
                });
                m.insert("edges".into(), edges);
            }
            if let Some(jev) = m.get_mut("jev").and_then(Value::as_object_mut) {
                if let Some(d) = jev.get("decisions") {
                    let d = by(d, &|x| text(&x["step"]));
                    jev.insert("decisions".into(), d);
                }
            }
        }
        v
    }
    let mut out = Vec::new();
    walk("", &keyed(a), &keyed(b), &mut out);
    out
}
