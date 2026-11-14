# Runs, invocations and fixtures

All runs happened on 2026-11-14 between 21:15 and 21:35 UTC on the owner's Mac (macOS, arm64), in a scratch directory under `/private/tmp`, never in a repo.

The fixtures are the CLI's output byte for byte, except for these rewrites. Nothing else was changed:

| Original | Rewritten to |
|---|---|
| The scratch root directory | `/spike` |
| The same root as Claude Code slugs it (`/` turned into `-`) | `-spike` |
| The Claude Code config directory (`CLAUDE_CONFIG_DIR`, both as an absolute path and as `$HOME/...` in `05`) | `<config-dir>` |
| The per-user temp root `/private/tmp/claude-<uid>/` | `/private/tmp/claude-UID/` |
| The home directory | `/Users/owner` |
| `email`, `orgId`, `orgName` in `05-auth-status.redacted.json` | `<redacted:...>` |

## The executable

`type -a claude` found three candidates:

| Candidate | What it is |
|---|---|
| shell alias -> `~/.claude/local/claude` | A bash wrapper that `exec`s `~/.claude/local/node_modules/.bin/claude` (an npm-local native build, 2.1.304, Developer ID signed). Aliases don't apply to a spawned process |
| `/opt/homebrew/bin/claude` | Symlink to `/opt/homebrew/Caskroom/claude-code/2.1.311/claude`, Mach-O arm64, `codesign --verify --strict` passes, authority `Developer ID Application: Anthropic PBC (Q6L2SF6YDW)`, sha256 `85d8c08707fcfd1e78ffa0204eefbc6ec9e4fe22f375fb2767ef85d957ed3161` |
| `~/.npm-global/bin/claude` | Old npm JS build, `cli.js`, 1.0.34 |

I used the Homebrew cask binary by its resolved absolute path, since it's the first `claude` on `PATH` for a spawned process and the newest. For #282 this means discovery should resolve every candidate, prefer a signed native binary, show the user which one it picked and why, and never pick the stale 1.0.34 by accident.

The owner's Claude Code config isn't in the default place; `CLAUDE_CONFIG_DIR` points at it. Every run passed that one variable through so the CLI used the owner's existing sign-in. I never looked inside that directory.

## Every CLI invocation (15 of the 25 allowed)

All of them ran with a cleared environment: `HOME`, `USER`, `LANG`, `PATH=/usr/bin:/bin` and `CLAUDE_CONFIG_DIR`, plus `LOGNAME`/`TMPDIR` for the harness runs and `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` where noted. 1 to 5 went through `env -i` from a shell; 6 to 15 went through the tokio harness below. Model: `haiku` (resolved to `claude-haiku-5-5`), the smallest alias the CLI offers. The raw log with exact argv quoting is `fixtures/claude-code/spike-281/inputs/invocations.log`.

| # | UTC | Args (prompts shortened) | Exit | Wall | Last `result.total_cost_usd` |
|---|---|---|---|---|---|
| 1 | 21:15:44 | `--version` | 0 | | |
| 2 | 21:15:44 | `--help` | 0 | | |
| 3 | 21:15:52 | `auth --help` | 0 | | |
| 4 | 21:15:56 | `auth status --help` | 0 | | |
| 5 | 21:16:01 | `auth status --json` (output redacted straight away, raw copy deleted) | 0 | | |
| 6 | 21:17:25 | `-p "Reply with exactly the word: ready" --output-format stream-json --verbose --include-partial-messages --model haiku --max-budget-usd 0.05 --no-session-persistence --setting-sources project --strict-mcp-config --tools ""` | 0 | 2.2 s | $0.00061 |
| 7 | 21:17:59 | `-p "<launch reader-a and reader-b in parallel ...>" --output-format stream-json --verbose --include-hook-events --forward-subagent-text --model haiku --max-budget-usd 0.30 --no-session-persistence --setting-sources project --strict-mcp-config --permission-prompts none --agents '<inputs/agents.json>' --tools Read,Agent --allowedTools Read,Agent` | 0 | 5.5 s | $0.00314 |
| 8 | 21:18:44 | as 7 without `--include-hook-events` and `--forward-subagent-text`, with `ps` sampling | 0 | 3.7 s | $0.00112 |
| 9 | 21:19:06 | `-p "<use Write once to create out.txt ...>" --output-format stream-json --verbose --include-hook-events --model haiku --max-budget-usd 0.10 --no-session-persistence --setting-sources project --strict-mcp-config --permission-prompts none --tools Read,Write --allowedTools Read` | 0 | 2.4 s | $0.00088 |
| 10 | 21:19:15 | as 7, harness `--timeout 3` (killed on purpose, but both children had already finished) | killed | 3.0 s | $0.00084 (last `result` before the kill) |
| 11 | 21:19:41 | as 7, prompt also asks for `run_in_background` false | 0 | 3.9 s | $0.00104 |
| 12 | 21:20:06 | env `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`, `-p "Reply with exactly the word: ok" --output-format stream-json --verbose --model haiku --max-budget-usd 0.05 --no-session-persistence --setting-sources project --strict-mcp-config --permission-prompts none` | 0 | 1.4 s | $0.00145 |
| 13 | 21:20:14 | as 12 without the env var | 0 | 1.4 s | $0.00145 |
| 14 | 21:21:24 | env as 12, `-p "<create an agent team with teammates alpha and beta ...>" --output-format stream-json --verbose --include-hook-events --forward-subagent-text --model haiku --max-turns 8 --max-budget-usd 0.20 --no-session-persistence --setting-sources project --strict-mcp-config --permission-prompts none --allowedTools Read,Agent` | 0 | 6.3 s | $0.00448 |
| 15 | 21:34:22 | as 11 with `--max-turns 8` and the slower `inputs/agents-slow.json` (three sequential reads per child), harness `--timeout-ms 2200`, `ps` series sampled alongside | killed | 2.2 s | none (no `result` before the kill) |

Summing the last `result.total_cost_usd` of each run gives $0.01502. Two caveats: runs 10 and 15 were killed, so whatever they spent after their last `result` (all of run 15) isn't counted, and these figures are the CLI's list-price estimate rather than a charge, because the account is a subscription. The seven-day `utilization` in `rate_limit_event` read 0.2 throughout. `--max-turns` is documented in the CLI reference but missing from `claude --help` on 2.1.311, so I only used it in runs 14 and 15 and capped the others by budget and by keeping prompts to one or two steps. No run used `--dangerously-skip-permissions`, `bypassPermissions` or any auto-accept mode. `--permission-prompts none` makes anything that would prompt get denied.

Side effects I know of and left alone: the CLI reported transcript and auto-memory paths under the owner's config dir (I didn't check whether it wrote them), and it wrote background-task output files under `/private/tmp/claude-UID/<cwd-slug>/<session>/tasks/`.

## The harness

A throwaway crate in the scratch directory (tokio 1 with `rt`, `macros`, `process`, `io-util`, `time`, `fs`), built with the local toolchain and kept out of the repo. It's here so #282 can start from something that ran. It takes an absolute executable path (no `PATH` lookup), canonicalises the cwd, clears the environment and passes through an allowlist, uses null stdin, and pipes stdout and stderr separately. It writes stdout byte for byte to `.stdout.jsonl`, keeps arrival times in a separate `.observed.tsv` (line, ms since spawn, bytes) so the JSONL stays exactly what the CLI printed, writes stderr to `.stderr.txt`, and records argv, env key names, exit and wall time in `.meta.txt`. On timeout it kills the process (no graceful stage, which #282 has to add). Runs 6 to 14 used it with whole-second `--timeout`; `--timeout-ms` was added for run 15. The listing below is the current source. Apart from `--timeout-ms`, the only change since run 6 is the wording of one comment.

```rust
//! Spike harness for #281: launch the official `claude` executable with tokio::process::Command,
//! an argv vector (no shell), a validated working directory and a bounded environment, and record
//! stdout (JSONL, byte for byte), stderr and exit status separately.
//!
//! Usage: harness --exe <abs path> --cwd <dir> --out <prefix> (--timeout <secs> | --timeout-ms <ms>) [--env K=V]... -- <claude args>
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Environment variables passed through from the parent when set. Nothing else is inherited:
/// in particular CLAUDECODE / CLAUDE_CODE_* inherited from a parent Claude Code process and any *_API_KEY never reach
/// the child unless named with --env.
const PASS_THROUGH: &[&str] = &["HOME", "USER", "LOGNAME", "LANG", "TMPDIR", "CLAUDE_CONFIG_DIR"];

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut exe, mut cwd, mut out, mut timeout_ms, mut extra_env) = (None, None, None, 120_000u64, vec![]);
    let mut claude_args = vec![];
    while let Some(a) = args.next() {
        match a.as_str() {
            "--exe" => exe = args.next().map(PathBuf::from),
            "--cwd" => cwd = args.next().map(PathBuf::from),
            "--out" => out = args.next(),
            "--timeout" => timeout_ms = 1000 * args.next().ok_or("--timeout needs a value")?.parse::<u64>()?,
            "--timeout-ms" => timeout_ms = args.next().ok_or("--timeout-ms needs a value")?.parse()?,
            "--env" => extra_env.push(args.next().ok_or("--env needs K=V")?),
            "--" => {
                claude_args.extend(args.by_ref());
            }
            other => return Err(format!("unknown harness arg {other}").into()),
        }
    }
    let exe = exe.ok_or("--exe required")?;
    if !exe.is_absolute() {
        return Err("--exe must be an absolute path (no PATH resolution)".into());
    }
    let exe = std::fs::canonicalize(&exe)?;
    let cwd = std::fs::canonicalize(cwd.ok_or("--cwd required")?)?;
    if !cwd.is_dir() {
        return Err("--cwd must be a directory".into());
    }
    let out = out.ok_or("--out required")?;

    let mut cmd = Command::new(&exe);
    cmd.args(&claude_args)
        .current_dir(&cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for k in PASS_THROUGH {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    for kv in &extra_env {
        let (k, v) = kv.split_once('=').ok_or("--env must be K=V")?;
        cmd.env(k, v);
    }

    let started_wall = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let t0 = Instant::now();
    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();

    let mut jsonl = tokio::fs::File::create(format!("{out}.stdout.jsonl")).await?;
    // Sidecar: line number, ms since spawn, byte length. Kept apart so the JSONL is exactly what
    // the CLI wrote.
    let mut timing = tokio::fs::File::create(format!("{out}.observed.tsv")).await?;
    timing.write_all(b"line\tms_since_spawn\tbytes\n").await?;

    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    });

    let mut lines = BufReader::new(stdout).lines();
    let mut n = 0u64;
    let read = async {
        while let Some(line) = lines.next_line().await? {
            n += 1;
            jsonl.write_all(line.as_bytes()).await?;
            jsonl.write_all(b"\n").await?;
            let ms = t0.elapsed().as_millis();
            timing.write_all(format!("{n}\t{ms}\t{}\n", line.len()).as_bytes()).await?;
        }
        Ok::<_, std::io::Error>(())
    };
    let timed_out = tokio::time::timeout(Duration::from_millis(timeout_ms), read).await.is_err();
    if timed_out {
        // Graceful first would be SIGTERM; tokio only offers kill here, which is fine for a spike.
        let _ = child.start_kill();
    }
    let status = child.wait().await?;
    let err = err_task.await?;
    tokio::fs::write(format!("{out}.stderr.txt"), &err).await?;
    let meta = format!(
        "exe={}\ncwd={}\nargv={:?}\nextra_env_keys={:?}\nstarted_unix_ms={started_wall}\nwall_ms={}\nstdout_lines={n}\ntimed_out={timed_out}\nexit={:?}\n",
        exe.display(),
        cwd.display(),
        claude_args,
        extra_env.iter().map(|kv| kv.split('=').next().unwrap_or("")).collect::<Vec<_>>(),
        t0.elapsed().as_millis(),
        status.code(),
    );
    tokio::fs::write(format!("{out}.meta.txt"), meta).await?;
    eprintln!("lines={n} exit={:?} timed_out={timed_out}", status.code());
    Ok(())
}
```

## Fixtures

Everything is in `fixtures/claude-code/spike-281/`. Per run there's `NN-name.stdout.jsonl` (the stream), `.stderr.txt` (empty in every run), `.hooks.jsonl` (hook payloads; a blank line follows each one), `.observed.tsv` (arrival timing) and `.meta.txt` (argv, exit, wall time). `inputs/` holds the agent definitions, the project hook settings, the two data files and the invocation log. `SHA256SUMS` covers every committed file.

| Fixture | What it shows |
|---|---|
| `01`..`04` | `--version`, `--help`, `auth --help`, `auth status --help` |
| `05-auth-status.redacted.json` | `auth status --json` with `email`, `orgId`, `orgName` replaced |
| `06-basic` | One prompt, partial messages on, no tools: `init`, `stream_event` deltas, `assistant`, `rate_limit_event`, `result` |
| `07-deleg` | **Background** delegation to two native subagents with hooks and forwarded text; three `result`s in one process |
| `08-control` | Same delegation without hook events or forwarded text in the stream (hooks still logged to file). `08-control.ps.txt` is the deduplicated set of `pid ppid command` lines seen while sampling `ps` during the run: a single `claude` process |
| `09-denied` | `Write` auto-denied: `permission_denied`, error `tool_result`, `permission_denials` |
| `10-killed` | Killed at 3 s, but only after both children had completed (`task_notification` at L53 and L58): the stream just stops, no `SessionEnd` |
| `11-foreground` | **The acceptance fixture**: one lead, two native children in the foreground, two `Read` calls, one final `result` |
| `12`, `13` | Agent-teams env var on and off, identical tool lists |
| `14-teams-attempt` | A "team" request under `-p` running as two ordinary subagents |
| `15-killed-midrun` | Killed at 2.2 s while both children were still working (reader-a had done one of its three reads, reader-b none); no `task_notification`, no `result`, no `SessionEnd`. `15-killed-midrun.ps.tsv` is the full `ps` series, one sample about every 300 ms from the spawn until 2 s after the kill |

Raw sha256 (before the rewrites) of the main captures, so the originals can be matched if they're ever needed:

| File | Raw sha256 |
|---|---|
| `06-basic.stdout.jsonl` | `9f05c81aba248bbe21da9208d91797b55e430e2b36ac4f1c8d6faf3ea76c6bde` |
| `07-deleg.hooks.jsonl` | `e81fa08f35406036e806294f02fed7cb6106ec5f2244afaf18809a9c841b5774` |
| `07-deleg.stdout.jsonl` | `61e8c00661ce1f33c389106533cbb292cabc7fc45eadfe5c7f0b5284e7c2738b` |
| `08-control.hooks.jsonl` | `905ae75afc12a5a575a5bb6a7eb0a46ff4fa298223ef3c5f348568ca2a90ea25` |
| `08-control.stdout.jsonl` | `0565fe894add0e2ba30298d4c065032e1a03baadc794d1d3a76f6a19a1192b38` |
| `09-denied.hooks.jsonl` | `5ca9f5c6e5af1c53af7e99cf9c6cd9df3a77f8c63959f8db6baba44c4e305a3b` |
| `09-denied.stdout.jsonl` | `303752b7471420440b9dd7c203dea3c997379974f26295c265642aac80183530` |
| `10-killed.hooks.jsonl` | `5e6049ec05de6dbee0893cdc877dd4956c30dedb9762590fdafc09796336369f` |
| `10-killed.stdout.jsonl` | `a4ebac677cc54d787f217365e82a7da4ca72b376d200a80267bebd081b207ba8` |
| `11-foreground.hooks.jsonl` | `dca0f9d68a054688b3654f016743d46dfc19d61914baf1c9baf5028c28cd13b0` |
| `11-foreground.stdout.jsonl` | `38b039983d1eb08705b981de955f97354304e9fc383e12ef0516acbe5796d28e` |
| `12-teams-probe.hooks.jsonl` | `7a8cf7a2a4437f03879abdf85f42b279246e0be642dc33566cd80f1c9bc38ad4` |
| `12-teams-probe.stdout.jsonl` | `3f16c31415660e248e19dc5176a9b8110268b16848e2907d3843a51ef6a4e8a5` |
| `13-teams-control.hooks.jsonl` | `4555f006c8b5e7d43d7ab8d1e2b0a6ef17731a3c14a0f3261d853932b88d0787` |
| `13-teams-control.stdout.jsonl` | `48e9b227c0b922040757da2eb6c2051ad8a6ceb1e89c32bc73ea450cfb65cc11` |
| `14-teams-attempt.hooks.jsonl` | `0d691453ae2eaaf5087777542019b91f2b4a8ebe2e1640c5681eb67f201eaaee` |
| `14-teams-attempt.stdout.jsonl` | `e9276d20b53a3fdfa0a3e78753c798669cd1d6134a81eab2d26935a11be66199` |
| `15-killed-midrun.hooks.jsonl` | `ecf66c54234f9f3d891f6ca1c4eda92cbea644c072c8a002f3f177811131668f` |
| `15-killed-midrun.stdout.jsonl` | `6b3b57b74f27443005d16148eed2fa099752e9111b57669322104a09c00c991a` |
