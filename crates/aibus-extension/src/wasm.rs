//! The WASM guest host (#333). Stub: the red tests run against this.

use std::time::{Duration, SystemTime};

use aibus_supervisor::{Channel, Ending, Engine};
use aibus_trace::{AttemptRef, Batch};
use serde_json::Value;

use crate::declarative::Declarative;
use crate::manifest::UsageQuality;

pub const MAX_EVENTS_PER_CALL: usize = 64;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub fuel: u64,
    pub memory_bytes: usize,
    pub deadline: Duration,
    pub max_event_bytes: usize,
    pub max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fuel: 10_000_000,
            memory_bytes: 16 << 20,
            deadline: Duration::from_millis(500),
            max_event_bytes: 16 << 10,
            max_output_bytes: 4 << 20,
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum WasmError {
    #[error("not hosted yet")]
    NotHosted,
    #[error("compile: {0}")]
    Compile(String),
    #[error("import: {0}")]
    Import(String),
    #[error("export: {0}")]
    Export(String),
    #[error("memory: {0}")]
    Memory(String),
    #[error("instantiate: {0}")]
    Instantiate(String),
}

#[derive(Clone, Debug)]
pub struct Module;

impl Module {
    pub fn load(_bytes: &[u8]) -> Result<Self, WasmError> {
        Err(WasmError::NotHosted)
    }
}

pub struct Wasm {
    inner: Declarative,
}

impl Wasm {
    pub fn new(_attempt: AttemptRef, _module: &Module, _limits: Limits) -> Result<Self, WasmError> {
        Err(WasmError::NotHosted)
    }
}

impl Engine for Wasm {
    fn observe(&mut self, id: uuid::Uuid) {
        self.inner.observe(id);
    }
    fn start(&mut self, at: SystemTime, info: Value) -> Batch {
        self.inner.start(at, info)
    }
    fn stream_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        self.inner.stream_line(line, at)
    }
    fn hook_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        self.inner.hook_line(line, at)
    }
    fn stderr_line(&mut self, line: &[u8], at: SystemTime) -> Batch {
        self.inner.stderr_line(line, at)
    }
    fn gap(&mut self, channel: Channel, reason: &str, bytes: u64, at: SystemTime) -> Batch {
        self.inner.gap(channel, reason, bytes, at)
    }
    fn finish(&mut self, ending: Ending, wall: Duration, at: SystemTime) -> Batch {
        self.inner.finish(ending, wall, at)
    }
}

#[allow(dead_code)]
fn _unused(_: UsageQuality) {}
