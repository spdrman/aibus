# Attach a Claude Code session to aibus (spike, #322)

Start aibus with `aibus --control-attach --control-present`, and add `aibus mcp --allow attach --allow present` to
Claude Code's MCP servers. Tell the session "attach to aibus": it calls `aibus_attach` and shows a code. Type
`/attach accept <code>` in aibus. Queue a step with `/attach step <text>`; the session takes it with `next_step`
and answers with `step_result`.

To have a finished turn pull the next step, install `stop-hook.sh` as a `Stop` command hook in your own settings
and `touch ~/.local/state/aibus/attached` (remove the file to switch it off). aibus installs nothing.
