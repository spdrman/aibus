# Deferred

Work that was in scope for an issue once and got cut to fit. Nothing here has an issue of its own yet.

## Attach phase 2 leftovers (#346)

I did `runtime = "attached"` in #353, and `step_ack`, tool-call correlation and a cooperative interrupt in #346. These three are left...

* **Channels as a push transport.** `--dangerously-load-development-channels` would let aibus push a step or an interrupt into a Claude Code session instead of waiting for it to pull. Not built, and not checked against a live client. Until it is, an interrupt reaches the agent only on its next `step_ack`, `step_tool_call` or `next_step`.
* **Codex push.** Whether a second client can attach to the thread the terminal shows, to start, steer and interrupt through the app-server, is unanswered. It needs a live Codex install and a session of the owner's, so it is an owner step.
* **A non-letter chord to accept an attach.** The typed `/attach accept <code>` stays. A chord only matters if the owner wants it instead of the code, and nobody asked for it yet.

Also unverified here: the live behaviour of `step_ack` and `step_tool_call` against Claude Code. The tests use fixtures; a PostToolUse hook that calls `step_tool_call` is not shipped.
