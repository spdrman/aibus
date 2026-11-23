//! The Jev decision record (#283, spec section 5.2) and the first deterministic routing rules.
//! Jev reads structured metadata, never a token or a prompt, and every decision says which rule
//! made it. The compiler uses it to pick a runtime and an execution mode for each step, and the
//! scheduler (`sched`, #288) asks it again at run time with the resolved account and logs the
//! same record.

use serde::{Deserialize, Serialize};

use super::{Mode, Runtime};

/// What a runtime is known to offer. `status` is `available`, `unverified` (an extension exists and the CLI was never run, so nothing is claimed that its manifest leaves to a probe), `experimental` (plans compile,
/// nothing runs) or `unavailable`. `capabilities` are the spec's capability vector.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub struct RuntimeInfo {
    pub id: &'static str,
    pub status: &'static str,
    pub capabilities: &'static [&'static str],
}

/// What Jev is asked for one step.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub task_id: String,
    /// Runtime capabilities the step needs (`one_shot`, `native_child_lineage`, ...).
    pub requires: Vec<String>,
    /// Runtime ids in the order the author prefers them.
    pub candidates: Vec<String>,
    pub policy_revision: String,
    pub budget_class: String,
    /// `read_only`, `writes` or `side_effects`.
    pub risk_class: String,
    /// The mode the step asked for.
    #[serde(default)]
    pub requested_mode: Mode,
    /// The runtime of the step that launches this one, for a step with a parent.
    #[serde(default)]
    pub parent_runtime: Option<String>,
    /// Runtimes the user approved for this run (`openrouter`: a separately billed API run).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved: Vec<String>,
    /// The provider of the account the step will run under, when the scheduler knows it. An
    /// `aibus` step on an OpenRouter account is OpenRouter for the approval rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_provider: Option<String>,
}

/// What Jev decided, and why. `selected_runtime` is `None` when no candidate fits, which is
/// handed to a person, never guessed.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub selected_runtime: Option<String>,
    pub execution_mode: Option<Mode>,
    pub reason_code: String,
    pub policy_evidence: Vec<String>,
    pub decision_source: String,
}

/// The table of runtimes this aibus knows. `aibus` is its own run loop. `cli.claude` is what
/// the #281 spike observed of Claude Code 2.1.311 under `-p`: structured events, tool events and
/// native child lineage, cancel only for the whole process, usage as a list-price estimate, no
/// session resume, no host-side approval prompt and no workspace control (aibus owns worktrees).
/// It stays `experimental` until #285 rules on the spike.
///
/// `attached` is the agent session attached to this one (#322, #353). It is `experimental` until
/// the live check has run more than once. It can run with no terminal of aibus's own and does one
/// step at a time (`headless`, `one_shot`); everything else is the agent's, so aibus claims
/// nothing more: no cancel (the session cannot be interrupted), no tool events, no usage.
pub fn runtime_info(rt: Runtime) -> RuntimeInfo {
    match rt {
        Runtime::Aibus => RuntimeInfo {
            id: "aibus",
            status: "available",
            capabilities: &[
                "cancel",
                "headless",
                "interactive_approval",
                "one_shot",
                "session_resume",
                "structured_events",
                "tool_events",
                "usage_report",
                "workspace_control",
            ],
        },
        Runtime::ClaudeCli => RuntimeInfo {
            id: "cli.claude",
            status: "experimental",
            capabilities: &[
                "cancel",
                "headless",
                "login_status",
                "native_child_lineage",
                "one_shot",
                "structured_events",
                "tool_events",
                "usage_report",
            ],
        },
        Runtime::Attached => RuntimeInfo {
            id: "attached",
            status: "experimental",
            capabilities: &["headless", "one_shot", "pull_delivery"],
        },
        // The Codex CLI extension (#357): what its manifest declares supported. No native child
        // lineage (no subagent item in 0.167.0), no resume, no host approvals, no workspace control.
        Runtime::CodexCli => RuntimeInfo {
            id: "cli.codex",
            status: "experimental",
            capabilities: &[
                "cancel",
                "headless",
                "login_status",
                "one_shot",
                "structured_events",
                "tool_events",
                "usage_report",
            ],
        },
        // The Gemini CLI extension (#358), unverified: nothing was recorded. No documented login
        // or status subcommand, so no `login_status`.
        Runtime::GeminiCli => RuntimeInfo {
            id: "cli.gemini",
            status: "unverified",
            capabilities: &[
                "cancel",
                "headless",
                "one_shot",
                "tool_events",
                "usage_report",
            ],
        },
        // OpenRouter, the one direct API runtime (#359). It reports the cost of a request, can be
        // cancelled and needs no terminal. It has no vendor CLI, so no login status, no native
        // children and no resume; its key is its own and is never another provider's token.
        Runtime::OpenRouter => RuntimeInfo {
            id: "openrouter",
            status: "experimental",
            capabilities: &["cancel", "headless", "one_shot", "usage_report"],
        },
    }
}

/// `native_or_managed` becomes `native` when the runtime can show native child lineage and the
/// parent runs on the same runtime (a native subagent lives in its parent's process); otherwise
/// `aibus_managed`. The other two modes are what they say.
pub fn resolve_mode(requested: Mode, runtime: Runtime, parent: Option<Runtime>) -> Mode {
    match requested {
        Mode::NativeOrManaged
            if parent == Some(runtime)
                && runtime_info(runtime)
                    .capabilities
                    .contains(&"native_child_lineage") =>
        {
            Mode::Native
        }
        Mode::NativeOrManaged => Mode::AibusManaged,
        other => other,
    }
}

/// The rules `decide` applies, in order. Part of what `policy_revision` hashes, so a change to
/// them is a new revision.
const RULES: &str = "jev-deterministic/1
1 safety: risk_class is read_only, writes or side_effects, else stop (unknown_risk_class)
2 policy: policy_revision is this one, else stop (stale_policy)
3 availability: skip a candidate this aibus does not know or that is unavailable
4 capabilities: skip a candidate missing a required capability (native mode requires native_child_lineage)
4b approval: skip openrouter, or an aibus step whose account is OpenRouter's, unless the user approved it for this run (it is separately billed and sends the prompt to a third party); the skip carries the proposal a person is shown
5 preference: the first candidate left wins (pinned_runtime when it was the only one, else capability_match)
6 mode: native_or_managed is native under a parent on the same runtime with native_child_lineage, else aibus_managed
none left: no_candidate, and a person decides";

/// The policy's revision: the hash of its rules and runtime table, so a plan records which
/// policy made its decisions.
pub fn policy_revision() -> String {
    let table = serde_json::to_string(&[
        runtime_info(Runtime::Aibus),
        runtime_info(Runtime::ClaudeCli),
        runtime_info(Runtime::Attached),
        runtime_info(Runtime::CodexCli),
        runtime_info(Runtime::GeminiCli),
        runtime_info(Runtime::OpenRouter),
    ])
    .unwrap_or_default();
    super::compile::content_hash(&format!("{RULES}\n{table}\n"))
}

/// Decides one request (spec 5.2 precedence: safety, availability, capabilities, then the
/// author's order).
pub fn decide(req: &Request) -> Decision {
    let decided = |runtime: Option<String>,
                   mode: Option<Mode>,
                   reason: &str,
                   evidence: Vec<String>| Decision {
        selected_runtime: runtime,
        execution_mode: mode,
        reason_code: reason.to_owned(),
        policy_evidence: evidence,
        decision_source: "jev_deterministic".to_owned(),
    };
    let mut evidence = vec![format!("risk:{}", req.risk_class)];
    if !["read_only", "writes", "side_effects"].contains(&req.risk_class.as_str()) {
        return decided(None, None, "unknown_risk_class", evidence);
    }
    if req.policy_revision != policy_revision() {
        return decided(
            None,
            None,
            "stale_policy",
            vec![format!("policy_revision:{}", req.policy_revision)],
        );
    }
    let mut needs: Vec<&str> = req.requires.iter().map(String::as_str).collect();
    if req.requested_mode == Mode::Native && !needs.contains(&"native_child_lineage") {
        needs.push("native_child_lineage");
    }
    let known = |id: &str| {
        [
            Runtime::Aibus,
            Runtime::ClaudeCli,
            Runtime::Attached,
            Runtime::CodexCli,
            Runtime::GeminiCli,
            Runtime::OpenRouter,
        ]
        .into_iter()
        .find(|rt| rt.as_str() == id)
    };
    let mut needs_approval = false;
    for id in &req.candidates {
        let Some(rt) = known(id) else {
            evidence.push(format!("unknown_runtime:{id}"));
            continue;
        };
        let info = runtime_info(rt);
        if info.status == "unavailable" {
            evidence.push(format!("unavailable:{id}"));
            continue;
        }
        if let Some(missing) = needs.iter().find(|c| !info.capabilities.contains(c)) {
            evidence.push(format!("missing:{missing}@{id}"));
            continue;
        }
        // OpenRouter is the receiving party whether it is the runtime or the account under `aibus`.
        let via_openrouter = rt == Runtime::OpenRouter
            || (rt == Runtime::Aibus && req.account_provider.as_deref() == Some("openrouter"));
        if via_openrouter {
            if !req.approved.iter().any(|a| a == "openrouter") {
                // No silent fallback: what the person would be agreeing to, each part its own line.
                needs_approval = true;
                evidence.extend([
                    "proposal:billing=separate OpenRouter credits on the OpenRouter key, not your Claude, ChatGPT or Gemini subscription allowance".to_owned(),
                    "proposal:models=downstream models chosen per request, not a vendor CLI's own model".to_owned(),
                    "proposal:data=the prompt and the step's context go to OpenRouter and to the model provider it routes to".to_owned(),
                    format!("proposal:capabilities={}; no login status, no native child lineage, no session resume", info.capabilities.join(", ")),
                ]);
                continue;
            }
            evidence.push("approved:openrouter".to_owned());
        }
        evidence.push(format!("status:{id}={}", info.status));
        let parent = req.parent_runtime.as_deref().and_then(known);
        let mode = resolve_mode(req.requested_mode, rt, parent);
        evidence.push(
            match (req.requested_mode, mode) {
                (Mode::NativeOrManaged, Mode::Native) => "mode:native_child_lineage",
                (Mode::NativeOrManaged, _) => "mode:no_native_parent",
                _ => "mode:as_requested",
            }
            .to_owned(),
        );
        let reason = if req.candidates.len() == 1 {
            "pinned_runtime"
        } else {
            "capability_match"
        };
        return decided(Some(id.clone()), Some(mode), reason, evidence);
    }
    let reason = if needs_approval {
        "openrouter_needs_approval"
    } else {
        "no_candidate"
    };
    decided(None, None, reason, evidence)
}
