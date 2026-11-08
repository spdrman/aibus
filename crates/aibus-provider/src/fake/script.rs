//! The fake's script: a queue of steps, built in a test or loaded from RON.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ProviderError;
use crate::provider::StopReason;

/// One thing the fake does. Owned strings, so a script can come from a RON file.
#[derive(Debug, Serialize, Deserialize)]
pub enum Step {
    /// Streamed in chunks of at most 8 bytes, never splitting a character.
    Text(String),
    ToolCall {
        name: String,
        input: serde_json::Value,
    },
    Usage {
        input: u64,
        output: u64,
    },
    /// Ends the turn with this error.
    Error(ProviderError),
    /// Sleeps on tokio's clock: instant when it is paused (`aibus_testkit::FakeClock`).
    Delay(Duration),
    /// When the request (system prompt and messages) contains `contains`, `then` runs next.
    MatchPrompt {
        contains: String,
        then: Vec<Step>,
    },
    /// Streams `{"choice":..,"p":..}`, plus a logprob chunk when the request asked for them.
    RouterAnswer {
        choice: String,
        p: f32,
    },
    /// Ends the turn with this reason. Without one, a turn ends when its steps run out:
    /// `ToolUse` if it called a tool, `EndTurn` otherwise.
    Stop(StopReason),
}

/// Steps are consumed across `stream` calls: one call runs until it reaches `Stop`, `Error`
/// or the end of the script. In RON a script is a plain list of steps.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Script {
    pub(crate) steps: VecDeque<Step>,
}

impl Script {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn then(mut self, step: Step) -> Self {
        self.steps.push_back(step);
        self
    }

    pub fn from_ron(src: &str) -> Result<Self, ron::error::SpannedError> {
        ron::from_str(src)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let src = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_ron(&src).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Steps not yet consumed.
    pub fn remaining(&self) -> usize {
        self.steps.len()
    }
}
