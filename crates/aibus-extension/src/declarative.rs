//! The declarative engine: for a CLI whose stdout is JSON lines and whose every line is one event,
//! the manifest's rules say which line is which event. It keeps no state across lines beyond the
//! counters of the attempt, which is exactly why it cannot parse Claude's stream (A1.5 E1). An
//! unmatched line is a `trace_warning`, never dropped silently; a line that is not a JSON object
//! is a `gap_detected`.

use std::time::{Duration, SystemTime};

use aibus_supervisor::{Channel, Ending, Engine};
use aibus_trace::{AttemptRef, Batch};
use serde_json::Value;

use crate::manifest::Rule;

pub const PARSER: &str = "declarative-jsonl/1";

pub struct Declarative {
    attempt: AttemptRef,
    rules: Vec<Rule>,
}

impl Declarative {
    pub fn new(attempt: AttemptRef, rules: Vec<Rule>) -> Self {
        Self { attempt, rules }
    }
}

impl Engine for Declarative {
    fn start(&mut self, at: SystemTime, info: Value) -> Batch {
        todo!("{at:?} {info:?} {:?}", self.attempt)
    }
    fn stream_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        todo!("{line:?} {at:?} {:?}", self.rules)
    }
    fn hook_line(&mut self, _line: &[u8], _at: SystemTime) -> Batch {
        Batch::default()
    }
    fn stderr_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        todo!("{line:?} {at:?}")
    }
    fn gap(&mut self, channel: Channel, reason: &str, bytes: u64, at: SystemTime) -> Batch {
        todo!("{channel:?} {reason} {bytes} {at:?}")
    }
    fn finish(&mut self, ending: Ending, wall: Duration, at: SystemTime) -> Batch {
        todo!("{ending:?} {wall:?} {at:?}")
    }
}
