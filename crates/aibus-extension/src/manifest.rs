//! The manifest contract (`aibus-extension.toml`, schema version 1). Strict: an unknown key, a
//! wrong type or a value outside the rules below is a refusal, never a guess. The JSON schema in
//! `docs/schemas/extension/manifest-v1.json` says the same in the schema's own language.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const MANIFEST_FILE: &str = "aibus-extension.toml";
/// A manifest larger than this is not read.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// The schema versions this host reads. Schema 2 adds `[[execution.typed]]` (#334).
pub const SCHEMAS: [&str; 2] = ["1", "2"];

/// The capability vector of spec 3, plus `hook_channel` (A1.5 amendment E5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    LoginStatus,
    OneShot,
    StructuredEvents,
    NativeChildLineage,
    ToolEvents,
    UsageReport,
    SessionResume,
    InteractiveApproval,
    Cancel,
    WorkspaceControl,
    Headless,
    SupportedOs,
    HookChannel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    Supported,
    /// Decided at install and at launch from what the real CLI's `--help` lists.
    ProbeRequired,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageQuality {
    Reported,
    Estimated,
    Unknown,
}

/// Who reads the CLI's events. The built-in engine is the host's own Claude parser (stateful: it
/// keeps tool-use to agent maps, dedupes across channels and has a `finish` step, which a
/// declarative manifest cannot express: amendment E1). The declarative engine is for CLIs whose
/// events are one line, one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    #[serde(rename = "builtin:claude-stream-json")]
    ClaudeStreamJson,
    #[serde(rename = "declarative-jsonl")]
    DeclarativeJsonl,
    #[serde(rename = "wasm-module")]
    WasmModule,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    MessageChunk,
    ToolStarted,
    ToolCompleted,
    ToolFailed,
    UsageReported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// JSON pointer to a string in the line (`/type`).
    pub pointer: String,
    pub equals: String,
    pub kind: RuleKind,
    /// Pointer to the tool use id, for the tool kinds.
    pub id_pointer: Option<String>,
    /// Pointers to token counts, for `usage_reported`. A missing number stays `unknown`.
    pub tokens_in_pointer: Option<String>,
    pub tokens_out_pointer: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Normalizer {
    pub engine: Engine,
    #[serde(default)]
    pub rule: Vec<Rule>,
    /// For `wasm-module`: the guest's path in the package.
    pub module: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Compat {
    /// The SDK protocol versions this extension speaks, a range such as `>=1.0.0, <2.0.0`.
    pub protocol: String,
    pub os: Vec<String>,
    /// The CLI versions it was written for, a range.
    pub cli_versions: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cli {
    /// File names only (`claude`), never paths. The host finds them on its own search path.
    pub executable_candidates: Vec<String>,
    /// Where the vendor documents the CLI, for provenance.
    pub vendor_urls: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    /// `vendor_cli`: the CLI owns its sign-in.
    pub owner: String,
    /// `forbidden`: aibus never reads, copies or stores the CLI's credentials.
    pub credential_access: String,
    pub login_argv: Vec<String>,
    pub status_argv: Vec<String>,
}

/// A flag added only when the probe found it and the capability it serves is wanted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionalFlag {
    pub flag: String,
    pub capability: Capability,
}

/// What a typed flag's value is. The host fills it from the plan; a manifest never writes one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    ModelId,
    AgentList,
    WorkdirPath,
}

/// A flag whose value the host supplies (schema 2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedFlag {
    pub flag: String,
    pub kind: ValueKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    /// Static tokens after the executable. No placeholder: the prompt goes on stdin (amendment E2).
    pub argv: Vec<String>,
    /// `stdin`, the only way a prompt reaches the CLI.
    pub prompt: String,
    /// `jsonl`.
    pub stdout: String,
    /// Environment names passed through from the host's environment, on top of the host's own
    /// allowlist (amendment E3). Credential-shaped names are refused.
    #[serde(default)]
    pub env_allow: Vec<String>,
    #[serde(default)]
    pub optional: Vec<OptionalFlag>,
    #[serde(default)]
    pub typed: Vec<TypedFlag>,
}

/// What the extension asks of the host's limits. The host's ceiling wins.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requested {
    pub wall_time_secs: Option<u64>,
    pub max_stdout_bytes: Option<u64>,
    pub max_line_bytes: Option<u64>,
    pub max_stderr_bytes: Option<u64>,
    pub lines_per_second: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub capability: Capability,
    /// Flags `--help` must list.
    pub flags: Vec<String>,
    /// The CLI version from which the capability exists.
    pub min_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    /// The extension's evidence is better with hook payloads. The host has no live way to receive
    /// them yet (A1.5 amendment E4), so a negotiation says `hook_channel` is unsupported and the
    /// stream is the only evidence.
    pub needs_delivery: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub usage_quality: UsageQuality,
}

/// The permission declaration. Every field has one acceptable value; a manifest that asks for
/// more is refused, so a reviewer reads one block and a later version that changes it is an
/// escalation the user has to accept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    pub spawn_only_declared_cli: bool,
    pub read_provider_credentials: bool,
    pub external_network_from_parser: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyInfo {
    pub owner: String,
    /// `YYYY-MM-DD`.
    pub review_date: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixtures {
    /// Recorded stdout streams, each with a sibling `<name>.expected.json`.
    pub files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notes {
    pub upgrade: String,
    pub rollback: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: String,
    pub id: String,
    pub version: String,
    pub name: String,
    /// `official_cli`.
    pub kind: String,
    pub provider: String,
    pub license: String,
    pub source_url: String,
    pub publisher: Publisher,
    pub compat: Compat,
    pub cli: Cli,
    pub auth: Auth,
    pub execution: Execution,
    pub limits: Option<Requested>,
    pub normalizer: Normalizer,
    pub capabilities: BTreeMap<Capability, Support>,
    #[serde(default)]
    pub probe: Vec<Probe>,
    pub hooks: Option<Hooks>,
    pub runtime: Runtime,
    pub permissions: Permissions,
    pub policy: PolicyInfo,
    pub fixtures: Fixtures,
    pub notes: Notes,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("the manifest is {0} bytes, the most read is {MAX_MANIFEST_BYTES}")]
    TooLarge(usize),
    #[error("the manifest is not valid TOML for schema 1: {0}")]
    Syntax(String),
    #[error("{field}: {why}")]
    Invalid { field: String, why: String },
}

/// Whether `id` is a well-formed extension id (reverse-DNS, lower case, three to eight labels).
pub fn valid_id(id: &str) -> bool {
    regex::Regex::new(r"^[a-z][a-z0-9-]{0,30}(\.[a-z][a-z0-9-]{0,30}){2,7}$")
        .expect("valid regex")
        .is_match(id)
}

fn bad(field: &str, why: impl Into<String>) -> ManifestError {
    ManifestError::Invalid {
        field: field.to_owned(),
        why: why.into(),
    }
}

fn text(field: &str, s: &str, max: usize) -> Result<(), ManifestError> {
    // Characters that confuse a terminal or a reader: controls, bidi overrides, zero-width marks.
    let invisible = |c: char| {
        c.is_control()
            || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
    };
    if s.trim().is_empty() || s.chars().count() > max || s.chars().any(invisible) {
        return Err(bad(
            field,
            format!("must be 1 to {max} printable characters"),
        ));
    }
    Ok(())
}

fn matches_re(field: &str, re: &str, s: &str, what: &str) -> Result<(), ManifestError> {
    let re = regex::Regex::new(re).expect("valid regex");
    if re.is_match(s) {
        Ok(())
    } else {
        Err(bad(field, format!("{s:?} is not {what}")))
    }
}

fn https(field: &str, s: &str) -> Result<(), ManifestError> {
    matches_re(
        field,
        r"(?i)^https://[a-z0-9]([a-z0-9.-]{0,98}[a-z0-9])?(:\d{1,5})?(/[A-Za-z0-9._~!$&'()*+,;=:@%/?#-]{0,200})?$",
        s,
        "an https URL with no credentials",
    )
}

/// The first token that means "run without asking", in any spelling: a flag whose name says it
/// (`--full-auto`, `--yolo`, `-y`, `--allowedTools=Bash`), a mode flag given a value that skips the
/// prompts (`--permission-mode acceptEdits`, `--ask-for-approval=never`, `-s danger-full-access`),
/// with the value inline or as the next token, and those values on their own. It is a denylist of
/// what is known, not a proof: the user sees the whole argv at install, and the real boundary is
/// the host's allowlist of programs (`sandbox::OFFICIAL_CLIS`).
pub(crate) fn unattended(tokens: &[String]) -> Option<&str> {
    // Flags that say it in their name, with any value.
    const NAMES: [&str; 20] = [
        "dangerously",
        "skip-permissions",
        "yolo",
        "bypass",
        "no-sandbox",
        "auto-approve",
        "auto-accept",
        "approve-all",
        "yes-always",
        "allow-all",
        "allow-tools",
        "full-auto",
        "trust-all",
        "trust-tools",
        "allowedtools",
        "allowed-tools",
        "allowed_tools",
        "auto-edit",
        "permission-prompt-tool",
        "dangerously-skip",
    ];
    // Flags that change what runs or what is reachable, so a manifest does not pass them.
    const NEVER: [&str; 3] = ["settings", "mcp-config", "add-dir"];
    // Flags that take a mode, and the modes that skip the prompts.
    const MODE_FLAGS: [&str; 9] = [
        "permission-mode",
        "approval-mode",
        "approval-policy",
        "ask-for-approval",
        "sandbox",
        "sandbox-mode",
        "profile",
        "a",
        "s",
    ];
    const RISKY_VALUES: [&str; 14] = [
        "acceptedits",
        "bypasspermissions",
        "dontask",
        "never",
        "yolo",
        "auto",
        "autoedit",
        "auto-edit",
        "auto-edit",
        "on-failure",
        "danger-full-access",
        "full-access",
        "unrestricted",
        "full-auto",
    ];
    const ALWAYS_RISKY: [&str; 4] = [
        "acceptedits",
        "bypasspermissions",
        "dontask",
        "danger-full-access",
    ];
    // `--config key=value` and `-c key=value` set what the CLI would ask about.
    let sets_policy = |v: &str| {
        let v = v.to_ascii_lowercase().replace('_', "-");
        [
            "approval-policy",
            "sandbox-mode",
            "approval-mode",
            "full-auto",
        ]
        .iter()
        .any(|k| v.contains(k))
    };
    for (i, t) in tokens.iter().enumerate() {
        let is_flag = t.starts_with('-');
        let body = t.trim_start_matches('-');
        let (name, inline) = body
            .split_once('=')
            .map_or((body, None), |(n, v)| (n, Some(v)));
        let name = name.to_ascii_lowercase();
        let next = tokens
            .get(i + 1)
            .filter(|n| !n.starts_with('-'))
            .map(String::as_str);
        let value = inline.or(next).unwrap_or_default();
        let normal = value.to_ascii_lowercase().replace('_', "-");
        if NAMES.iter().any(|w| name.contains(w))
            || NEVER.contains(&name.as_str())
            || (is_flag && matches!(name.as_str(), "y" | "yes"))
        {
            return Some(t);
        }
        if is_flag && MODE_FLAGS.contains(&name.as_str()) && RISKY_VALUES.contains(&normal.as_str())
        {
            return Some(t);
        }
        if is_flag && matches!(name.as_str(), "config" | "c") && sets_policy(value) {
            return Some(t);
        }
        // A short flag with its value attached: `-anever`, `-sdanger-full-access`, `-capproval_policy=never`.
        if is_flag && !t.starts_with("--") && body.len() > 1 {
            let (short, rest) = body.split_at(1);
            let rest = rest
                .trim_start_matches('=')
                .to_ascii_lowercase()
                .replace('_', "-");
            if (matches!(short, "a" | "s") && RISKY_VALUES.contains(&rest.as_str()))
                || (short == "c" && sets_policy(&rest))
            {
                return Some(t);
            }
        }
        if ALWAYS_RISKY.contains(&t.to_ascii_lowercase().as_str()) {
            return Some(t);
        }
    }
    None
}

/// Whether `name` is a program that is never an official CLI: an interpreter, a shell, a package
/// manager, a fetcher, a privilege or remote-execution tool. A trailing version is ignored
/// (`python3.12`, `pip3`). Used on a manifest's executable names, on the user's additions to the
/// allowlist and on what an allowlisted name resolves to.
pub fn forbidden_program(name: &str) -> bool {
    const FORBIDDEN: [&str; 52] = [
        "sh",
        "bash",
        "zsh",
        "fish",
        "dash",
        "ksh",
        "csh",
        "tcsh",
        "python",
        "perl",
        "ruby",
        "node",
        "deno",
        "bun",
        "php",
        "lua",
        "java",
        "npx",
        "npm",
        "pnpm",
        "yarn",
        "pip",
        "pipx",
        "uv",
        "gem",
        "bundle",
        "cargo",
        "go",
        "brew",
        "apt",
        "apt-get",
        "dpkg",
        "curl",
        "wget",
        "env",
        "sudo",
        "doas",
        "su",
        "ssh",
        "scp",
        "sftp",
        "nc",
        "ncat",
        "netcat",
        "socat",
        "busybox",
        "xargs",
        "osascript",
        "pwsh",
        "powershell",
        "cmd",
        "open",
    ];
    let lower = name.to_ascii_lowercase();
    let base = lower.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    FORBIDDEN.contains(&lower.as_str()) || FORBIDDEN.contains(&base)
}

fn argv_tokens(
    field: &str,
    tokens: &[String],
    max: usize,
    official: &[&str],
    shape: bool,
) -> Result<(), ManifestError> {
    if tokens.len() > max {
        return Err(bad(field, format!("more than {max} tokens")));
    }
    // The host's list of flags for every official CLI the manifest names (#339).
    for cli in official {
        crate::flags::check(cli, tokens).map_err(|why| bad(field, why))?;
    }
    for t in tokens {
        matches_re(
            field,
            r"^-{0,2}[A-Za-z0-9][A-Za-z0-9_.:=,%+-]{0,127}$",
            t,
            "a plain token (no space, slash, quote, $, `, ~, @ or braces)",
        )?;
        // The shapes of an install, an eval and a one-liner. Only for a program that is not one of
        // the host's own official CLIs: `codex exec --json` is Codex's own headless mode.
        if shape
            && matches!(
                t.as_str(),
                "install"
                    | "uninstall"
                    | "add"
                    | "exec"
                    | "eval"
                    | "-c"
                    | "-e"
                    | "-m"
                    | "--eval"
                    | "--exec"
                    | "--command"
            )
        {
            return Err(bad(
                field,
                format!("{t:?} is the shape of an install, an exec or an eval"),
            ));
        }
    }
    if let Some(t) = unattended(tokens) {
        return Err(bad(field, format!("{t:?} would run without asking")));
    }
    Ok(())
}

fn range(field: &str, s: &str) -> Result<crate::version::Range, ManifestError> {
    crate::version::Range::parse(s).map_err(|why| bad(field, why))
}

impl Manifest {
    pub fn parse(text_in: &str) -> Result<Self, ManifestError> {
        if text_in.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge(text_in.len()));
        }
        // The schema first, on its own: a manifest of a schema this host does not read is refused
        // by name, not by whichever of its keys this host happens to have never heard of.
        let table: toml::Table =
            toml::from_str(text_in).map_err(|e| ManifestError::Syntax(e.message().to_owned()))?;
        if let Some(v) = table.get("schema_version").and_then(toml::Value::as_str) {
            if !SCHEMAS.contains(&v) {
                return Err(bad(
                    "schema_version",
                    format!(
                        "this host reads schemas {:?} and {:?}, not {v:?}; a newer aibus may read it",
                        SCHEMAS[0], SCHEMAS[1]
                    ),
                ));
            }
        }
        let m: Self =
            toml::from_str(text_in).map_err(|e| ManifestError::Syntax(e.message().to_owned()))?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if !valid_id(&self.id) {
            return Err(bad(
                "id",
                "must be a lower-case reverse-DNS id with at least three labels",
            ));
        }
        // Amendment E6: no vendor product name in an id or a name.
        let squashed = |s: &str| {
            s.chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_ascii_lowercase()
        };
        for (field, value) in [("id", &self.id), ("name", &self.name)] {
            if squashed(value).contains("claudecode") {
                return Err(bad(
                    field,
                    "must not contain the vendor's product name \"Claude Code\"",
                ));
            }
        }
        if crate::version::exact(&self.version).is_none() {
            return Err(bad("version", "must be exactly major.minor.patch"));
        }
        text("name", &self.name, 64)?;
        if self.kind != "official_cli" {
            return Err(bad("kind", "the only kind is official_cli"));
        }
        text("provider", &self.provider, 64)?;
        text("license", &self.license, 100)?;
        https("source_url", &self.source_url)?;
        text("publisher.name", &self.publisher.name, 64)?;
        https("publisher.url", &self.publisher.url)?;

        range("compat.protocol", &self.compat.protocol)?;
        range("compat.cli_versions", &self.compat.cli_versions)?;
        // A host that speaks protocol 1.0.0 does not read schema 2, and says so by not finding
        // itself in this range, before it looks at a key it does not know.
        let schema_2 = self.schema_version == "2";
        if schema_2
            && range("compat.protocol", &self.compat.protocol)?
                .contains(&crate::version::Version(1, 0, 0))
        {
            return Err(bad(
                "compat.protocol",
                "a schema 2 manifest has to ask for protocol 1.1.0 or later, so a host that speaks 1.0.0 refuses it",
            ));
        }
        if self.compat.os.is_empty()
            || self.compat.os.len() > 3
            || self
                .compat
                .os
                .iter()
                .any(|o| !matches!(o.as_str(), "macos" | "linux" | "windows"))
        {
            return Err(bad("compat.os", "one to three of macos, linux, windows"));
        }

        let exes = &self.cli.executable_candidates;
        if exes.is_empty() || exes.len() > 4 {
            return Err(bad("cli.executable_candidates", "one to four file names"));
        }
        for e in exes {
            if forbidden_program(e) {
                return Err(bad(
                    "cli.executable_candidates",
                    format!("{e} is an interpreter, shell, package manager or fetcher, never an official CLI"),
                ));
            }
            matches_re(
                "cli.executable_candidates",
                r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$",
                e,
                "a plain file name (no path, no leading dot)",
            )?;
        }
        if self.cli.vendor_urls.is_empty() || self.cli.vendor_urls.len() > 8 {
            return Err(bad("cli.vendor_urls", "one to eight vendor URLs"));
        }
        for u in &self.cli.vendor_urls {
            https("cli.vendor_urls", u)?;
        }

        if self.auth.owner != "vendor_cli" {
            return Err(bad("auth.owner", "the vendor CLI owns its sign-in"));
        }
        if self.auth.credential_access != "forbidden" {
            return Err(bad("auth.credential_access", "must be \"forbidden\""));
        }
        let official: Vec<&str> = exes
            .iter()
            .map(String::as_str)
            .filter(|e| crate::sandbox::OFFICIAL_CLIS.contains(e))
            .collect();
        let shape = official.len() != exes.len();
        argv_tokens(
            "auth.login_argv",
            &self.auth.login_argv,
            16,
            &official,
            shape,
        )?;
        argv_tokens(
            "auth.status_argv",
            &self.auth.status_argv,
            16,
            &official,
            shape,
        )?;

        let ex = &self.execution;
        argv_tokens("execution.argv", &ex.argv, 32, &official, shape)?;
        if ex.prompt != "stdin" {
            return Err(bad(
                "execution.prompt",
                "the prompt goes on stdin, never in argv",
            ));
        }
        if ex.stdout != "jsonl" {
            return Err(bad("execution.stdout", "the only stdout format is jsonl"));
        }
        if ex.env_allow.len() > 16 {
            return Err(bad("execution.env_allow", "more than 16 names"));
        }
        for n in &ex.env_allow {
            matches_re(
                "execution.env_allow",
                r"^[A-Z][A-Z0-9_]{0,63}$",
                n,
                "an upper-case name",
            )?;
            // Names the host never passes on an extension's say-so, on top of `bounded_env`'s
            // credential, session and PATH rules: aibus's own, and the variables that make a
            // process load someone's code.
            let private = n.starts_with("AIBUS_")
                || n.starts_with("LD_")
                || n.starts_with("DYLD_")
                || matches!(
                    n.as_str(),
                    "NODE_OPTIONS"
                        | "PYTHONPATH"
                        | "PYTHONSTARTUP"
                        | "PERL5OPT"
                        | "RUBYOPT"
                        | "BASH_ENV"
                        | "ENV"
                        | "IFS"
                        | "SHELL"
                );
            if private
                || aibus_supervisor::cli::bounded_env(std::iter::empty(), &[n.as_str()], &[])
                    .is_err()
            {
                return Err(bad(
                    "execution.env_allow",
                    format!("{n} is never passed to a CLI"),
                ));
            }
        }
        for o in &ex.optional {
            matches_re(
                "execution.optional",
                r"^--[a-z][a-z0-9-]{0,63}$",
                &o.flag,
                "a long flag",
            )?;
            for cli in &official {
                crate::flags::check(cli, std::slice::from_ref(&o.flag))
                    .map_err(|why| bad("execution.optional", why))?;
            }
            if unattended(std::slice::from_ref(&o.flag)).is_some() {
                return Err(bad(
                    "execution.optional",
                    format!("{} would run without asking", o.flag),
                ));
            }
            let listed = self
                .probe
                .iter()
                .any(|p| p.capability == o.capability && p.flags.contains(&o.flag));
            if !listed {
                return Err(bad(
                    "execution.optional",
                    format!(
                        "{} needs a [[probe]] for {:?} that lists it",
                        o.flag, o.capability
                    ),
                ));
            }
        }

        if !schema_2 && !ex.typed.is_empty() {
            return Err(bad(
                "execution.typed",
                "typed flags need schema 2 (schema_version = \"2\")",
            ));
        }
        if ex.typed.len() > 8 {
            return Err(bad("execution.typed", "more than 8 typed flags"));
        }
        for (i, t) in ex.typed.iter().enumerate() {
            matches_re(
                "execution.typed",
                r"^--[a-z][a-z0-9-]{0,63}$",
                &t.flag,
                "a long flag with no value",
            )?;
            if ex.typed[..i].iter().any(|o| o.flag == t.flag) {
                return Err(bad("execution.typed", format!("{} is typed twice", t.flag)));
            }
            // The host decides which flags can be typed, per CLI, and with which kind.
            for cli in exes {
                if crate::flags::typed(cli, &t.flag) != Some(t.kind) {
                    return Err(bad(
                        "execution.typed",
                        format!(
                            "{} as {:?} is not a typed flag the host has for {cli}",
                            t.flag, t.kind
                        ),
                    ));
                }
            }
        }
        // What the host fills is never also written by the manifest, in any spelling.
        if schema_2 {
            let groups = [
                &ex.argv,
                &self.auth.login_argv,
                &self.auth.status_argv,
                &ex.optional
                    .iter()
                    .map(|o| o.flag.clone())
                    .collect::<Vec<_>>(),
            ];
            for tokens in groups {
                if let Some(t) = exes
                    .iter()
                    .find_map(|cli| crate::flags::writes_typed(cli, tokens))
                {
                    return Err(bad(
                        "execution.argv",
                        format!(
                            "{t:?} is a flag the host fills (schema 2 typed), never a static token"
                        ),
                    ));
                }
            }
        }

        if let Some(l) = &self.limits {
            for (name, v) in [
                ("wall_time_secs", l.wall_time_secs),
                ("max_stdout_bytes", l.max_stdout_bytes),
                ("max_line_bytes", l.max_line_bytes),
                ("max_stderr_bytes", l.max_stderr_bytes),
                ("lines_per_second", l.lines_per_second.map(u64::from)),
            ] {
                if v == Some(0) {
                    return Err(bad(&format!("limits.{name}"), "must be above zero"));
                }
            }
        }

        let n = &self.normalizer;
        match n.engine {
            Engine::ClaudeStreamJson if !n.rule.is_empty() => {
                return Err(bad("normalizer.rule", "the built-in engine takes no rules"));
            }
            Engine::DeclarativeJsonl if n.rule.is_empty() || n.rule.len() > 64 => {
                return Err(bad(
                    "normalizer.rule",
                    "the declarative engine needs one to 64 rules",
                ));
            }
            Engine::WasmModule => {
                #[cfg(not(feature = "wasm"))]
                return Err(bad(
                    "normalizer.engine",
                    "this build of aibus is without WASM support (the `wasm` cargo feature is off)",
                ));
                #[cfg(feature = "wasm")]
                {
                    if !n.rule.is_empty() {
                        return Err(bad("normalizer.rule", "a WASM module takes no rules"));
                    }
                    match &n.module {
                        Some(m) => matches_re(
                            "normalizer.module",
                            r"^wasm/[A-Za-z0-9][A-Za-z0-9._-]{0,63}\.wasm$",
                            m,
                            "a .wasm file directly under wasm/",
                        )?,
                        None => {
                            return Err(bad(
                                "normalizer.module",
                                "the engine wasm-module names its module",
                            ))
                        }
                    }
                }
            }
            _ => {}
        }
        if n.module.is_some() && n.engine != Engine::WasmModule {
            return Err(bad(
                "normalizer.module",
                "only the engine wasm-module has a module",
            ));
        }
        for r in &n.rule {
            for p in [
                Some(&r.pointer),
                r.id_pointer.as_ref(),
                r.tokens_in_pointer.as_ref(),
                r.tokens_out_pointer.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                matches_re(
                    "normalizer.rule",
                    r"^(/[A-Za-z0-9_-]{1,32}){1,6}$",
                    p,
                    "a short JSON pointer",
                )?;
            }
            text("normalizer.rule", &r.equals, 128)?;
            let tool = matches!(
                r.kind,
                RuleKind::ToolStarted | RuleKind::ToolCompleted | RuleKind::ToolFailed
            );
            if r.id_pointer.is_some() && !tool {
                return Err(bad("normalizer.rule", "id_pointer is for the tool kinds"));
            }
            let usage = r.kind == RuleKind::UsageReported;
            if (r.tokens_in_pointer.is_some() || r.tokens_out_pointer.is_some()) && !usage {
                return Err(bad(
                    "normalizer.rule",
                    "token pointers are for usage_reported",
                ));
            }
        }

        for c in [
            Capability::LoginStatus,
            Capability::OneShot,
            Capability::StructuredEvents,
            Capability::NativeChildLineage,
            Capability::ToolEvents,
            Capability::UsageReport,
            Capability::SessionResume,
            Capability::InteractiveApproval,
            Capability::Cancel,
            Capability::WorkspaceControl,
            Capability::Headless,
            Capability::SupportedOs,
            Capability::HookChannel,
        ] {
            if !self.capabilities.contains_key(&c) {
                return Err(bad(
                    "capabilities",
                    format!("{c:?} is not declared; declare every capability"),
                ));
            }
        }
        for p in &self.probe {
            if self.capabilities.get(&p.capability) != Some(&Support::ProbeRequired) {
                return Err(bad(
                    "probe",
                    format!("{:?} has a probe but is not probe_required", p.capability),
                ));
            }
            if p.flags.is_empty() || p.flags.len() > 8 {
                return Err(bad("probe", "one to eight flags"));
            }
            for f in &p.flags {
                matches_re("probe", r"^--[a-z][a-z0-9-]{0,63}$", f, "a long flag")?;
            }
            if let Some(v) = &p.min_version {
                if crate::version::exact(v).is_none() {
                    return Err(bad("probe", "min_version must be major.minor.patch"));
                }
            }
        }
        for (c, s) in &self.capabilities {
            if *s == Support::ProbeRequired && !self.probe.iter().any(|p| p.capability == *c) {
                return Err(bad(
                    "probe",
                    format!("{c:?} is probe_required and has no [[probe]]"),
                ));
            }
        }
        // Amendment E5 and C1: native children live inside the lead process, so they cannot resume.
        let lineage = self.capabilities[&Capability::NativeChildLineage] != Support::Unsupported;
        if lineage && self.capabilities[&Capability::SessionResume] != Support::Unsupported {
            return Err(bad(
                "capabilities.session_resume",
                "native children die with the lead process, so session_resume must be unsupported",
            ));
        }

        let p = &self.permissions;
        for (name, ok) in [
            ("spawn_only_declared_cli", p.spawn_only_declared_cli),
            ("read_provider_credentials", !p.read_provider_credentials),
            (
                "external_network_from_parser",
                !p.external_network_from_parser,
            ),
        ] {
            if !ok {
                return Err(bad(
                    &format!("permissions.{name}"),
                    "this value is never granted",
                ));
            }
        }

        text("policy.owner", &self.policy.owner, 100)?;
        matches_re(
            "policy.review_date",
            r"^\d{4}-(0[1-9]|1[0-2])-(0[1-9]|[12]\d|3[01])$",
            &self.policy.review_date,
            "a date as YYYY-MM-DD",
        )?;
        if self.fixtures.files.is_empty() || self.fixtures.files.len() > 32 {
            return Err(bad("fixtures.files", "one to 32 fixtures"));
        }
        for f in &self.fixtures.files {
            matches_re(
                "fixtures.files",
                r"^fixtures/[A-Za-z0-9][A-Za-z0-9._-]{0,63}\.jsonl$",
                f,
                "a .jsonl file directly under fixtures/",
            )?;
        }
        text("notes.upgrade", &self.notes.upgrade, 500)?;
        text("notes.rollback", &self.notes.rollback, 500)?;
        Ok(())
    }
}

/// What `new` asks for that `old` did not: a new executable name, environment name, optional flag,
/// argv token, capability, engine, or larger requested limit. An update that returns a non-empty
/// list is a capability escalation: the user has to accept it by name, and nothing changes
/// until then.
pub fn escalations(old: &Manifest, new: &Manifest) -> Vec<String> {
    let mut out = Vec::new();
    let mut added = |prefix: &str, new: &[String], old: &[String]| {
        for n in new.iter().filter(|n| !old.contains(n)) {
            out.push(format!("{prefix}:{n}"));
        }
    };
    added(
        "executable",
        &new.cli.executable_candidates,
        &old.cli.executable_candidates,
    );
    added("env", &new.execution.env_allow, &old.execution.env_allow);
    let flags = |m: &Manifest| {
        m.execution
            .optional
            .iter()
            .map(|o| o.flag.clone())
            .collect::<Vec<_>>()
    };
    added("flag", &flags(new), &flags(old));
    let typed = |m: &Manifest| {
        m.execution
            .typed
            .iter()
            .map(|t| t.flag.clone())
            .collect::<Vec<_>>()
    };
    added("typed", &typed(new), &typed(old));
    added("argv", &new.execution.argv, &old.execution.argv);
    let auth = |m: &Manifest| [m.auth.login_argv.clone(), m.auth.status_argv.clone()].concat();
    added("auth_argv", &auth(new), &auth(old));
    let rank = |s: Support| match s {
        Support::Unsupported => 0,
        Support::ProbeRequired => 1,
        Support::Supported => 2,
    };
    for (c, s) in &new.capabilities {
        let before = old.capabilities.get(c).copied().map_or(0, rank);
        if rank(*s) > before {
            let name = serde_json::to_value(c)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned));
            out.push(format!("capability:{}", name.unwrap_or_default()));
        }
    }
    // An absent limit is the host's own, which can be larger than any number the old one named.
    let limit = |m: &Manifest| {
        let l = m.limits.clone().unwrap_or_default();
        [
            ("wall_time_secs", l.wall_time_secs),
            ("max_stdout_bytes", l.max_stdout_bytes),
            ("max_line_bytes", l.max_line_bytes),
            ("max_stderr_bytes", l.max_stderr_bytes),
            ("lines_per_second", l.lines_per_second.map(u64::from)),
        ]
    };
    for ((name, was), (_, now)) in limit(old).into_iter().zip(limit(new)) {
        if now.unwrap_or(u64::MAX) > was.unwrap_or(u64::MAX) {
            out.push(format!("limit:{name}"));
        }
    }
    if old.normalizer.engine != new.normalizer.engine {
        out.push(format!(
            "engine:{}",
            serde_json::to_value(new.normalizer.engine)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        ));
    }
    // New code under the same engine: a different module path.
    if new.normalizer.engine == old.normalizer.engine
        && new.normalizer.module != old.normalizer.module
    {
        out.push(format!(
            "module:{}",
            new.normalizer.module.clone().unwrap_or_default()
        ));
    }
    let needs = |m: &Manifest| m.hooks.as_ref().is_some_and(|h| h.needs_delivery);
    if needs(new) && !needs(old) {
        out.push("hooks:needs_delivery".to_owned());
    }
    out
}
