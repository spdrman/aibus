# Codex CLI extension

The official OpenAI Codex CLI (`codex exec --json`) as an aibus extension (#357). Local, opt-in use
of your own installed CLI only: you sign in with `codex login`, and aibus never reads or stores the
token. The fixtures are streams recorded from codex-cli 0.167.0 (ids and token counts replaced).
`aibus extensions test . --fixtures` runs them offline.

Only what the recorded streams show is mapped: messages, shell commands (started, completed,
failed) and the turn's token usage. Other item types (file changes, MCP calls, web search) arrive as
trace warnings until a stream with them is recorded. Codex shows no native subagents in this
version, so child lineage, session resume and approvals through aibus are declared unsupported.
