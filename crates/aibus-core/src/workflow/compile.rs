//! The workflow compiler (#283, spec sections 4.1 and 4.2): a parsed workflow and the agent
//! definitions it names become one normalized plan. The plan is the definition DAG with typed
//! edges, the fan-out groups, the envelope, the approval boundary, a content hash for every prompt
//! and agent, the invocation each step would get and the routing decisions, all with a
//! `plan_hash` over the lot. Nothing runs, and nothing in the plan depends on the host, the clock
//! or the order a map happens to iterate in, so the same input gives the same bytes anywhere.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::graph::{self, Dag};
use super::jev::{self, Request};
use super::{reads, Capability, Mode, Problem, Runtime, Workflow};
use crate::agent::def::{model_str, to_json};
use crate::agent::{AgentDef, Scope};
use crate::ids::AgentName;
use crate::Redactor;

/// `sha256:<hex>` of `text` with CRLF line ends read as LF, so a checkout that converts line ends
/// does not change a version.
pub fn content_hash(text: &str) -> String {
    let digest = Sha256::digest(text.replace("\r\n", "\n").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// `v` as JSON with every object's keys sorted and no whitespace: what `plan_hash` hashes.
pub fn canonical_json(v: &Value) -> String {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    write(&m[k], out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(v, &mut out);
    out
}

/// The hash of a compiled plan: `content_hash` of its canonical JSON without the `plan_hash`
/// key. A saved plan whose stored hash differs from this was edited by hand.
pub fn plan_hash(plan: &Value) -> String {
    let mut body = plan.clone();
    if let Some(m) = body.as_object_mut() {
        m.remove("plan_hash");
    }
    content_hash(&canonical_json(&body))
}

/// The agent tools that grant a capability.
fn granted_by(c: Capability) -> &'static [&'static str] {
    match c {
        Capability::RepoRead => &["read", "glob", "grep", "ls"],
        Capability::RepoWrite => &["write", "edit"],
        Capability::Shell | Capability::NetworkWrite | Capability::GitMerge => &["bash"],
        Capability::Network => &["web_fetch"],
        Capability::Delegate => &["delegate"],
        Capability::DestructiveTool => &["bash", "write", "edit"],
    }
}

const ALL: [Capability; 8] = [
    Capability::RepoRead,
    Capability::RepoWrite,
    Capability::Shell,
    Capability::Network,
    Capability::Delegate,
    Capability::NetworkWrite,
    Capability::GitMerge,
    Capability::DestructiveTool,
];

/// The tool names a step's agent gets when the lead starts it (depth 1, foreground).
fn tool_names(def: &AgentDef) -> Vec<String> {
    let depth = crate::orchestrator::Limits::default().max_spawn_depth;
    crate::tools::effective_tools(def, def.role, 1, false, depth)
        .allow
        .iter()
        .map(|p| p.tool.to_string())
        .collect()
}

/// The capabilities `tools` grant.
fn grants(tools: &[String]) -> Vec<Capability> {
    ALL.into_iter()
        .filter(|c| granted_by(*c).iter().any(|t| tools.iter().any(|x| x == t)))
        .collect()
}

/// The capabilities a step `requires` that its agent's tools don't grant.
pub fn capability_problems(wf: &Workflow, agents: &IndexMap<AgentName, AgentDef>) -> Vec<Problem> {
    let mut out = Vec::new();
    for (i, s) in wf.steps.iter().enumerate() {
        let Some(def) = agents.get(&s.agent) else {
            continue;
        };
        let tools = tool_names(def);
        for c in &s.requires {
            let by = granted_by(*c);
            if !by.iter().any(|t| tools.iter().any(|x| x == t)) {
                out.push(Problem {
                    step: Some(i + 1),
                    field: Some("requires".into()),
                    message: format!(
                        "requires {}, and agent {} has no tool that grants it ({})",
                        c.as_str(),
                        s.agent,
                        by.join(" or ")
                    ),
                });
            }
        }
    }
    out
}

/// An aibus tool's name in Claude Code, for the ones that have one.
fn claude_tool(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "read" => "Read",
        "write" => "Write",
        "edit" => "Edit",
        "glob" => "Glob",
        "grep" => "Grep",
        "bash" => "Bash",
        "web_fetch" => "WebFetch",
        "delegate" => "Agent",
        _ => return None,
    })
}

/// The Claude Code tools of an agent, sorted. `Agent` only for a step that launches native
/// subagents.
fn claude_tools(def: &AgentDef, delegates: bool) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = tool_names(def)
        .iter()
        .filter_map(|t| claude_tool(t))
        .filter(|t| delegates || *t != "Agent")
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// A model alias Claude Code takes on `--model`, when the agent asks for one.
fn claude_model(def: &AgentDef) -> Option<&str> {
    let m = model_str(&def.model);
    ["haiku", "sonnet", "opus"].contains(&m).then_some(m)
}

/// The native subagents step `parent` launches, as Claude Code's `--agents` JSON takes them: the
/// definition's prompt first (stable across workflows, so cache friendly) and the step's after it.
pub fn native_agents(
    wf: &Workflow,
    agents: &IndexMap<AgentName, AgentDef>,
    parent: usize,
) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for (i, s) in wf.steps.iter().enumerate() {
        if s.parent.as_deref() != Some(wf.key(parent)) || !wf.native(i) {
            continue;
        }
        let Some(def) = agents.get(&s.agent) else {
            continue;
        };
        let mut agent = json!({
            "description": def.description,
            "prompt": format!("{}\n\n{}", def.system_prompt.trim_end(), s.prompt.trim_end()),
            "tools": claude_tools(def, false),
        });
        if let Some(m) = claude_model(def) {
            agent["model"] = json!(m);
        }
        out.insert(wf.key(i).to_owned(), agent);
    }
    out
}

/// Compiles `wf` (read from `source`) against `agents`. `Err` is every problem found, and a
/// workflow with any is never planned.
pub fn compile(
    wf: &Workflow,
    source: &str,
    agents: &IndexMap<AgentName, AgentDef>,
    redactor: &Redactor,
) -> Result<Value, Vec<Problem>> {
    let refuse = |message: String| Problem {
        step: None,
        field: None,
        message,
    };
    if wf.steps.len() > graph::MAX_STEPS {
        return Err(vec![refuse(format!(
            "{} steps: the compiler plans at most {} steps",
            wf.steps.len(),
            graph::MAX_STEPS
        ))]);
    }
    let mut problems = Vec::new();
    if redactor.redact(source) != source {
        problems.push(refuse(
            "the workflow contains something that looks like a credential, so it gets no plan"
                .into(),
        ));
    }
    // A native subagent's definition is copied into the plan as it is, so it is held to the same rule.
    for (i, s) in wf.steps.iter().enumerate() {
        if let Some(def) = agents.get(&s.agent).filter(|_| wf.native(i)) {
            for (what, text) in [
                ("prompt", &def.system_prompt),
                ("description", &def.description),
            ] {
                if redactor.redact(text) != *text {
                    problems.push(Problem {
                        step: Some(i + 1),
                        field: Some("agent".into()),
                        message: format!(
                            "the {what} of agent `{}` looks like a credential, so it gets no plan",
                            s.agent
                        ),
                    });
                }
            }
        }
    }
    for (i, s) in wf.steps.iter().enumerate() {
        if !agents.contains_key(&s.agent) {
            problems.push(Problem {
                step: Some(i + 1),
                field: Some("agent".into()),
                message: format!("no agent named `{}` in the roster", s.agent),
            });
        }
    }
    problems.extend(capability_problems(wf, agents));
    if !problems.is_empty() {
        return Err(problems);
    }

    /// The process a `cli.claude` step would start: the official executable with an argv vector and
    /// no shell, null stdin, a bounded environment, `--permission-prompts none` so anything that
    /// would ask is denied, only read-only tools approved in advance, and its native subagents inline
    /// in `--agents`, which writes no file. The prompt is rendered at run time from the versioned
    /// template, so the plan names it by hash.
    fn claude_invocation(
        wf: &Workflow,
        agents: &IndexMap<AgentName, AgentDef>,
        i: usize,
        prompt_sha: &str,
    ) -> Value {
        let key = wf.key(i);
        let def = &agents[&wf.steps[i].agent];
        let children = native_agents(wf, agents, i);
        let tools = claude_tools(def, !children.is_empty());
        let mut argv: Vec<String> = vec![
            "-p".into(),
            format!("<prompt of step {key}, template {prompt_sha}, rendered at run time>"),
        ];
        for a in [
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            "--setting-sources",
            "project",
            "--strict-mcp-config",
            "--permission-prompts",
            "none",
        ] {
            argv.push(a.into());
        }
        if let Some(m) = claude_model(def) {
            argv.extend(["--model".into(), m.into()]);
        }
        let read_only = ["Agent", "Glob", "Grep", "Read"];
        let approved: Vec<&str> = tools
            .iter()
            .copied()
            .filter(|t| read_only.contains(t))
            .collect();
        let denied: Vec<&str> = tools
            .iter()
            .copied()
            .filter(|t| !read_only.contains(t))
            .collect();
        if !tools.is_empty() {
            argv.extend(["--tools".into(), tools.join(",")]);
        }
        if !approved.is_empty() {
            argv.extend(["--allowedTools".into(), approved.join(",")]);
        }
        let mut out = json!({
            "kind": "process",
            "program": "claude",
            "shell": false,
            "stdin": "null",
            "env_allowlist": ["CLAUDE_CONFIG_DIR", "HOME", "LANG", "PATH", "USER"],
            "writes_files": [],
            "denied_until_approved": denied,
        });
        if !children.is_empty() {
            let inline = Value::Object(children.clone().into_iter().collect::<Map<_, _>>());
            argv.extend([
                "--agents".into(),
                format!("<agents json {}>", content_hash(&canonical_json(&inline))),
            ]);
            out["agents"] = inline;
            out["delegation"] =
                json!({ "foreground": "requested_in_prompt", "host_enforced": false });
        }
        out["argv"] = json!(argv);
        out
    }

    let n = wf.steps.len();
    let native: Vec<bool> = (0..n).map(|i| wf.native(i)).collect();
    let def = |i: usize| &agents[&wf.steps[i].agent];
    let granted: Vec<Vec<Capability>> = (0..n).map(|i| grants(&tool_names(def(i)))).collect();

    let mut prompts: BTreeMap<String, String> = BTreeMap::new();
    let mut agent_pins: BTreeMap<String, Value> = BTreeMap::new();
    let mut runtimes: BTreeMap<String, Value> = BTreeMap::new();
    let mut approvals: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut decisions = Vec::new();
    let mut notes = Vec::new();
    let mut steps = Vec::new();

    for (i, s) in wf.steps.iter().enumerate() {
        let key = wf.key(i);
        let d = def(i);
        let mut def_json = to_json(d);
        def_json["prompt"] = json!(d.system_prompt.replace("\r\n", "\n"));
        let agent_sha = content_hash(&canonical_json(&def_json));
        agent_pins.insert(
            s.agent.to_string(),
            json!({
                "sha256": agent_sha,
                "scope": match d.source.scope {
                    Scope::Cli => "cli",
                    Scope::Project => "project",
                    Scope::User => "user",
                    Scope::Extra(_) => "extra",
                    Scope::Builtin => "builtin",
                },
                "role": d.role.as_str(),
                "model": model_str(&d.model),
                "permission_mode": def_json["permission_mode"],
                "tools": tool_names(d),
            }),
        );
        let info = jev::runtime_info(s.runtime);
        runtimes.insert(
            info.id.to_owned(),
            json!({ "status": info.status, "capabilities": info.capabilities }),
        );
        let prompt_sha = content_hash(&s.prompt);
        prompts.insert(prompt_sha.clone(), s.prompt.replace("\r\n", "\n"));
        // The part of the prompt that is the same on every run: everything before its first template.
        let prefix = s.prompt.find("{{").unwrap_or(s.prompt.len());
        if prefix == 0 {
            notes.push(format!(
                "step {key}: the prompt starts with a template, so it has no stable prefix for the prompt cache; put the task text at the end"
            ));
        }

        if s.runtime == Runtime::Attached {
            notes.push(format!(
                "step {key}: runs in the attached session under its own permissions, which stay with the user there; the agent's tools do not bound it"
            ));
        }

        // The approval boundary: what this step's agent can do (and a gate, which runs a shell
        // command) that the workflow holds for a person's yes.
        let mut can = granted[i].clone();
        if s.gate.is_some() && !can.contains(&Capability::Shell) {
            can.push(Capability::Shell);
        }
        let held: Vec<&str> = ALL
            .into_iter()
            .filter(|c| can.contains(c) && wf.approvals.contains(c))
            .map(Capability::as_str)
            .collect();
        if !held.is_empty() {
            approvals.insert(key.to_owned(), held);
        }

        let parent = s.parent.as_deref().and_then(|p| wf.index(p));
        // The attached session works under its own permissions, so what the agent definition
        // grants says nothing about what the step can do.
        let risk = if s.runtime == Runtime::Attached
            || granted[i].iter().any(|c| {
                matches!(
                    c,
                    Capability::Shell
                        | Capability::Network
                        | Capability::NetworkWrite
                        | Capability::GitMerge
                )
            }) {
            "side_effects"
        } else if granted[i].contains(&Capability::RepoWrite) {
            "writes"
        } else {
            "read_only"
        };
        let request = Request {
            task_id: key.to_owned(),
            requires: vec!["headless".into(), "one_shot".into()],
            candidates: vec![s.runtime.as_str().to_owned()],
            policy_revision: jev::policy_revision(),
            budget_class: if wf.limits.is_some() {
                "bounded"
            } else {
                "unbounded"
            }
            .into(),
            risk_class: risk.into(),
            requested_mode: s.mode,
            parent_runtime: parent.map(|p| wf.steps[p].runtime.as_str().to_owned()),
            approved: Vec::new(),
        };
        let decision = jev::decide(&request);
        decisions.push(json!({ "step": key, "request": request, "decision": decision }));

        let invocation = if native[i] {
            json!({
                "kind": "native_subagent",
                "launched_by": parent.map(|p| wf.key(p)),
                "via": "--agents",
            })
        } else {
            match s.runtime {
                Runtime::Aibus => json!({
                    "kind": "aibus_run",
                    "agent": s.agent.as_str(),
                    "foreground": true,
                    "parent": parent.map(|p| wf.key(p)),
                    "isolation": s.isolation,
                }),
                Runtime::ClaudeCli => claude_invocation(wf, agents, i, &prompt_sha),
                Runtime::CodexCli | Runtime::GeminiCli => json!({
                    "kind": "cli_run",
                    "extension": if s.runtime == Runtime::CodexCli {
                        "org.devshell.aibus.codex-cli"
                    } else {
                        "org.devshell.aibus.gemini-cli"
                    },
                    "agent": s.agent.as_str(),
                    "prompt_via": "stdin",
                    "parent": parent.map(|p| wf.key(p)),
                }),
                Runtime::OpenRouter => json!({
                    "kind": "openrouter_run",
                    "agent": s.agent.as_str(),
                    "billing": "separate_openrouter_credits",
                    "credential": "the openrouter account's own key only",
                    "needs": "the user's approval for this run",
                    "parent": parent.map(|p| wf.key(p)),
                }),
                Runtime::Attached => json!({
                    "kind": "attached_session",
                    "agent": s.agent.as_str(),
                    "via": "next_step and step_result",
                    "needs": "--control-attach and an accepted /attach",
                }),
            }
        };
        steps.push(json!({
            "key": key,
            "n": i + 1,
            "agent": s.agent.as_str(),
            "agent_sha256": agent_sha,
            "runtime": s.runtime.as_str(),
            "mode": if native[i] { Mode::Native } else { Mode::AibusManaged },
            "mode_requested": s.mode,
            "parent": parent.map(|p| wf.key(p)),
            "requires": s.requires.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            "isolation": s.isolation,
            "output": s.output,
            "gate": s.gate,
            "retries": s.retries,
            "prompt": {
                "sha256": prompt_sha,
                "reads": reads(&s.prompt),
                "stable_prefix_bytes": prefix,
                "volatile_at_end": s.prompt.rfind("}}").is_none_or(|end| s.prompt[end + 2..].trim().is_empty()),
            },
            "invocation": invocation,
        }));
    }

    let mut edges = Vec::new();
    let mut fan_out = Vec::new();
    if wf.routes.is_empty() {
        let scheduled: Vec<usize> = (0..n).filter(|&i| !native[i]).collect();
        for pair in scheduled.windows(2) {
            edges.push(
                json!({ "kind": "sequence", "from": wf.key(pair[0]), "to": wf.key(pair[1]) }),
            );
        }
        for (k, &i) in scheduled.iter().enumerate() {
            let retries = wf.steps[i].retries;
            if retries > 0 {
                let back = if k > 0 { scheduled[k - 1] } else { i };
                edges.push(json!({
                    "kind": "retry",
                    "from": wf.key(i),
                    "to": wf.key(back),
                    "max_iterations": retries,
                }));
            }
        }
    } else {
        for r in &wf.routes {
            let mut e = json!({
                "kind": if r.max_iterations.is_some() { "loop" } else { "route" },
                "from": r.from,
                "to": r.to,
                "when": r.when.to_string(),
            });
            if let Some(k) = r.max_iterations {
                e["max_iterations"] = json!(k);
            }
            edges.push(e);
        }
        let dag = Dag::of(wf);
        for (i, next) in dag.next.iter().enumerate() {
            if next.len() > 1 {
                let mut to: Vec<&str> = next.iter().map(|&t| wf.key(t)).collect();
                to.sort_unstable();
                fan_out.push(json!({ "from": wf.key(i), "to": to }));
            }
        }
    }
    let mut handoffs = Vec::new();
    for (i, s) in wf.steps.iter().enumerate() {
        for read in reads(&s.prompt) {
            let from = if read == "input" {
                Some("input")
            } else {
                wf.source(i, &read).map(|j| wf.key(j))
            };
            if let Some(from) = from {
                edges.push(json!({ "kind": "data", "from": from, "to": wf.key(i), "via": read }));
            }
            // A step's output read by a step on another runtime is a handoff: the user is shown it
            // and says yes before it moves (#360, spec 10.2).
            if let Some(j) = (read != "input").then(|| wf.source(i, &read)).flatten() {
                let (a, b) = (wf.steps[j].runtime, s.runtime);
                if a != b {
                    handoffs.push(json!({
                        "from": wf.key(j), "to": wf.key(i), "via": read,
                        "from_runtime": a.as_str(), "to_runtime": b.as_str(),
                        "needs_consent": true,
                    }));
                }
            }
        }
    }
    for (i, s) in wf.steps.iter().enumerate() {
        if let Some(p) = s.parent.as_deref().and_then(|p| wf.index(p)) {
            edges.push(json!({
                "kind": "delegation",
                "from": wf.key(p),
                "to": wf.key(i),
                "lineage": if native[i] { "native" } else { "aibus_managed" },
            }));
        }
    }

    let limits = wf.limits.unwrap_or_default();
    let plan_only = wf.plan_only();
    let mut plan = json!({
        "kind": "aibus.workflow.plan",
        "manifest_version": 1,
        "compiler": "aibus-workflow-compiler/1",
        "workflow": {
            "name": wf.name,
            "revision": wf.revision,
            "description": wf.description,
            "source_sha256": content_hash(source),
        },
        "entry": wf.key(0),
        "runnable_now": plan_only.is_none(),
        "plan_only_because": plan_only,
        "steps": steps,
        "edges": edges,
        "fan_out": fan_out,
        "envelope": {
            "worst_case_agent_runs": graph::worst_case_runs(wf),
            "max_agent_runs": limits.max_agent_runs,
            "max_parallel": limits.max_parallel,
            "max_wall_seconds": limits.max_wall_seconds,
        },
        "approvals": {
            "required_for": wf.approvals.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            "steps": approvals,
        },
        "agents": agent_pins,
        "prompts": prompts,
        "runtimes": runtimes,
        "jev": { "policy_revision": jev::policy_revision(), "decisions": decisions },
        "notes": notes,
    });
    if !handoffs.is_empty() {
        plan["handoffs"] = json!(handoffs);
    }
    let hash = plan_hash(&plan);
    plan["plan_hash"] = json!(hash);
    Ok(plan)
}
