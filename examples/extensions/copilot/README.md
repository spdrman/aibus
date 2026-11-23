# GitHub Copilot CLI extension

The official GitHub Copilot CLI CLI as an aibus extension (#291). Local, opt-in use of your own installed CLI
only: it signs in on its own, and aibus never reads or stores the token.

**Unverified.** The CLI was never installed or run where this was written, so no stream was recorded.
The fixtures are synthetic, built from the vendor's public documentation as I read it.
Not checked against any real version: the flags, reading the prompt from stdin, the event shapes,
and what the CLI does with a tool that needs approval when nothing can answer. The compat range is
a placeholder. Run it once on a machine that has the CLI, and replace the fixtures with a
recording, before relying on it. The dated status is in `docs/EXTENSION-SCORECARD.md`.

Declared unsupported: native child lineage, session resume, approvals, workspace control and hooks.
