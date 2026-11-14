//! The server's own question before a write or a run (E12, #107). When a client declared
//! `elicitation` (form mode), `workflow_create`, `workflow_update` and `workflow_run` ask the
//! user to retype the workflow name (and, for a run, to accept a cost under the process cap)
//! before anything is written or started. A client without the capability, and
//! `--confirm never`, keep the epic's behaviour: the flags are the only gate.
//!
//! Two shapes, one form:
//!
//! * `2025-11-25`: one `elicitation/create` sent to the client in the middle of the call.
//! * `2026-07-28`: server-initiated requests are gone, so the call answers `input_required` with
//!   the form and a `requestState`; the client retries the same call with `inputResponses` and
//!   the state untouched. The state is attacker-controlled input: it is an HMAC-SHA256 seal
//!   (rmcp's `RequestStateCodec`, key made per process and never stored) over a nonce, bound by
//!   its associated data to the tool, the exact arguments and the client's `clientInfo`, good for
//!   ten minutes and for one use.
//!
//! The form never carries a credential or anything shaped like one: `name` is a workflow name the
//! tool would have to accept anyway, and `max_cost_usd` is a number.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ElicitRequest,
    ElicitRequestParams, ElicitResult, ElicitationAction, ElicitationSchema, Implementation,
    InputRequest, InputRequiredResult, InputResponses, RequestStateCodec, SealOptions,
};
use rmcp::service::{RequestContext, RoleServer};
use serde_json::{json, Value};

use crate::Server;

/// The tools that change the disk or spend money, the ones the epic puts a human in front of.
const GATED: [&str; 3] = ["workflow_create", "workflow_update", "workflow_run"];

/// The live-session tools that change the session (#274): the same question, with the session's
/// short id to retype in place of a workflow name.
const SESSION_GATED: [&str; 4] = ["session_send", "session_command", "session_cancel", "session_permission"];

/// How much of a session id the form asks for.
const SESSION_SHORT: usize = 8;

/// How long a legacy client has to answer the form.
const ASK_TIMEOUT: Duration = Duration::from_secs(300);

/// One message for every way a `requestState` can fail (the codec's own advice: the detail is for
/// logs, not for the caller that may be forging it).
const REFUSED: &str = "the confirmation is not valid: it was edited, expired, already used or belongs to another call. \
Nothing was written or started. Call the tool again to be asked again.";

/// How `Server` confirms, set once at start. The three move together: `aibus mcp --confirm`
/// fills `elicit`, the run tools fill `run_cap_usd` with their process-wide cost cap, and the
/// tests shorten `ttl`.
#[derive(Clone, Debug)]
pub struct ConfirmConfig {
    /// `--confirm elicit` (the default) is true, `--confirm never` is false.
    pub elicit: bool,
    /// The cap a `workflow_run` confirmation may accept. `None` leaves the field out.
    pub run_cap_usd: Option<f64>,
    /// How long a `2026-07-28` `requestState` is good for.
    pub ttl: Duration,
}

impl Default for ConfirmConfig {
    fn default() -> Self {
        Self { elicit: true, run_cap_usd: None, ttl: Duration::from_secs(600) }
    }
}

pub(crate) struct Confirm {
    config: ConfirmConfig,
    codec: RequestStateCodec,
    /// Nonces already redeemed, with when. A state is single use; entries older than the ttl are
    /// dropped because the state they belong to has expired by then anyway.
    used: Mutex<HashMap<String, Instant>>,
}

impl Confirm {
    /// A fresh key every time: states from another process, or from before a `with_confirm`,
    /// do not open.
    pub(crate) fn new(config: ConfirmConfig) -> Self {
        let key = rand::random::<[u8; RequestStateCodec::MIN_KEY_LENGTH]>();
        let codec = RequestStateCodec::try_new(key.to_vec()).expect("32 bytes is the minimum key length");
        Self { config, codec, used: Mutex::new(HashMap::new()) }
    }
}

/// The bytes a state is bound to: tool, arguments (keys sorted, so their order on the wire does
/// not matter) and who is calling. They are mixed into the tag and not stored in it.
fn binding(tool: &str, args: &Value, client: Option<&Implementation>) -> Vec<u8> {
    fn sorted(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let sorted: BTreeMap<&String, Value> = m.iter().map(|(k, v)| (k, sorted(v))).collect();
                json!(sorted)
            }
            Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    let who = client.map_or(String::new(), |c| format!("{}\u{1}{}", c.name, c.version));
    format!("aibus-mcp-confirm\u{0}{tool}\u{0}{}\u{0}{who}", sorted(args)).into_bytes()
}

impl Server {
    /// Replaces how this server confirms (`--confirm`, the run cap, the ttl).
    pub fn with_confirm(mut self, config: ConfirmConfig) -> Self {
        self.confirm = Confirm::new(config);
        self
    }

    /// The form to put in front of the user for this call, or `None` when no question is due:
    /// confirmation is off, the tool is not one of the three, or it has no `name` to retype (the
    /// tool will refuse that call on its own).
    ///
    /// `name` is a string of exactly the workflow's length. rmcp's form schema has no `pattern`
    /// keyword, so the exact text is enforced here on the answer; the length bounds only stop a
    /// form that cannot match.
    pub fn confirmation_form(&self, tool: &str, args: &Value) -> Option<ElicitRequestParams> {
        if !self.confirm.config.elicit || !(GATED.contains(&tool) || SESSION_GATED.contains(&tool)) {
            return None;
        }
        let name = self.confirmation_target(tool, args)?;
        let name = name.as_str();
        let len = name.chars().count();
        let session = SESSION_GATED.contains(&tool);
        let (title, description) = match session {
            true => ("Session", "Type the session id shown above again to confirm."),
            false => ("Workflow name", "Type the name of the workflow again to confirm."),
        };
        let mut properties = json!({
            "name": {
                "type": "string",
                "title": title,
                "description": description,
                "minLength": len,
                "maxLength": len,
            },
        });
        let mut required = vec!["name"];
        let cap = self.confirm.config.run_cap_usd.filter(|_| tool == "workflow_run");
        if let Some(cap) = cap {
            properties["max_cost_usd"] = json!({
                "type": "number",
                "title": "Cost cap (USD)",
                "description": format!("The most this run may spend, at most {cap} USD."),
                "minimum": 0,
                "maximum": cap,
            });
            required.push("max_cost_usd");
        }
        let schema = json!({ "type": "object", "properties": properties, "required": required });
        let schema = ElicitationSchema::from_json_schema(schema.as_object()?.clone()).ok()?;
        let message = match tool {
            "session_send" => format!("Confirm: send a prompt to the live aibus session {name}."),
            "session_command" => format!("Confirm: run a command in the live aibus session {name}."),
            "session_cancel" => format!("Confirm: cancel a run in the live aibus session {name}."),
            "session_permission" => format!("Confirm: answer a permission prompt of the live aibus session {name}."),
            "workflow_run" => format!("Confirm: start a run of the workflow {name}."),
            "workflow_update" => format!("Confirm: overwrite the workflow {name}."),
            _ => format!("Confirm: create the workflow {name}."),
        };
        Some(ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: self.redactor.redact(&message).into_owned(),
            requested_schema: schema,
        })
    }

    /// What the form asks the user to retype: the workflow's name, or the first eight characters
    /// of the live session's id.
    fn confirmation_target(&self, tool: &str, args: &Value) -> Option<String> {
        if SESSION_GATED.contains(&tool) {
            let id = self.session.as_ref()?.target().ok()?;
            return Some(id.chars().take(SESSION_SHORT).collect());
        }
        args.get("name")?.as_str().filter(|n| !n.is_empty()).map(str::to_owned)
    }

    /// Whether the user's answer lets the call go on. `Err` is the sentence for the `isError`
    /// result and names the user's choice.
    pub fn confirmation_check(&self, tool: &str, args: &Value, answer: &ElicitResult) -> Result<(), String> {
        match answer.action {
            ElicitationAction::Accept => {}
            ElicitationAction::Decline => return Err("the user declined, so nothing was written or started".into()),
            _ => return Err("the user cancelled, so nothing was written or started".into()),
        }
        let content = answer.content.as_ref().and_then(Value::as_object);
        let typed = content.and_then(|c| c.get("name")).and_then(Value::as_str);
        let want = self.confirmation_target(tool, args);
        if typed.is_none() || typed != want.as_deref() {
            return Err(match SESSION_GATED.contains(&tool) {
                true => "the id typed in the confirmation is not the session's, so nothing was sent".into(),
                false => "the name typed in the confirmation is not the workflow's name, so nothing was written or started".into(),
            });
        }
        if let Some(cap) = self.confirm.config.run_cap_usd.filter(|_| tool == "workflow_run") {
            let cost = content.and_then(|c| c.get("max_cost_usd")).and_then(Value::as_f64);
            if !cost.is_some_and(|c| c > 0.0 && c <= cap) {
                return Err(format!(
                    "the confirmed cost cap is missing or not between 0 and {cap} USD, so nothing was started"
                ));
            }
        }
        Ok(())
    }

    /// The `2026-07-28` first leg: the form and a sealed `requestState`.
    pub fn confirmation_ask(
        &self,
        tool: &str,
        args: &Value,
        client: Option<&Implementation>,
    ) -> Option<InputRequiredResult> {
        let form = self.confirmation_form(tool, args)?;
        let bound = binding(tool, args, client);
        let state = self.confirm.codec.seal_with(
            ulid::Ulid::new().to_string().as_bytes(),
            &SealOptions::new().associated_data(&bound).ttl(self.confirm.config.ttl),
        );
        let asks = BTreeMap::from([("confirm".to_owned(), InputRequest::Elicitation(ElicitRequest::new(form)))]);
        Some(InputRequiredResult::new(Some(asks), Some(state)))
    }

    /// The `2026-07-28` retry: the state must open for this tool, these arguments and this
    /// client, must not have been used, and the answer must pass `confirmation_check`. A state
    /// is spent the moment it opens, whatever the answer was.
    pub fn confirmation_resume(
        &self,
        tool: &str,
        args: &Value,
        client: Option<&Implementation>,
        state: &str,
        answers: Option<&InputResponses>,
    ) -> Result<(), String> {
        let nonce = self.confirm.codec.open_with(state, &binding(tool, args, client)).map_err(|_| REFUSED.to_owned())?;
        {
            let mut used = self.confirm.used.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            used.retain(|_, at| now.duration_since(*at) < self.confirm.config.ttl);
            if used.insert(String::from_utf8_lossy(&nonce).into_owned(), now).is_some() {
                return Err(REFUSED.to_owned());
            }
        }
        let answer = answers
            .and_then(|a| a.get("confirm"))
            .ok_or("the retry carried no answer to the confirmation, so nothing was written or started")?;
        let answer: ElicitResult = serde_json::from_value(answer.clone())
            .map_err(|_| "the answer to the confirmation is not an elicitation result".to_owned())?;
        self.confirmation_check(tool, args, &answer)
    }

    /// The step `call_tool` runs before `call`: `None` lets the call go on, `Some(response)` is
    /// the whole answer (`input_required`, or an `isError` result that says why).
    ///
    /// A client that did not declare `elicitation` is never asked, and neither is a call to a
    /// tool that is off (the `-32602` for that comes from `call`).
    pub(crate) async fn confirm(
        &self,
        request: &CallToolRequestParams,
        context: &RequestContext<RoleServer>,
        client: Option<&Implementation>,
        addr: Option<SocketAddr>,
    ) -> Option<CallToolResponse> {
        let tool = request.name.as_ref();
        let gated = GATED.contains(&tool) || SESSION_GATED.contains(&tool);
        if !self.confirm.config.elicit || !gated || !self.tools().iter().any(|t| t.name == tool) {
            return None;
        }
        let declared = context.client_capabilities().and_then(|c| c.elicitation);
        if !declared.is_some_and(|e| e.form.is_some() || e.url.is_none()) {
            return None;
        }
        let args = request.arguments.clone().map_or(Value::Null, Value::Object);
        let form = self.confirmation_form(tool, &args)?;
        let started = Instant::now();
        let stateless = context.protocol_version().is_some_and(|v| !v.has_initialize());

        let verdict = if stateless {
            match request.request_state.as_deref() {
                None => {
                    let asked = self.confirmation_ask(tool, &args, client)?;
                    self.audit.record(client, addr, "confirm/ask", Some(tool), None, None, &args, true, started.elapsed());
                    return Some(CallToolResponse::InputRequired(asked));
                }
                Some(state) => {
                    self.confirmation_resume(tool, &args, client, state, request.input_responses.as_ref())
                }
            }
        } else {
            match context.peer.create_elicitation_with_timeout(form, Some(ASK_TIMEOUT)).await {
                Ok(answer) => self.confirmation_check(tool, &args, &answer),
                Err(e) => Err(format!(
                    "the client did not answer the confirmation ({}), so nothing was written or started",
                    self.redactor.redact(&e.to_string())
                )),
            }
        };
        match verdict {
            Ok(()) => None,
            Err(why) => {
                self.audit.record(client, addr, "confirm/refused", Some(tool), None, None, &args, false, started.elapsed());
                Some(CallToolResponse::from(CallToolResult::error(vec![ContentBlock::text(why)])))
            }
        }
    }
}
