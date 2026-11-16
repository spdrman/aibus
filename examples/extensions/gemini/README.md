# Gemini CLI extension

The official Google Gemini CLI as an aibus extension (#358). Local, opt-in use of your own
installed CLI only: it signs in on its own first run, and aibus never reads or stores the token.

**Unverified.** No Gemini CLI was installed where this was written, so the CLI was never run and
no stream was recorded. The fixtures are built from the event types in the CLI's own source
(`fixtures/gemini/types-2026-11-15.ts.txt` at the repo root) with synthetic values. Not checked
against any real version:

- the `--version` and `--help` text, and every field outside that source;
- reading the prompt from stdin without `-p`;
- what `--approval-mode default` does with a tool that needs approval in headless mode;
- whether tools that need none (web fetch, web search) run headless, which would be data leaving
  with no sandbox flag to stop it.

The compat range (`>=0.1.0, <1.0.0`) is a placeholder, because no version has been tried. Run it
once on a machine that has the CLI, and replace the fixtures with a recording, before relying on it.

Mapped: assistant messages, tool use and result (success or error), the result's token stats.
Everything else (init, user message echo, error events) arrives as a trace warning. Login status,
native child lineage, session resume, approvals, workspace control and hooks are declared
unsupported.
