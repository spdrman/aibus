# Provider feasibility register

A dated yes or no on whether a first-party agent CLI exists that aibus may launch (an official,
unmodified executable, a documented automation surface, and a permitted way to use it). A "no"
means the provider stays `unavailable` and there is no fallback to an undocumented endpoint. A
"yes" is only a yes for that date, and it is not a build decision on its own...

I read the vendors' own repos and docs on 2026-11-25 (retrieval date for every URL below). Claims
I could not read at the source are listed as unverified, not guessed.

| Provider | First-party agent CLI with an automation surface | Checked | Evidence | Status |
|---|---|---|---|---|
| xAI/Grok | yes | 2026-11-25 | Grok Build (`grok`), published by xAI in `xai-org/grok-build`, Apache-2.0 (E1, E2). Headless: `grok -p`, `--output-format plain/json/streaming-json/streaming-messages-json`, `--max-turns`, `--allow/--deny` (E3). Login: `grok login` (OAuth or `--device-auth`), `grok logout`, credentials in `~/.grok/auth.json`, `XAI_API_KEY` as fallback (E4). Apache-2.0 section 6 grants no trademark rights (E5). | `disabled`, no build until the terms are read |
| DeepSeek | yes | 2026-11-25 | DeepSeek Harness (`dsh`), `deepseek-ai/deepseek-harness`, MIT, "developer preview" with a warning of compatibility-breaking changes (E6). Its CLI README documents `dsh --profile headless "job"` (one fresh persisted session, prints the final answer, exits; fatal configuration or boot failures exit nonzero), `--profile acp` (ACP over stdio) and `--profile sdk` (JSON-RPC over stdio) (E10). The Web UI launch (`npx @deepseek-ai/dsh web`, `--no-open`) is the other documented mode (E6). DeepSeek's docs also list Claude Code, Codex, OpenCode and others as third-party tools that use the DeepSeek API, and the integrations repo does not label any entry official (E8, E9). DeepSeek models stay reachable through separately billed OpenRouter. | `disabled`, no build until the terms and login are read |

## Evidence (all retrieved 2026-11-25)

- E1: https://github.com/xai-org/grok-build, README: "SpaceXAI's terminal-based AI coding agent", Rust source for the `grok` CLI, "headlessly for scripting/CI", installs from `https://x.ai/cli/install.sh`, license sidebar Apache-2.0.
- E2: https://github.com/xai-org/grok-build/blob/main/LICENSE, "Copyright 2023-2026 SpaceXAI", Apache License 2.0.
- E3: https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/14-headless-mode.md.
- E4: https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md.
- E5: Apache-2.0 section 6 "Trademarks", in E2.
- E6: https://github.com/deepseek-ai/deepseek-harness (README, MIT, "Developer preview", Run section) and https://deepseek-harness.github.io/deepseek-harness/en/guide/quickstart (API key entered in Settings, Models).
- E7: https://github.com/deepseek-ai/deepseek-harness/blob/master/BRAND_GUIDELINES.md: "DeepSeek Harness" is a registered trademark, so projects should not use the full name in their own names.
- E8: https://api-docs.deepseek.com/ ("Agent Integrations" section) and https://api-docs.deepseek.com/guides/anthropic_api.
- E9: https://github.com/deepseek-ai/awesome-deepseek-agent, README, no entry marked official.
- E10: https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/cli/README.md (the CLI README linked from the quickstart as "other CLI modes"), profile table: `headless` ("Run one fresh persisted session, print the final answer, and exit."), `acp` ("Serve automation clients over ACP stdio until disconnect."), `sdk` ("Serve SDK clients over JSON-RPC stdio until shutdown or disconnect."). Read with WebFetch on 2026-11-25.

## Unverified

- xAI terms of service and the consumer or enterprise usage terms for running `grok` from another program, and whether a SuperGrok subscription login may be driven that way: `https://x.ai/cli` and `https://x.ai/legal/terms-of-service` both returned HTTP 403 to my fetch, so I read neither. The yes above covers existence, provenance and surface only, not permission to embed. Per aibus invariant 5 the row stays `disabled` until someone reads those terms.
- Whether `grok login` has a status command aibus can call without touching `auth.json`: not found in E4.
- DeepSeek Harness: the CLI README has no license or terms text for running `dsh` from another program, and no login or API key instructions (the quickstart says the key goes in Settings, Models, E6), so I do not know how a headless run authenticates or whether embedding is allowed. The README does not describe the ACP or SDK message formats, the stdout format or exit codes of `acp` and `sdk`, or a stable-versus-preview split for the profiles, and the repo README (E6) still warns of breaking changes. The yes covers existence, provenance and surface only, so the row stays `disabled` (same as xAI, invariant 5). The trademark rule (E7) applies to any aibus naming.
- No CLI was installed or run, and no vendor was contacted.

Owner step: read xAI's terms and the DeepSeek Harness terms and login flow, then change the rows. No
unofficial client gets built in the meantime.
