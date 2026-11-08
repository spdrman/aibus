#![allow(dead_code)]

use std::time::Duration;

use aibus_core::state::reduce::Env;
use aibus_core::state::value::s;
use aibus_core::*;
use aibus_graph::mini::MiniGraph;
use aibus_graph::{Graph, NodeKey, Value};
use aibus_provider::StreamChunk;
use ulid::Ulid;

pub const NOW: u64 = 1_700_000_000_000;
/// Planted in every text field of every sample message; it must never reach `model:last_msg`.
pub const SENTINEL: &str = "SECRET-SENTINEL-7731";

pub type A = App<MiniGraph>;

pub const ALL_NAMES: [&str; 26] = [
    "Key",
    "Mouse",
    "Resize",
    "Tick",
    "SubmitPrompt",
    "Command",
    "RunStarted",
    "Stream",
    "ToolCallRequested",
    "ToolCallFinished",
    "PermissionNeeded",
    "PermissionDecided",
    "RunFinished",
    "RunFailed",
    "UsageUpdate",
    "HookFired",
    "RouterAnswered",
    "AdvisorSpoke",
    "CheckpointHit",
    "Compacted",
    "HandoffWritten",
    "AgentsReloaded",
    "AccountsReloaded",
    "IoError",
    "Shutdown",
    "Persisted",
];

pub fn env() -> Env {
    Env {
        now_ms: NOW,
        ticks_dropped: 0,
    }
}

/// An app whose clock already reads `NOW`, so the clock write drops out of write sets.
pub fn app() -> A {
    let mut a = App::new(MiniGraph::new(), || NOW);
    a.step(Msg::Key(KeyEvent {
        code: KeyCode::Enter,
        ctrl: false,
        alt: false,
        shift: false,
    }))
    .unwrap();
    a
}

pub fn rid(n: u128) -> RunId {
    RunId(Ulid::from_parts(1, n))
}

pub const R: fn() -> RunId = || rid(1);
pub fn perm() -> PermissionId {
    PermissionId(Ulid::from_parts(1, 2))
}
pub fn call() -> ToolCallId {
    ToolCallId(Ulid::from_parts(1, 3))
}
/// The id the first `Delegate` of a fresh app gets.
pub fn first_delegated() -> RunId {
    RunId(Ulid::from_parts(NOW, 1))
}

pub fn poke(a: &mut A, key: &str, v: Value) {
    let n = a.graph.node(key).unwrap();
    a.graph.commit("poke", &[(n, v)]).unwrap();
}

pub fn started(run: RunId) -> Msg {
    Msg::RunStarted {
        run,
        agent: "worker".parse().unwrap(),
        role: Role::Worker,
        parent: None,
        model: ModelId("m".into()),
        account: AccountName("acct".into()),
    }
}

pub fn text_stream(run: RunId, t: &str) -> Msg {
    Msg::Stream {
        run,
        chunks: vec![StreamChunk::TextDelta(t.into())],
    }
}

pub fn delegate() -> Msg {
    Msg::Command(Command::Delegate {
        agent: "worker".parse().unwrap(),
        prompt: SENTINEL.into(),
        background: false,
        isolation: Isolation::None,
        parent: None,
    })
}

pub fn running(a: &mut A) {
    a.step(started(R())).unwrap();
    a.step(text_stream(R(), "hi")).unwrap();
}

pub fn tool_call(needs_permission: bool) -> ToolCall {
    ToolCall {
        id: call(),
        name: "bash".into(),
        input: serde_json::json!({"command": SENTINEL}),
        needs_permission,
    }
}

pub fn with_tool(a: &mut A) {
    running(a);
    a.step(Msg::ToolCallRequested {
        run: R(),
        call: tool_call(true),
    })
    .unwrap();
}

pub fn permission_request() -> PermissionRequest {
    PermissionRequest {
        id: perm(),
        run: R(),
        call: call(),
        tool: "bash".into(),
        summary: SENTINEL.into(),
    }
}

pub fn with_perm(a: &mut A) {
    with_tool(a);
    a.step(Msg::PermissionNeeded {
        request: permission_request(),
    })
    .unwrap();
}

pub fn none(_: &mut A) {}

pub struct Case {
    pub name: &'static str,
    pub setup: fn(&mut A),
    pub msg: Msg,
    pub expect: Vec<&'static str>,
}

/// The nine run inputs, the order list and the log row of a new run.
pub fn new_run_keys() -> Vec<&'static str> {
    vec![
        "model:runs:order",
        "model:log",
        "model:run:<id>:meta",
        "model:run:<id>:status",
        "model:run:<id>:turns",
        "model:run:<id>:usage",
        "model:run:<id>:progress",
        "model:run:<id>:blocks",
        "model:run:<id>:tail",
        "model:run:<id>:error",
        "model:run:<id>:cancel_requested",
    ]
}

pub fn normalise(key: &str) -> String {
    key.split(':')
        .map(|seg| {
            if seg.len() == 26 && seg.chars().all(|c| c.is_ascii_alphanumeric()) {
                "<id>"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join(":")
}

pub fn norm_keys(w: &[(NodeKey, Value)]) -> Vec<String> {
    let mut v: Vec<String> = w.iter().map(|(k, _)| normalise(k)).collect();
    v.sort();
    v
}

fn def() -> AgentDef {
    AgentDef {
        name: "reviewer".parse().unwrap(),
        description: SENTINEL.into(),
        role: Role::Worker,
        tools: ToolPolicy::default(),
        model: ModelRef::Inherit,
        account: None,
        permission_mode: PermissionMode::Default,
        max_turns: None,
        max_tokens_total: None,
        max_cost_usd: None,
        effort: None,
        skills: vec![],
        hooks: Default::default(),
        memory: None,
        background: false,
        isolation: Isolation::None,
        omit_project_rules: false,
        color: None,
        checkpoints: vec![],
        router: None,
        initial_prompt: None,
        auto_compact_at: None,
        system_prompt: SENTINEL.into(),
        source: DefSource {
            path: None,
            scope: Scope::Builtin,
        },
    }
}

fn setup_hook(a: &mut A) {
    let h = aibus_core::state::value::map([
        ("id", s("h1")),
        ("event", s("stop")),
        ("run", Value::Null),
    ]);
    poke(a, "model:hooks:due", Value::List(vec![h].into()));
}

fn setup_router(a: &mut A) {
    let q = aibus_core::state::value::map([("fork", Value::I64(7))]);
    poke(a, "model:router:queue", Value::List(vec![q].into()));
}

fn setup_compacting(a: &mut A) {
    running(a);
    a.step(Msg::Command(Command::Compact { run: R() })).unwrap();
}

fn setup_queued(a: &mut A) {
    a.step(delegate()).unwrap();
}

fn setup_checkpoint(a: &mut A) {
    a.step(Msg::CheckpointHit {
        checkpoint: Checkpoint::BeforePlan,
        detail: SENTINEL.into(),
    })
    .unwrap();
}

fn cmd(c: Command, expect: Vec<&'static str>) -> Case {
    Case {
        name: "Command",
        setup: none,
        msg: Msg::Command(c),
        expect,
    }
}

fn cmd_on(setup: fn(&mut A), c: Command, expect: Vec<&'static str>) -> Case {
    Case {
        name: "Command",
        setup,
        msg: Msg::Command(c),
        expect,
    }
}

/// Every `Msg` variant at least once, with the exact keys its write set must contain besides
/// `eng:msg_seq` and `model:last_msg`, which every message writes.
pub fn cases() -> Vec<Case> {
    let t = SENTINEL.to_string();
    let mut v = vec![
        Case {
            name: "Key",
            setup: none,
            msg: Msg::Key(KeyEvent {
                code: KeyCode::Char('p'),
                ctrl: false,
                alt: false,
                shift: false,
            }),
            expect: vec![],
        },
        Case {
            name: "Mouse",
            setup: none,
            msg: Msg::Mouse(MouseEvent {
                kind: MouseKind::Down,
                col: 1,
                row: 2,
            }),
            expect: vec![],
        },
        Case {
            name: "Resize",
            setup: none,
            msg: Msg::Resize { cols: 98, rows: 46 },
            expect: vec!["ctrl:size"],
        },
        Case {
            name: "Tick",
            setup: none,
            msg: Msg::Tick,
            expect: vec!["eng:tick", "eng:ticks_dropped"],
        },
        Case {
            name: "SubmitPrompt",
            setup: none,
            msg: Msg::SubmitPrompt { text: t.clone() },
            expect: {
                let mut k = new_run_keys();
                k.push("eng:run_seq");
                k
            },
        },
        Case {
            name: "RunStarted",
            setup: none,
            msg: started(R()),
            expect: new_run_keys(),
        },
        Case {
            name: "RunStarted",
            setup: setup_queued,
            msg: started(first_delegated()),
            expect: vec!["model:log", "model:run:<id>:meta", "model:run:<id>:status"],
        },
        Case {
            name: "Stream",
            setup: running,
            msg: Msg::Stream {
                run: R(),
                chunks: vec![
                    StreamChunk::TextDelta(t.clone()),
                    StreamChunk::Usage(Usage {
                        output_tokens: 5,
                        ..Usage::default()
                    }),
                ],
            },
            expect: vec!["model:run:<id>:tail", "model:run:<id>:usage"],
        },
        Case {
            name: "Stream",
            setup: |a| a.step(started(R())).unwrap(),
            msg: text_stream(R(), "first"),
            expect: vec!["model:run:<id>:status", "model:run:<id>:tail"],
        },
        Case {
            name: "Stream",
            setup: running,
            msg: text_stream(R(), &"x\n".repeat(40 * 1024)),
            expect: vec!["model:run:<id>:blocks", "model:run:<id>:tail"],
        },
        Case {
            name: "ToolCallRequested",
            setup: running,
            msg: Msg::ToolCallRequested {
                run: R(),
                call: tool_call(false),
            },
            expect: vec!["model:log", "model:tools:pending"],
        },
        Case {
            name: "ToolCallFinished",
            setup: with_tool,
            msg: Msg::ToolCallFinished {
                run: R(),
                call_id: call(),
                result: ToolResult {
                    content: t.clone(),
                    is_error: false,
                },
                duration: Duration::from_millis(3),
            },
            expect: vec!["model:log", "model:tools:pending"],
        },
        Case {
            name: "PermissionNeeded",
            setup: with_tool,
            msg: Msg::PermissionNeeded {
                request: permission_request(),
            },
            expect: vec![
                "model:permissions:open",
                "model:run:<id>:status",
                "model:tools:pending",
            ],
        },
        Case {
            name: "PermissionDecided",
            setup: with_perm,
            msg: Msg::PermissionDecided {
                id: perm(),
                decision: Decision::Allow,
            },
            expect: vec![
                "model:permissions:open",
                "model:run:<id>:status",
                "model:tools:pending",
            ],
        },
        Case {
            name: "RunFinished",
            setup: running,
            msg: Msg::RunFinished {
                run: R(),
                result: RunResult {
                    run_id: R(),
                    agent: "worker".parse().unwrap(),
                    status: RunStatus::Completed,
                    turns: 4,
                    usage: Usage::default(),
                    summary: t.clone(),
                    summary_file: None,
                    duration: Duration::from_secs(1),
                    error: None,
                },
            },
            expect: vec![
                "model:log",
                "model:run:<id>:blocks",
                "model:run:<id>:meta",
                "model:run:<id>:progress",
                "model:run:<id>:status",
                "model:run:<id>:tail",
                "model:run:<id>:turns",
                "model:run:<id>:usage",
            ],
        },
        Case {
            name: "RunFailed",
            setup: running,
            msg: Msg::RunFailed {
                run: R(),
                error: RunError {
                    kind: RunErrorKind::Provider,
                    message: t.clone(),
                    retryable: true,
                },
            },
            expect: vec!["model:log", "model:run:<id>:error", "model:run:<id>:status"],
        },
        Case {
            name: "UsageUpdate",
            setup: running,
            msg: Msg::UsageUpdate {
                run: R(),
                usage: Usage {
                    input_tokens: 9,
                    ..Usage::default()
                },
            },
            expect: vec!["model:run:<id>:usage"],
        },
        Case {
            name: "HookFired",
            setup: setup_hook,
            msg: Msg::HookFired {
                run: None,
                event: HookEvent::Stop,
                outcome: HookOutcome {
                    exit_code: 0,
                    message: Some(t.clone()),
                },
            },
            expect: vec!["model:hooks:due", "model:log"],
        },
        Case {
            name: "RouterAnswered",
            setup: setup_router,
            msg: Msg::RouterAnswered {
                fork: ForkId(7),
                question: RouterQuestion {
                    kind: "which_file".into(),
                    prompt: t.clone(),
                },
                answer: RouterAnswer {
                    choice: t.clone(),
                    p: 0.9,
                    verdict: Verdict::Sharp,
                },
            },
            expect: vec![
                "model:log",
                "model:router:forks",
                "model:router:last:which_file",
                "model:router:queue",
            ],
        },
        Case {
            name: "AdvisorSpoke",
            setup: setup_checkpoint,
            msg: Msg::AdvisorSpoke {
                checkpoint: Checkpoint::BeforePlan,
                advice: t.clone(),
                tokens_read: 1000,
            },
            expect: vec!["model:advisor:advice", "model:advisor:state", "model:log"],
        },
        Case {
            name: "CheckpointHit",
            setup: none,
            msg: Msg::CheckpointHit {
                checkpoint: Checkpoint::SameErrorX2,
                detail: t.clone(),
            },
            expect: vec!["model:advisor:state", "model:log"],
        },
        Case {
            name: "Compacted",
            setup: setup_compacting,
            msg: Msg::Compacted {
                run: R(),
                pre_tokens: 90_000,
            },
            expect: vec!["model:log", "model:run:<id>:status"],
        },
        Case {
            name: "HandoffWritten",
            setup: none,
            msg: Msg::HandoffWritten {
                path: t.clone().into(),
            },
            expect: vec!["model:handoff:latest", "model:log"],
        },
        Case {
            name: "AgentsReloaded",
            setup: none,
            msg: Msg::AgentsReloaded {
                defs: vec![def()],
                warnings: vec![t.clone()],
            },
            expect: vec!["model:agents:defs", "model:agents:warnings", "model:log"],
        },
        Case {
            name: "AccountsReloaded",
            setup: none,
            msg: Msg::AccountsReloaded {
                accounts: vec![AccountView {
                    name: AccountName("anthropic-work".into()),
                    provider: "anthropic".into(),
                    label: "work".into(),
                    default_model: None,
                    key_ref: "keyring:aibus/anthropic-work".into(),
                }],
            },
            expect: vec!["model:accounts", "model:log"],
        },
        Case {
            name: "IoError",
            setup: none,
            msg: Msg::IoError {
                context: t.clone(),
                error: t.clone(),
            },
            expect: vec!["model:log"],
        },
        Case {
            name: "Shutdown",
            setup: none,
            msg: Msg::Shutdown {
                reason: ShutdownReason::Signal,
            },
            expect: vec!["eng:shutdown"],
        },
        Case {
            name: "Persisted",
            setup: none,
            msg: Msg::Persisted,
            expect: vec!["eng:snapshot_ms", "eng:snapshot_seq"],
        },
    ];
    let _ = &t;
    // Every `Command` variant.
    v.extend([
        cmd(
            Command::Delegate {
                agent: "worker".parse().unwrap(),
                prompt: SENTINEL.into(),
                background: true,
                isolation: Isolation::Worktree,
                parent: None,
            },
            {
                let mut k = new_run_keys();
                k.push("eng:run_seq");
                k
            },
        ),
        cmd(
            Command::Fork {
                prompt: SENTINEL.into(),
                parent: None,
            },
            {
                let mut k = new_run_keys();
                k.push("eng:run_seq");
                k
            },
        ),
        cmd_on(
            running,
            Command::Cancel { run: R() },
            vec!["model:run:<id>:cancel_requested"],
        ),
        cmd_on(
            setup_queued,
            Command::Cancel {
                run: first_delegated(),
            },
            vec!["model:run:<id>:cancel_requested", "model:run:<id>:status"],
        ),
        cmd_on(
            running,
            Command::Resume {
                run: R(),
                message: SENTINEL.into(),
            },
            vec!["model:run:<id>:status"],
        ),
        cmd_on(
            running,
            Command::SendMessage {
                run: R(),
                text: SENTINEL.into(),
            },
            vec!["model:log"],
        ),
        cmd_on(
            with_perm,
            Command::Permission {
                id: perm(),
                decision: Decision::Deny,
            },
            vec![
                "model:permissions:open",
                "model:run:<id>:status",
                "model:tools:pending",
            ],
        ),
        cmd_on(
            running,
            Command::Retry { run: R() },
            vec![
                "model:run:<id>:cancel_requested",
                "model:run:<id>:error",
                "model:run:<id>:status",
            ],
        ),
        cmd(Command::Focus(Pane::Cards), vec!["ctrl:focus"]),
        cmd(Command::Scroll(Pane::Log, 3), vec!["ctrl:scroll:log"]),
        cmd(
            Command::Select(Pane::Cards, Selection::Next),
            vec!["ctrl:selected:card"],
        ),
        cmd(
            Command::Select(Pane::Log, Selection::Index(4)),
            vec!["ctrl:selected:log"],
        ),
        cmd(Command::Select(Pane::Advisor, Selection::Next), vec![]),
        cmd(Command::OpenTranscript(R()), vec!["ctrl:modal"]),
        cmd(Command::CloseModal, vec!["ctrl:modal"]),
        cmd(Command::OpenPalette, vec!["ctrl:modal"]),
        cmd(Command::OpenHelp, vec!["ctrl:modal"]),
        cmd(
            Command::Workflow {
                name: SENTINEL.into(),
                input: SENTINEL.into(),
            },
            vec!["model:log"],
        ),
        cmd(
            Command::LedgerUpdate {
                section: "goal".into(),
                text: SENTINEL.into(),
            },
            vec!["model:ledger"],
        ),
        cmd(
            Command::CreateHandoff {
                outcome: Some(Outcome::PartialPlus),
            },
            vec!["model:log"],
        ),
        cmd_on(
            running,
            Command::Compact { run: R() },
            vec!["model:run:<id>:status"],
        ),
        cmd(Command::Clear, vec!["model:log"]),
        cmd(Command::Quit { force: true }, vec!["eng:shutdown"]),
    ]);
    v
}
