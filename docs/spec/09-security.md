# 09. Security

## 1. Credentials

Everything in 04 section 5 applies. In short:

1. Storage: OS keychain via `keyring`; `env` and `0600` file as explicit alternatives; nothing else.
2. In memory: `Secret<String>` with zeroising `Drop`, no `Serialize`, `Debug` prints `***`, resolved per request, cached at most 60 s.
3. Never in: config, agent definitions, the graph, snapshots, commit logs, `Msg` recordings, transcripts, logs, error messages, `--json` output, hook stdin, hook environment, tool environment.
4. Hooks and tools run with a scrubbed environment: every variable matching `*_KEY`, `*_TOKEN`, `*_SECRET`, `*_PASSWORD`, `AIBUS_ACCOUNT_*` is removed unless listed in `[hooks] pass_env`. The `bash` tool inherits the same scrubbed environment.

## 2. Tool allow-lists: boundary or advisory

The honest table. "Boundary" means the mechanism holds even if the model is adversarial. "Advisory" means it relies on the model cooperating.

| Mechanism | Kind | Notes |
|---|---|---|
| Tool allow-list (`tools`, `disallowed_tools`) | **boundary** inside aibus | The model only receives schemas for allowed tools, and `ToolExecutor::call` rejects any name not in the effective list before dispatch. The model cannot invoke a tool it was not given. |
| Tool specifiers (`bash(git *)`) | **boundary** inside aibus | Matched as a glob against the `command` argument after shell-word normalisation (collapse whitespace, strip leading env assignments). `bash(git *)` does not stop `git -c core.sshCommand=... push`; it stops `rm`. |
| `pre_tool_use` hooks exiting 2 | **boundary** inside aibus | Fail closed on timeout. |
| Permission prompts | **boundary** inside aibus | The call does not execute until a decision arrives. |
| `read` path restriction | **boundary** inside aibus | Paths are canonicalised; outside the project (or outside `read(/abs/**)` specifiers) is denied. Symlinks are resolved before the check. |
| `write`/`edit` path restriction | **boundary** inside aibus | Same, plus a deny-list of `.git/**`, `.aibus/settings.toml`, `~/.config/aibus/**` unless explicitly allowed. |
| System prompt instructions ("never edit files") | **advisory** | Useful, not security. |
| Anything a `bash` command does once it runs | **not a boundary in v1** | A process started by `bash` has the user's full rights. The allow-list decides whether it starts, not what it can touch. |

So: the tool policy is a real boundary at the aibus process boundary and nothing more. There is no OS sandbox in v1. The spec says this plainly and the `--help` for `--permission-mode bypass` repeats it. An optional OS sandbox for `bash` (`sandbox-exec` profiles on macOS, `bubblewrap` on Linux, both restricting the filesystem to the project and `$TMPDIR` and optionally cutting network) is M6 (11-delivery section 2), behind `[tools] sandbox = "bwrap"`, and until it lands `aibus doctor` prints `shell sandbox: none`.

## 3. Who runs shell commands, and with what confirmation

Only `ToolExecutor` runs commands, on the blocking pool, via `tokio::process::Command::new("sh").arg("-c").arg(command)` with the scrubbed environment, cwd set to the run's working directory (the worktree when `isolation: worktree`), a process group so cancellation kills children, `tools.bash_timeout_secs` enforced, stdout and stderr capped. Hooks run the same way with their own timeouts.

Permission modes decide confirmation:

| Mode | `read`/`grep`/`glob`/`ls` | `write`/`edit` | `bash`, `web_fetch`, `delegate` with `worktree` | Notes |
|---|---|---|---|---|
| `default` | allow | ask | ask | pre-approved patterns from `[tools] allow` and `--allow` skip the prompt |
| `accept_edits` | allow | allow | ask | |
| `plan` | allow | deny | deny | read-only; `plan` tool allowed |
| `dont_ask` | allow | deny if it would ask | deny if it would ask | for unattended runs that should fail closed |
| `bypass` | allow | allow | allow | needs `AIBUS_I_KNOW=1`; the status bar shows `BYPASS` in red the whole session |

"Always for this pattern" (`a` in the modal) adds a session-scoped allow entry; it is not persisted unless the user confirms `persist to .aibus/settings.toml`.

Background runs surface their prompts in the main session named with the run; a denied call returns `ToolDenied` to the run, it does not kill the run. The `audit` built-in runs in a worktree precisely so `bash(cargo *)` cannot touch the user's checkout.

Destructive command patterns are denied before the prompt even in `default`: `rm -rf /`, `rm -rf ~`, `rm -rf *`, `git push --force` to `main`/`master`, `git reset --hard` without a ref, `:(){ :|:& };:`, `mkfs`, `dd of=/dev/`. The list is `tools/shell_deny.txt`, extensible via `[tools] deny_patterns`, and a match is logged with the pattern name.

## 4. Redaction in logs and transcripts

A `redact` layer sits in front of every writer: tracing, transcript JSONL, `msgs.jsonl`, hook logs, `--json` output, error `Display`.

- Known-secret redaction: every credential currently resolved (section 1) is registered with the redactor for its lifetime; any occurrence of the exact value or its base64 in output is replaced with `[redacted:<account>]`. This is the reliable half.
- Pattern redaction: `sk-ant-[A-Za-z0-9_-]{20,}`, `sk-[A-Za-z0-9]{20,}`, `ghp_[A-Za-z0-9]{36}`, `gho_`, `github_pat_`, `xox[baprs]-`, `AKIA[0-9A-Z]{16}`, `-----BEGIN [A-Z ]*PRIVATE KEY-----` blocks, `Bearer [A-Za-z0-9._-]{20,}`, JWT shapes `eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.`, `password=`/`passwd=`/`token=` query and env assignments. Replaced with `[redacted:pattern:<name>]`. Patterns live in `redact/patterns.toml` and are tested against a corpus. This is the best-effort half; the spec does not claim it catches everything.
- Thinking blocks are written to transcripts only when `[persist] store_thinking = true` (default false); the model sees them regardless.
- `aibus session export` runs the same redactor again on the way out.

## 5. Child results and prompt injection

A worker's summary is data, not instructions (01 section 5.3): wrapped with a fixed header, markers escaped, capped. Hook stdout on `user_prompt_submit` and `session_start` is also data and is labelled `[hook <name>]`. Project rules files are trusted by definition (they are in the repo the user chose to run in), but `agents.extra_dirs` definitions are not: their hooks are stripped unless the directory is trusted, and their `permission_mode` is clamped to `default`.

Web content (`web_fetch`, M5) is wrapped the same way and never merged into a system prompt.

## 6. Hooks and definitions as code

A hook is a shell command the user configured. aibus treats `~/.config/aibus/` and `.aibus/` as trusted; it does not run hooks from `agents.extra_dirs` without `trusted_dirs`. `aibus agent import .claude/agents` copies definitions and prints every hook it found, disabled, with the command text, so the user can re-enable them deliberately.

Workflows can carry `gate` commands; same rule.

## 7. Filesystem

- Session directories are created `0700`; files `0600`.
- Worktrees are created from the default branch into the cache dir, never inside the project, and `git worktree remove` runs at cleanup; a dirty worktree is kept and reported.
- Atomic writes for every file aibus owns (temp, fsync, rename).
- No file outside `~/.config/aibus`, `~/.local/{share,state}/aibus`, `~/.cache/aibus`, `<project>/.aibus`, `<project>/thoughts` and the run's worktree is written by aibus itself. Tools write where the user allowed.

## 8. Network

Only provider crates open sockets, plus `web_fetch` when enabled and `http` hooks in M5, and one listener: `aibus mcp --http` (E11, off unless the flag is given), which binds a loopback address only, wants a bearer token on every request and checks `Origin` and `Host`; it lives in the `aibus` binary, not in `aibus-core` or `aibus-mcp`. TLS via `rustls` (no OpenSSL). Proxies from `HTTPS_PROXY` honoured. Every request carries `User-Agent: aibus/<version>` and a `RequestMetadata` with session and run ids for provider-side tracing, never usernames or paths. There is no telemetry; `aibus` does not phone home.

## 9. Supply chain

- `cargo-deny` in CI with `deny.toml`: licences allow-list (MIT, Apache-2.0, BSD-2/3, ISC, MPL-2.0, Unicode, Zlib; everything else is a review), `bans` on duplicate major versions of `tokio`, `reqwest`, `serde`, `ratatui`, `advisories` with `vulnerability = "deny"`, `unmaintained = "warn"`, `sources` restricted to crates.io plus the one Gitea remote for `causl-engine-core` under the `causl-rs` feature.
- `cargo-audit` nightly on the lockfile; a new advisory opens an issue.
- `Cargo.lock` committed; `--locked` in CI and release builds.
- Release binaries built in CI with `--locked`, signed (`cosign` keyless) and published with SHA-256 sums and an SBOM (`cargo-cyclonedx`).
- `#![forbid(unsafe_code)]` everywhere except the keychain FFI, which is `deny` with one documented `allow`.
- Dependencies that execute at build time (`build.rs`) are listed in `docs/BUILD.md` with the reason.
- The `fake` provider has no network code; it cannot be misused as a proxy.

## 10. Threat model summary

| Threat | Mitigation |
|---|---|
| Key leaks to disk | keychain-only storage; `Secret<T>`; redactor; no secret in graph |
| Model exfiltrates a key via a tool | keys are not in the environment or files the tools can read; `env` auth variables are scrubbed from tool env |
| Model runs a destructive command | allow-lists, deny patterns, permission prompts, worktrees; no OS sandbox in v1 (stated) |
| Worker output hijacks the lead | wrapped as data, markers escaped, capped |
| Malicious agent definition in a cloned repo | project definitions' hooks run only from `.aibus/` (trusted by choice); extra dirs stripped; `bypass` clamped |
| Malicious dependency | cargo-deny, cargo-audit, locked builds, SBOM |
| Two aibus processes corrupt shared files | SQLite locking, advisory file locks, atomic writes |
| Hook hangs | timeouts, fail closed where blocking |
| Runaway spend | `max_tokens_total`, `max_cost_usd`, `max_turns`, concurrency caps, per-account rate limits; exit 8 |
