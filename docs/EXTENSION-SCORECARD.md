# Extension conformance scorecard

Status as of 2026-11-22. `unverified` means the CLI was never installed or run: the manifest and
the fixtures come from the vendor's public documentation as I read it, and the fixtures are
synthetic. The fixture checks run offline in `crates/aibus-extension/tests/extensions_a31.rs`.

| Extension id | CLI | Status | Fixture conformance | Tool ids | Crash and cancel | Login status | Usage report |
|---|---|---|---|---|---|---|---|
| `org.devshell.aibus.kimi-cli` | `kimi` | unverified | pass | pass | pass | unsupported | unsupported |
| `org.devshell.aibus.copilot-cli` | `copilot` | unverified | pass | pass | pass | unsupported | unsupported |
| `org.devshell.aibus.cursor-cli` | `cursor-agent` | unverified | pass | pass | pass | declared (`status`, unverified) | unsupported |
| `org.devshell.aibus.qwen-cli` | `qwen` | unverified | pass | pass | pass | unsupported | reported |
| `org.devshell.aibus.gemini-cli` | `gemini` | unverified | pass | pass | pass | unsupported | reported |

The xAI/Grok and DeepSeek rows are in `docs/PROVIDER-FEASIBILITY.md`, both `unavailable`.

Not covered yet, and why: the install, login-state and launch tests against a real CLI, the
negative-event tests against a recorded failure (the crash fixture only ends a stream early), and
the policy gate per platform. None of the four CLIs was available, so those wait for someone who
has them. See `docs/DEFERRED.md`.
