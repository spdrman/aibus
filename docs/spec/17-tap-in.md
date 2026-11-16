# 17. Tap in: watching Claude Code sessions that are already open (#348 phase 1, #355)

I wanted aibus to see the Claude Code sessions I started in other terminals. This is phase 1: Claude Code only and read-only. The decisions are D-355 and D-355b in `docs/DECISIONS.md` (D-355b is proposed, not ratified); the threats and their tests are in `docs/SECURITY.md`. Phase 2 (control through the documented inbox socket) and phase 3 (other clients) stay on #348.

## Commands

```text
aibus sessions [list]                     the live sessions: client, kind, name, state, started, what aibus can do (watch)
aibus sessions tap <session> [--follow]   header, sources, graph, timeline, intent, status line
aibus sessions hooks install --client claude [--config-dir DIR] [--yes]
aibus sessions hooks uninstall --client claude [--config-dir DIR] [--yes]
aibus sessions allow-transcripts claude [--revoke]
/sessions    /tap <session>               the same texts on the runs screen
```

`<session>` is a session id, a prefix of one, a background job id or a name, and it has to name exactly one. `--config-dir` otherwise comes from `CLAUDE_CONFIG_DIR`, then `~/.claude`. `--json` on `list` prints the rows.

## Where the data comes from

| Source | What it gives | Level and quality |
| --- | --- | --- |
| `claude agents --json` (documented) | the list: session id, job id, name, cwd, kind, start time, state | what the CLI says |
| `<config>/sessions/*.json` (local, not documented) | a pid (and a version, if a file has one) for a session the command listed | only as a fallback, never adds a session |
| hook events (command hooks that post to a loopback endpoint) | agents started and stopped, tool calls, delegations | `observed`, only from the moment the hooks were installed |
| `tasks/<session>/*.json` and `plans/*.md` | what the session wrote down | `intent`, not proof; a plan is matched by time and labelled `inferred` |
| the transcript tail (opt-in) | recent message text, tool call count | `unstable-format`, version gated, never stored |

The state column is the CLI's own word (`busy`, `idle`, `blocked`, `done`, ...). I did not map `blocked` to "waiting on a permission prompt" because I could not confirm that from the docs. A session that was tapped and is no longer listed shows as `gone`.

## The tap text

A header (client, version, state, kind, cwd), the sources and how good each is (a missing one says `unavailable` and how to turn it on), the status line (`control: watch only`), then exactly what `aibus runs inspect` prints for the graph and the timeline of the tap run, then the intent section and, with the opt-in, recent messages and a `trace_warning unknown_variant` count. `version` is `unavailable` unless a local session file carried one: the list does not.

## The hook receiver

`aibus sessions hooks install` writes `<state>/tap/aibus-tap-hook.sh` (0700) and, beside it, `hook.url` and `hook.header` (0600, the second holds the token), then adds one `command` hook to `<config>/settings.json` for each of `SessionStart`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `SubagentStart` and `SubagentStop`. It prints the exact diff and asks first. The token is not in the settings and not in the diff. The script posts the event to `http://127.0.0.1:<port>/aibus-tap/hook` with `curl -m 1`, throws the reply away, prints nothing and exits 0: a receiver that is not there, or never answers, costs a session one second at most, and nothing that listens on that port can answer a permission prompt or add text to a session. It needs `curl`; without it the script does nothing. `allowedHttpHookUrls` does not apply to command hooks. On a machine with other local users, whoever can bind the port while `--follow` is not running can still read the events, so this is meant for a single-user machine. `uninstall` takes out only the entries aibus wrote and rotates the token.

The settings edit keeps everything else as it was: other keys and events in their order, and your own empty lists. If the file is two-space JSON, install then uninstall gives back the same bytes; if it is written another way the diff shows it reformatted and it does not come back byte for byte. The old text is saved before every write as `settings.json.aibus-before` with the permissions of the settings file. A `hooks.<event>` that is not a list is refused, for install and uninstall.

The receiver is `aibus sessions tap <id> --follow`. It binds the install's port on `127.0.0.1`, serves 16 connections at most, cuts off a slow header or body after 2 s, requires the token, the `Host` and the path, caps the body at 1 MiB, records the tapped session's events and answers every valid post with an empty 200. It prints the text again after a change, at most twice a second. `/tap` in the TUI shows what the store holds and does not listen: run `--follow` in another terminal for the live feed.

## Limits

* Hooks show only what happens after they are installed; before that there is nothing unless transcripts are on.
* The version of a session is mostly `unavailable`, so no version counts as tested: until a real transcript has been parsed the screen says `format not verified against a real transcript`. `claude` on PATH on the machine I checked was 2.1.311 and the shell alias 2.1.304.
* `/sessions` and `/tap` run `claude agents --json` on a thread of their own (ten seconds at most) and the text arrives as a message, so the screen does not wait. Without `CLAUDE_CONFIG_DIR` or `HOME` they say so and read nothing.
* Same machine, same user, Claude Code only. Nothing is sent to a session.
