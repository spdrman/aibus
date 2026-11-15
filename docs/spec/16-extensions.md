# 16. Extensions: the SDK, the sandbox host and the registry (EPIC A, #286)

This is what #286 built for spec A2.1 (`epic_a_aibus_workflow_engineering_v3.md`, sections 3, 6.1 to 6.3 and 10.2) on top of the supervisor and the trace (`14-evidence-trace.md`). It covers what an extension is, the manifest and its schema, how the host negotiates with the real CLI, what the sandbox lets an extension do, the registry and the installed store, the `aibus extensions` commands and how to write one. The decisions behind it are D-286 in `docs/DECISIONS.md`; the threats and their tests are in `docs/SECURITY.md`. The Phase 1 gate it starts from is `docs/spikes/A1.5-reference-pilot/`.

## What an extension is

A directory. `aibus-extension.toml`, a licence, a readme, `fixtures/*.jsonl` with a `*.expected.json` beside each, and nothing that runs. aibus never loads code from a package: no native plugin (unstable Rust ABI), no script, no WASI component in this release. The host alone starts a process, and only a program on the host's own allowlist of official CLIs (see "Which programs run"), through the supervisor (`14-evidence-trace.md`). A manifest *names* a program; it never decides that the program counts as an official CLI.

That is enough for the two things an extension has to say. How to launch the CLI (a static argv, an environment allowlist, a limit request, optional flags the probe has to find) is data. How to read its events is an engine, and there are two:

| Engine | For | State | Who owns the code |
|---|---|---|---|
| `builtin:claude-stream-json` | The Claude Code stream | Stateful: tool use to agent maps, dedupe across the stream and hook channels, a `finish` step at process exit | The host (`aibus_supervisor::Normalizer`) |
| `declarative-jsonl` | A CLI whose events are one JSON line, one event | None beyond counters and the tool uses still open | The host (`aibus_extension::declarative`), driven by the manifest's rules |

Both are `aibus_supervisor::Engine`: `start`, `stream_line`, `hook_line`, `stderr_line`, `gap`, `finish`, per attempt. This is amendment E1 of the Phase 1 gate: a declarative-only manifest cannot express Claude's normaliser, so the normaliser is a per-attempt stateful object and `supervise` runs whichever one the manifest names. A CLI whose events need state across lines and is not Claude's needs a WASI component with the same shape. **This release does not host one** (see "Not here"), and a manifest that names `wasi-component` is refused like any other unknown value.

## The manifest (schema version 1)

Strict: an unknown key, a wrong type or a value outside these rules is a refusal that names the field (`aibus_extension::manifest::Manifest::parse`, at most 64 KiB). `docs/schemas/extension/manifest-v1.json` says the shape in JSON Schema; the rules a schema cannot say are marked below. `examples/extensions/template/aibus-extension.toml` is the annotated starting point.

| Block | What it says | Rules |
|---|---|---|
| `schema_version`, `id`, `version`, `name`, `kind`, `provider`, `license`, `source_url`, `[publisher]` | Identity | `schema_version = "1"`. `id` is lower-case reverse-DNS with three to eight labels. `version` is exactly `x.y.z`. `kind = "official_cli"`. URLs are `https`, with no credentials. **The id and the name never contain "Claude Code"** (E6) |
| `[compat]` | `protocol`, `os`, `cli_versions` | Ranges are comparators (`>=1.0.0, <2.0.0`). `os` is any of `macos`, `linux`, `windows` |
| `[cli]` | `executable_candidates`, `vendor_urls` | Executables are file names, never paths. Vendor URLs are the provenance |
| `[auth]` | `owner = "vendor_cli"`, `credential_access = "forbidden"`, `login_argv`, `status_argv` | The CLI owns its sign-in; aibus never reads, copies or stores the token |
| `[execution]` | `argv`, `prompt = "stdin"`, `stdout = "jsonl"`, `env_allow`, `[[execution.optional]]` | **The prompt goes on stdin, never in argv** (E2): no placeholder exists. An argv token is a plain word (no space, slash, quote, `$`, backtick, tilde or brace, and nothing that means "run without asking": `dangerously`, `skip-permissions`, `yolo`, `bypass`, `no-sandbox`, `auto-approve`, `allow-all`). `env_allow` names are upper case and never credential-shaped (key, token, secret, password, credential, auth, cookie, session id), never `AIBUS_*`, `LD_*`, `DYLD_*`, `PATH`, `NODE_OPTIONS`, `PYTHONPATH` and the like, never `CLAUDECODE` or `CLAUDE_CODE_*` (E3) |
| `[limits]` | What the extension asks for | Above zero. The host's ceiling wins |
| `[normalizer]` | `engine` and, for `declarative-jsonl`, one to 64 `[[normalizer.rule]]` | A built-in engine takes no rules. A rule is a JSON pointer, a string it equals and an event kind (`message_chunk`, `tool_started`, `tool_completed`, `tool_failed`, `usage_reported`); `id_pointer` only for the tool kinds, token pointers only for usage |
| `[capabilities]` | All thirteen declared | `supported`, `probe_required` or `unsupported`. Spec 3's twelve plus `hook_channel` (E5). **`session_resume` has to be `unsupported` when `native_child_lineage` is not**: native children die with the lead process (C1, E5) |
| `[[probe]]` | For each `probe_required` capability | The `--help` flags it needs and an optional minimum CLI version. A probe for any other capability is refused |
| `[hooks]` | `needs_delivery` | The extension's evidence is better with hook payloads (E4) |
| `[runtime]` | `usage_quality` | `reported`, `estimated` or `unknown` (E5) |
| `[permissions]` | One acceptable value each | `spawn_only_declared_cli = true`, `read_provider_credentials = false`, `external_network_from_parser = false`. Anything else is refused, so a reviewer reads one block |
| `[policy]`, `[fixtures]`, `[notes]` | Owner and review date, the recorded streams, upgrade and rollback notes | Spec 6.2 requires each of them. Fixtures are `fixtures/<name>.jsonl` and have to be in the package |

## The event contract

An engine hands the host events in the trace envelope (`aibus-trace`, schema 2; `docs/schemas/extension/event-v1.json`):

- Event ids are uuid v7 and ascend in the order the events were made (D-319); the order of a trace is the id.
- Every event has an evidence level in `visibility` (`observed`, `correlated`, `inferred`, `unavailable`). A derived event is never `observed`; an event from the CLI cites its raw line.
- Every number is a metric with a data quality (`reported`, `derived`, `estimated`, `unknown`). A missing number is `null` with quality `unknown`, never zero.
- A line the engine cannot map is a `trace_warning`, a line that is not an object is a `gap_detected`, and nothing is dropped. A tool use that never reported back ends the attempt with a `tool_outcome_unknown` gap.
- The attempt ends once, last, with `attempt_completed` or `attempt_failed`, when the process ends.

`aibus extensions test` checks each of these on every fixture and names the check (`basic: event ids ascend (uuid v7)`).

## Negotiation and probes

`negotiate::negotiate` takes the manifest and what `--version` and `--help` said. In order: the protocol range has to contain this host's (`1.0.0`), the operating system has to be listed, the CLI's version has to be in `cli_versions`; then each capability gets a verdict. `supported` and `unsupported` stay as declared (an extension cannot gain a capability the CLI happens to have), `probe_required` follows the probe flags and minimum version, and each unsupported verdict carries its reason. For `hook_channel`, when the manifest says `needs_delivery`, the verdict is `unsupported` with the reason that the host has no live way to receive hook payloads yet (E4; the inline `--settings` route is untried). The stream is then the only evidence, and a trace says so.

For the Claude engine the host also runs the supervisor's own probe (`cli::probe`): the required flags, the 2.1.0 floor and the refusal of a CLI whose `--help` says bare mode is the default for `-p`.

## Which programs run

The host keeps a short allowlist of official CLI program names: **claude, codex, gemini, kimi** (`sandbox::OFFICIAL_CLIS`). The user can add their own names in their policy file, `extra_official_clis = ["mycli"]` in `<config>/extension-policy.toml`; a manifest can never add one. This is the owner's default, to be confirmed: I put it in after the review of #332 and recorded it in D-286.

- `prepare` searches only for the manifest's names that are on the list, in the host's own search directories, and resolves the one it finds to an absolute path itself. A manifest whose names are all off the list gets `NotOfficial` and nothing is searched. `check_launch` asks the same of any launch, whoever built it.
- Refused outright, in a manifest and in `extra_official_clis`: shells, interpreters, package managers and fetchers (`sh`, `bash`, `zsh`, `python`, `perl`, `ruby`, `node`, `deno`, `bun`, `npx`, `npm`, `pnpm`, `yarn`, `pip`, `pipx`, `uv`, `gem`, `cargo`, `go`, `brew`, `curl`, `wget`, `env`, `sudo`, `doas`, `su`, `ssh`, `nc`, `socat`, `busybox`, `xargs`, `osascript`, `pwsh` and the rest of the list in `manifest::forbidden_program`, with a trailing version ignored). For a program that is not one of the host's four, argv with the shape of an install or an eval (`install`, `add`, `exec`, `eval`, `-c`, `-e`, `-m`) is refused too; an official CLI's own subcommands are its own (`codex exec --json` is fine). `@` is not a token character.
- **The resolved-name check is best effort.** After the search, if the file an allowlisted name resolves to is itself called a shell, interpreter or package manager (`claude` symlinked to `python3`), it is refused. That is all it does: a link to `nodejs`, `rbash`, `mksh`, `awk`, `bash-5.2`, `mise`, an npm `cli.js` or a wrapper script still launches, because the name says nothing about what the file is. The allowlist is only as trustworthy as the user's own search directories and policy file: a directory or a policy an attacker can write defeats it, and the host does not defend against that.
- A manifest has no setup step. There is no `prepare` table (an unknown key is a refusal), nothing a package says runs to get it ready, and if a later schema adds setup steps they will be declared, shown to the user and run only after a confirmation that names the exact command.
- `aibus extensions install` and `update` print each program, whether it is on the allowlist (an extension for an off-list program installs but will not run until the user adds it) and the whole argv.
- The "run without asking" list (`--full-auto`, `-y`, `--yes`, `--allowedTools=Bash`, `--permission-mode=acceptEdits` and the separate-value form, `--sandbox=danger-full-access`, `--ask-for-approval=never`, `--dangerously-*`, ...) is a denylist of spellings known today (the flag names, the mode flags with a skipping value in any of `--flag=value`, `--flag value` and `-fvalue`, `--config key=value` settings of an approval policy or a sandbox mode, and the flags that change what runs: `--settings`, `--mcp-config`, `--add-dir`, `--permission-prompt-tool`), applied to the argv, the auth argv and every optional flag. It stops what it knows; it is not a proof that an argv is safe, and I did not move to a per-CLI allowlist of flags, which would be safer but needs each CLI's flag set maintained. The allowlist of programs and the argv printed at install are the boundary.

## The sandbox host

`sandbox::prepare` is the only place a manifest becomes a launch, and `sandbox::check_launch` is the check every launch has to pass whoever built it.

- **Which program.** One of the manifest's file names, found in the host's own search directories by `cli::discover_from`: a relative directory is refused, so is a file others can write, one in a directory others can write, one owned by someone else, and one that does not print a version. The launch carries the declared name, so a swapped target is the supervisor's existing trust boundary, not a new one.
- **Which arguments.** The manifest's static tokens, then the optional flags the caller asked for and the probe found. A flag the caller needs that the CLI lacks is an error, never a silent drop.
- **Which environment.** `cli::bounded_env`: `PATH=/usr/bin:/bin`, the host's allowlist, then the manifest's `env_allow`. No credential-shaped, `AIBUS_*`, loader or session name ever reaches the CLI, whatever the manifest says, and the keychain is never opened here. Files under `HOME` are not protected by this (below).
- **Which directory.** The one the host granted, resolved, absolute and existing. A manifest cannot name a path.
- **Which limits.** The smaller of the request and the host's ceiling, field by field (wall time, stdout, line, stderr, lines a second). The supervisor enforces them: SIGTERM, then SIGKILL to the whole process group.
- **Which network.** None from aibus: no package code runs. The vendor CLI talks to its vendor, as it does outside aibus.

**What this is not.** It is not an operating system sandbox, and it gives the CLI no less than the user has. The host decides which program runs, with which argument, environment, directory and limits, and no package code runs in aibus. Once the CLI is running it is the user's process with the user's rights: it gets `HOME` (its own sign-in lives there, so it has to), so it can read other files under `HOME` and write outside its working directory, and nothing blocks its network. A descendant that leaves the process group (`setsid`, `setpgrp`) is not reached by the deadline's group kill and can outlive it, and a grandchild that keeps stdout open can make a clean exit read as a wall-time stop. The reviewer's stand-in CLI did all three. An OS sandbox around the CLI is not here (see "Not here"). The process-group escape is tracked in #338, and #288 (the scheduler that launches runs) is blocked on it.

## The registry and the installed store

A registry is a directory with `aibus-registry.json` (`registry-v1.json`): entries `{id, version, hash, path, signature?}` and a revocation list `{id, version?, hash?, reason}`. No network service, nothing fetched. The hash is `sha256-tree-v1:` over the sorted lines `<path> NUL <sha256 of the file> LF`, so one changed byte, one added file or one rename changes it. The index is at most 1 MiB and 4096 entries, with no duplicate (id, version) and no path that climbs.

Every fetch re-reads the package (`package::read`, walking by descriptor with `O_NOFOLLOW` at every step: no symbolic link and no hard link anywhere, empty directories counted, no special file, at most 256 files, 4 MiB a file, 16 MiB in all, six directories deep, plain ASCII names, no two names that differ only by case) and checks, in order: the manifest names the entry's id and version, the hash is the entry's, the registry's and the user's revocations, the signature, the publisher allowlist, and compatibility. A failure is filed as `revoked`, `provenance_invalid`, `incompatible` or `malformed`, and `install` records it as a quarantine and activates nothing.

The user's policy is `<config>/extension-policy.toml` (`require_signature`, `extra_official_clis`, `[trusted_keys]` as ed25519 public keys in hex, `allow_publishers`, `[[revoke]]`). With no file, nothing is required; a file another user can write, or that is owned by someone else, is refused. A signature covers `aibus-extension-v1`, the id, the version and the hash. Signing a package is the publisher's own step (`ring`, `ssh-keygen` or anything that makes ed25519); `aibus extensions publish` records a hash and leaves the signature field to the publisher.

The store is `<data>/extensions/` (`state.json` under a lock, and `packages/<id>/<version>/`, all `0700` and `0600`). It keeps every installed version, the active one, the previous one for `rollback`, a pin and a disabled flag per id. Resolving an extension for use re-hashes it every time, so a file changed after the install quarantines it (`tampered`) instead of running.

Revocations are checked at install, update, rollback, resolve and verify. The store keeps the revocations of the last registry any command read, so `resolve` and `rollback` need no registry; `rollback` refuses a revoked version and `resolve` does not serve one (it quarantines it); `verify` fails, and says why, when the registry cannot be read, instead of printing `ok`. A pin holds against `install` as much as `update`. A version number names one content: a registry that offers an installed (id, version) with other bytes is refused as `provenance_invalid`, and the installed copy stays. The store keeps the manifest of the last version the user accepted and compares with it, so an unreadable or quarantined active copy neither skips nor empties the escalation check (an unreadable active copy fails the install as `tampered`).

An update never goes down: a version below the active one is a downgrade, refused, and a registry that replays an older index changes nothing. An update that asks for more than the installed version (a new executable, environment name, optional flag, argv token, capability, engine, a larger limit, hook delivery) is a capability escalation: it is refused with each item named (`executable:othercli`, `env:EXAMPLE_HOME`, `capability:workspace_control`, ...), and nothing changes until the user repeats the command with `--accept <item>` for every one.

## Commands

```
aibus extensions [--registry DIR] search <query>
aibus extensions install <id>[@<version>] [--accept <item>]...
aibus extensions verify <id>
aibus extensions list
aibus extensions update <id> [--accept <item>]...
aibus extensions disable <id> | enable <id> | pin <id> | unpin <id> | rollback <id> | remove <id>
aibus extensions test <path-or-id> --fixtures
aibus extensions publish <dir>
```

The registry is `--registry`, else `$AIBUS_EXTENSION_REGISTRY`, else `<config>/extension-registry`. Spec 6.2 writes `<publisher/name>@<version>`; the id is a reverse-DNS name, so it is `<id>@<version>` here. Exit codes: 2 for a name that does not exist, a bad version or a registry that cannot be read; 6 for a refusal the user has to act on (a quarantine, an escalation, a pin, a downgrade, a disabled or quarantined extension); 1 when a conformance check fails. `--json` prints one object for `list` and `test`.

## Writing one

1. Copy `examples/extensions/template/`. Change the id (a domain you own), the executable name, the argv, the rules and the fixtures.
2. Record a real stream of your CLI into `fixtures/<name>.jsonl`, then write `fixtures/<name>.expected.json` (`fixture-expected-v1.json`): the ending, the attempt status and the event kinds in order. `AIBUS_BLESS=1 cargo test -p aibus-extension --test reference bless` writes them from what the engine makes now, for you to read before you keep them.
3. `aibus extensions test ./my-extension --fixtures` runs them offline.
4. `aibus extensions publish ./my-extension --registry ./my-registry`, share the directory, and anyone can `aibus extensions install <id> --registry ./my-registry`.

Nothing in the core source changes for any of it; `crates/aibus/tests/extensions_cli.rs::a_third_party_extension_round_trips_without_a_core_change` is that, end to end.

## The reference extension

`examples/extensions/reference-cli/` is the official Claude Code CLI as an extension, built on the supervisor: id `org.devshell.aibus.reference-cli` (the owner's reverse-DNS, replacing the spec's `org.aibus.claude-code`), engine `builtin:claude-stream-json`, argv equal to what `cli::argv` builds for a default request, and two recorded streams from the spike (`basic`, `native-subagent`). Local, opt-in use of the user's own installed CLI only: the user signs in with the CLI itself (an API key, a Console sign-in or, for local use, a subscription sign-in) and aibus never reads or stores the token. Bundled, preinstalled, hosted and commercial use stay blocked (#280, #281). The suite runs the real manifest through the sandbox and the supervisor against a stand-in `claude` that answers `--version` and `--help` with the real 2.1.311 text and replays a recorded delegation (`aibus-extension/tests/reference.rs`).

## Where this release differs from the issue text

The Phase 1 amendments win where they disagree with the issue, and each is applied:

| Issue text or spec | What it does here | Amendment |
|---|---|---|
| Declarative manifests by default; Claude Code "published" as one | The Claude parser is the host's engine, named by the manifest | E1 |
| `argv = ["-p", "{prompt}", ...]` | No placeholder; the prompt goes on stdin | E2 |
| `org.aibus.claude-code` | `org.devshell.aibus.reference-cli`; "Claude Code" is refused in an id and a name | E6 and the owner's decision |
| `RuntimeCapabilities` of spec 6.3 | Capabilities in the manifest and the negotiation, plus `hook_channel`, `usage_quality`, and `session_resume` unsupported for native children | E5 |
| Environment | Declared allowlist, credential-shaped names refused, `CLAUDECODE` and `CLAUDE_CODE_*` never passed | E3 |
| Hooks | A manifest can say it needs them; the host says they are not deliverable yet | E4 |
| "Constrained WASI parser" | Not hosted (below) | E1 (third-party stateful parsers) |
| The checkpoint amendments C1 to C5 | Not this issue: they bind #289 | |

## Not here

- **An operating system sandbox, and a way to keep files under `HOME` away from the CLI.** The CLI needs `HOME` for its own sign-in, so the answer is a sandbox profile per CLI (macOS `sandbox-exec`, Linux landlock or a container), and it is a design of its own.
- **A WASI host.** The scope says "only when needed". The declarative engine lets a third party write, test and install an extension for a one-line-one-event CLI with no core change, which is the acceptance criterion; a stateful parser for another CLI is the case that needs WASI, and which runtime to host (the issue leaves it open) is a decision with a security review of its own. Until then such a manifest is refused. Follow-up to open.
- **Optional flags with values** (`--model`, `--max-budget-usd`, `--agents`). Schema 1 has static tokens and value-less optional flags. The workflow compiler (#283) and the scheduler (#288) need them, and they need a typed, host-supplied value; the schema bumps to 2 for that.
- **A package archive.** A package is a directory. A single-file form can reuse the hardened tar reader of `.aibus` bundles (#271) later.
- **Signing from the command line, and a hosted catalog.** The signature is verified and enforced by policy; making one is the publisher's tool. Who signs a curated catalog and where it lives is still the open question of the issue.
- **Live hook delivery** (E4), **an operating system sandbox around the CLI**, and **a live run of the reference extension from the command line** (the scheduler, #288, launches runs).
