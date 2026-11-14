//! G8: a workflow write by the author goes through the permission step every time. The tool is
//! the one `/workflow new --write` registers late, under `mcp:aibus-write:*`; here a spy stands in
//! for the MCP client, so what is checked is the executor: the line it shows, that a pre-approved
//! pattern does not skip the question, that "always" does not stick, and that a refusal is a
//! result and not a write.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use aibus_core::mcp::{asks_every_time, WRITE_SERVER};
use aibus_core::*;
use aibus_provider::ToolSpec;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const CREATE: &str = "mcp__aibus-write__workflow_create";
const UPDATE: &str = "mcp__aibus-write__workflow_update";

const TWO_STEPS: &str = "[[step]]\nagent = \"explore\"\nprompt = \"a {{input}}\"\n\n[[step]]\nagent = \"plan\"\nprompt = \"b {{prev.summary}}\"\n";

struct Spy {
    name: &'static str,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for Spy {
    fn name(&self) -> &str {
        self.name
    }
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: String::new(),
            schema: json!({}),
        }
    }
    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Ask
    }
    async fn call(&self, _: &ToolCx, _: Value) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput {
            content: "written".into(),
            ..Default::default()
        })
    }
}

fn cx(cwd: &Path) -> ToolCx {
    ToolCx {
        run: RunId::new(),
        cwd: cwd.to_path_buf(),
        cancel: CancellationToken::new(),
        limits: ToolLimits::default(),
        tx: mpsc::channel(1).0,
        memory: None,
    }
}

fn allow(tool: &str) -> ToolPattern {
    ToolPattern {
        tool: tool.into(),
        spec: None,
    }
}

/// An executor that starts with no write tool, as a session does, with `mcp:*` pre-approved the
/// way a careless `[tools] allow` would.
fn executor() -> (ToolExecutor, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Gate {
        allow: vec![allow("mcp:*")],
        tty: true,
        ..Gate::default()
    };
    let exec = ToolExecutor::with_gate(ToolRegistry::default(), gate).unwrap();
    exec.register_late(Arc::new(Spy {
        name: CREATE,
        calls: calls.clone(),
    }));
    exec.register_late(Arc::new(Spy {
        name: UPDATE,
        calls: calls.clone(),
    }));
    (exec, calls)
}

fn eff(patterns: &[&str]) -> EffectiveTools {
    EffectiveTools {
        allow: patterns.iter().map(|p| allow(p)).collect(),
        deny: vec![],
    }
}

fn create_input() -> Value {
    json!({"name": "review", "scope": "project", "toml": TWO_STEPS})
}

/// Calls `tool` and answers its question with `answer`; returns the result and the lines asked.
async fn call(
    exec: &ToolExecutor,
    tool: &str,
    input: Value,
    answer: Decision,
) -> (Result<ToolOutput, ToolError>, Vec<String>) {
    let asked = Mutex::new(Vec::new());
    let dir = tempfile::tempdir().unwrap();
    let got = exec
        .call_gated(
            &eff(&["mcp:aibus-write:*"]),
            &cx(dir.path()),
            PermissionMode::Default,
            tool,
            input,
            |line| {
                asked.lock().unwrap().push(line);
                async move { answer }
            },
        )
        .await;
    (got, asked.into_inner().unwrap())
}

#[tokio::test]
async fn create_asks_and_y_writes_n_denies_as_data() {
    let (exec, calls) = executor();
    let (got, asked) = call(&exec, CREATE, create_input(), Decision::Allow).await;
    assert_eq!(got.unwrap().content, "written");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The pre-approved `mcp:*` did not skip the question, and the line says what is being written.
    assert_eq!(
        asked,
        [format!(
            "{CREATE}: create review (project), 2 steps, no gate"
        )]
    );

    let (got, asked) = call(&exec, CREATE, create_input(), Decision::Deny).await;
    assert!(
        matches!(got, Err(ToolError::Denied { ref reason }) if reason == "permission denied"),
        "{got:?}"
    );
    assert_eq!(asked.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a refusal writes nothing");
}

#[tokio::test]
async fn always_is_not_offered_for_write_tools() {
    let (exec, calls) = executor();
    // Answering "always" lets this call through, and the next one asks again.
    let (first, asked_first) = call(&exec, CREATE, create_input(), Decision::AllowSession).await;
    assert!(first.is_ok());
    let (second, asked_second) = call(&exec, CREATE, create_input(), Decision::Allow).await;
    assert!(second.is_ok());
    assert_eq!((asked_first.len(), asked_second.len()), (1, 1));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        exec.session_allow().is_empty(),
        "no session pattern was added: {:?}",
        exec.session_allow()
    );
    // Nor is the modal's key a way in: the tool name is what the modal reads.
    assert!(asks_every_time(CREATE) && asks_every_time(UPDATE));
    assert!(!asks_every_time("mcp__aibus__workflow_validate"));
    assert!(!asks_every_time("mcp__aibus-writer__workflow_create"));
    assert!(!asks_every_time("bash"));
}

#[tokio::test]
async fn an_update_names_the_workflow_and_the_revision_it_expects() {
    let (exec, _) = executor();
    let input = json!({"name": "review", "scope": "project", "toml": TWO_STEPS, "expected_revision": "ab12cd34ef56"});
    let (_, asked) = call(&exec, UPDATE, input, Decision::Deny).await;
    assert_eq!(
        asked,
        [format!("{UPDATE}: update review, revision ab12cd34...")]
    );
}

#[test]
fn without_the_flag_the_write_tools_are_not_in_the_list_the_agent_sees() {
    let (exec, _) = executor();
    // The read-only author's allow-list is `mcp:aibus:*`: the write child's tools are not in it.
    assert!(exec.specs(&eff(&["read", "mcp:aibus:*"])).is_empty());
    // A write run's list names them, and a deny for them (every other agent's) hides them again.
    let with = exec.specs(&eff(&["mcp:aibus-write:*"]));
    assert_eq!(with.len(), 2, "{with:?}");
    let denied = EffectiveTools {
        allow: vec![allow("mcp:*")],
        deny: vec![allow(&format!("mcp:{WRITE_SERVER}:*"))],
    };
    assert!(exec.specs(&denied).is_empty());
}
