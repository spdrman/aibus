//! `aibus run`: one agent, headless, against the fake provider. Each `Msg` the run sends goes
//! through the Elm loop and out as one NDJSON line (the redacted summary, never the text).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use aibus_core::agent::discover::discover;
use aibus_core::replay::{msg_to_json, replay_hash};
use aibus_core::state::value::value_to_json;
use aibus_core::*;
use aibus_graph::mini::MiniGraph;
use aibus_provider::fake::{FakeProvider, Script, FAKE_KEY};
use aibus_provider::{AuthMethodKind, Credential, ModelRef, Secret};
use serde_json::json;
use tokio::sync::mpsc;

const DEFAULT_SCRIPT: &str = include_str!("../../../fixtures/scripts/explore.ron");

struct Args {
    agent: String,
    model: String,
    script: Option<PathBuf>,
    record: Option<PathBuf>,
    session_dir: Option<PathBuf>,
    prompt: String,
}

fn parse(args: &[String]) -> Result<Args, AibusError> {
    let usage = |m: &str| AibusError::Usage(m.to_owned());
    let mut out = Args {
        agent: "lead".into(),
        model: "fake-small".into(),
        script: None,
        record: None,
        session_dir: None,
        prompt: String::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| usage(&format!("{flag} needs a value")))
        };
        match a.as_str() {
            "--agent" => out.agent = value("--agent")?,
            "--model" => out.model = value("--model")?,
            "--script" => out.script = Some(value("--script")?.into()),
            "--record" => out.record = Some(value("--record")?.into()),
            "--session-dir" => out.session_dir = Some(value("--session-dir")?.into()),
            flag if flag.starts_with("--") => return Err(usage(&format!("unknown flag {flag}"))),
            _ if out.prompt.is_empty() => out.prompt = a.clone(),
            _ => return Err(usage("one prompt only: quote it")),
        }
    }
    if out.prompt.is_empty() {
        return Err(usage("aibus run [--agent NAME] [--model ID] [--script FILE] [--record DIR] <PROMPT>"));
    }
    Ok(out)
}

/// `YYYY-MM-DD` for a count of days since 1970-01-01 (proleptic Gregorian).
fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Returns the process exit code.
pub fn run(args: &[String]) -> Result<u8, AibusError> {
    let args = parse(args)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    rt.block_on(go(args))
}

async fn go(args: Args) -> Result<u8, AibusError> {
    let cwd = std::env::current_dir()?;
    let found = discover(None, &cwd, None, &[], &[])
        .map_err(|e| AibusError::Usage(e.to_string()))?;
    let name: AgentName = args
        .agent
        .parse()
        .map_err(|e: InvalidAgentName| AibusError::Usage(e.to_string()))?;
    let def = found
        .agents
        .get(&name)
        .ok_or_else(|| AibusError::Usage(format!("no agent named {name}")))?
        .clone();

    let script_text = match &args.script {
        Some(p) => std::fs::read_to_string(p)?,
        None => DEFAULT_SCRIPT.to_owned(),
    };
    let script = Script::from_ron(&script_text)
        .map_err(|e| AibusError::Usage(format!("script: {e}")))?;
    let provider = Arc::new(FakeProvider::new(script));

    let session_default = Some(ModelRef::Full(ModelId(args.model.as_str().into())));
    let defaults = ModelDefaults {
        default_model: session_default.clone(),
        advisor_model: session_default.clone(),
        router_model: session_default.clone(),
        swarm_model: session_default,
    };
    let model = resolve_model(&def.model, &defaults, None, &*provider, &Default::default())
        .ok_or_else(|| AibusError::Usage(format!("no model {}", args.model)))?;

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let session_dir = args
        .session_dir
        .unwrap_or_else(|| std::env::temp_dir().join(format!("aibus-{}", SessionId::new())));
    let run = RunId::new();
    let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
    let handle = spawn(
        RunConfig {
            run,
            def,
            prompt: args.prompt,
            parent: None,
            depth: 0,
            model,
            account: AccountName("fake".into()),
            cwd,
            runs_dir: session_dir.join("runs"),
            limits: ToolLimits::default(),
            date: civil_date((now.as_secs() / 86_400) as i64),
            data_home: None,
        },
        RunDeps {
            provider,
            creds: Arc::new(Credential {
                account: AccountName("fake".into()),
                secret: Secret::new(FAKE_KEY.to_owned()),
                kind: AuthMethodKind::None,
                resolved_at: Instant::now(),
            }),
            tools: Arc::new(ToolExecutor::new(ToolRegistry::read_only())),
            tx,
        },
    );

    let mut app = App::new(MiniGraph::new(), || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    });
    let mut recorded = String::new();
    let mut finished = None;
    while let Some(msg) = rx.recv().await {
        println!("{}", value_to_json(&msg.summary()));
        if let Some(line) = msg_to_json(&msg) {
            recorded.push_str(&line.to_string());
            recorded.push('\n');
        }
        app.step(msg.clone())?;
        if let Msg::RunFinished { result, .. } = msg {
            finished = Some(result);
            break;
        }
    }
    let _ = handle.join.await;
    let result = finished.ok_or_else(|| AibusError::Interrupted)?;

    println!(
        "{}",
        json!({
            "msg": "result",
            "status": result.status.as_value(),
            "turns": result.turns,
            "summary": result.summary,
            "summary_file": result.summary_file,
        })
    );

    if let Some(dir) = args.record {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("msgs.jsonl"), &recorded)?;
        std::fs::write(dir.join("script.ron"), &script_text)?;
        let hash = replay_hash(&recorded).map_err(AibusError::Usage)?;
        std::fs::write(dir.join("snapshot.hash"), format!("{hash}\n"))?;
    }

    match (result.status, result.error) {
        (_, Some(e)) if result.status == RunStatus::Failed => Err(e.into()),
        (RunStatus::Cancelled, _) => Err(AibusError::Interrupted),
        (RunStatus::Denied, _) => Err(AibusError::Denied("run denied".into())),
        _ => Ok(0),
    }
}
