//! A prompt typed into a session with no runs is the lead's first prompt: the reducer queues it,
//! the orchestrated effects hand it to the launcher, and nothing else is needed to start it. This
//! is the seam the TUI uses (C1.2), without the lead plan `aibus run` builds up front.

use std::sync::{Arc, Mutex};

use aibus_core::*;
use aibus_graph::mini::MiniGraph;
use tokio::sync::mpsc;

/// Records what it was asked to launch, and says it cannot (no provider in a unit test).
#[derive(Default)]
struct Recorder(Mutex<Vec<(SpawnSpec, String)>>);

impl Launcher for Recorder {
    fn launch(
        &self,
        spec: &SpawnSpec,
        prompt: String,
        _tx: mpsc::Sender<Msg>,
    ) -> Result<(RunConfig, RunDeps), String> {
        self.0.lock().unwrap().push((spec.clone(), prompt));
        Err("no provider in this test".into())
    }
}

#[tokio::test]
async fn the_first_prompt_starts_the_lead_through_the_launcher() {
    let mut app = App::new(MiniGraph::new(), || 1_700_000_000_000);
    let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
    let orch = Orchestrator::new(Limits::default(), tx);
    let launcher = Arc::new(Recorder::default());
    app.set_limits(&Limits::default()).unwrap();
    app.effects = Effects::orchestrated(orch, launcher.clone(), app.store.clone(), |_| {});

    app.step(Msg::SubmitPrompt { text: "build the thing".into() }).unwrap();

    {
        let launched = launcher.0.lock().unwrap();
        assert_eq!(launched.len(), 1, "one lead");
        let (spec, prompt) = &launched[0];
        assert_eq!(spec.agent.as_str(), "lead");
        assert_eq!((spec.parent, spec.depth), (None, 0));
        assert_eq!(prompt, "build the thing");
    }

    // A launcher that cannot build the run fails it where the session sees it.
    let told = rx.recv().await.expect("the orchestrator reports the failure");
    assert!(matches!(told, Msg::RunFailed { .. }), "{told:?}");

    // A second prompt goes to the lead that is already there, not to a second lead.
    app.step(Msg::SubmitPrompt { text: "and also this".into() }).unwrap();
    assert_eq!(launcher.0.lock().unwrap().len(), 1);
}
