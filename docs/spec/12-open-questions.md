# 12. Open questions

Each with the panel's recommended default. The default is what gets built unless someone objects on the issue.

| # | Question | Recommended default |
|---|---|---|
| 1 | Should aibus read `.claude/agents/` automatically, so existing setups work with zero config? | No. `agents.extra_dirs` is opt-in and hooks from there are stripped. `aibus agent import` is the migration path. Convenience is not worth running hooks the user never looked at. |
| 2 | Frontmatter field names: snake_case canonical with camelCase accepted, or camelCase to match Claude Code exactly? | snake_case canonical, both parsed. `aibus agent show` prints snake_case. |
| 3 | Is the router worth its latency on every turn? | Keep it, default on, capped at 10 per turn, off with one config line. The recording makes it a headline feature; measure in M4 and revisit if median added latency per turn exceeds 300 ms. |
| 4 | Router confidence without logprobs (Anthropic does not expose them): self-reported `p` or a fixed 0.5? | Self-reported through a strict output schema, with the provider name shown next to `p` in the transcript so the user knows it is self-reported. Prefer a logprob-capable account for the router when one exists. |
| 5 | Advisor packet: whole session every call, or a delta since the last call? | Whole session, elided to `packet_max_tokens`, matching the recording's "reads the whole session". Delta mode is a config option in M4 if cost bites. |
| 6 | Default advisor model: a large model is expensive on every checkpoint. | Default `opus` alias, but the advisor is disabled when the alias resolves to no account, with a one-line note. |
| 7 | Where do results over the cap go: file only, or also a head in the parent context? | Head (32 KiB) in context plus the file path. |
| 8 | Should background be the default for `delegate`? | Yes (matches Claude Code's current default). `background: false` is the opt-out. |
| 9 | Compaction summary by the run's own model or by a cheaper one? | The run's own model; quality of the summary is what keeps the session coherent. |
| 10 | Handoff topic naming: newest handoff dir like Continuous-Claude, or the git branch name? | Newest dir, with `--topic`. Branch-name topics are a config option (`continuity.topic = "branch"`). |
| 11 | Learning extraction at `session_end` adds one model call and a few seconds to quitting. Keep it default on? | On, skipped when the session had no completed run, and `q!` skips it. |
| 12 | `mini` vs `causl-rs` as the default graph backend at v1.0. | `mini` until the M6 gate passes (crate obtainable under supply-chain rules, differential test green). The trait is the contract; the default is logistics. |
| 13 | Should the graph hold tool call inputs (which may contain file contents) or only handles? | Handles for anything over 4 KiB; small inputs inline so `explain` is useful. |
| 14 | Commit history cap 4096: enough for `[`/`]` time travel? | Yes for a dashboard; the full history is on disk and `aibus session replay --until <t>` covers the rest. |
| 15 | YAML crate: `serde_yaml` is archived. | `serde_yml` at M0 unless it fails the fuzz target, then `yaml-rust2` with a thin serde shim. |
| 16 | Keychain on headless Linux without Secret Service. | `env` or `file` auth, never a plaintext fallback. `doctor` says so. |
| 17 | Permission prompts from eight background runs at once can be noisy. | Queue with `1 of N`; `a` (always for pattern) is what makes it manageable; consider a per-session "trust this agent for bash(cargo *)" suggestion in M3 UX review. |
| 18 | Mouse support on by default? | On; `[ui] mouse = false` and `--no-mouse` exist. Terminal text selection needs shift-drag with mouse on, which is the usual trade. |
| 19 | Should `aibus` without arguments on a TTY start a session immediately or show a launcher (recent sessions, resume)? | Start immediately with an empty prompt; `aibus session list` and `:resume` cover the rest. A launcher is a later UX item. |
| 20 | Token counting heuristic for providers without an endpoint. | 4 chars per token with a 10 % margin, per family override table in `providers/<id>/models.json`. |
| 21 | Windows: is the TUI a v1 target? | Build and smoke-test it; it is not a release blocker until M6. |
| 22 | The fork layer's label: `ROUTER` or the recording's `JEV`? | `ROUTER`; `router.label` overrides. The recording's name is a product, not a concept. |
| 23 | Where does the project's `AIBUS.md` sit relative to `CLAUDE.md`? | Both are read (`AIBUS.md` first, then `CLAUDE.md`); `omit_project_rules` skips both. |
| 24 | Should worker runs see the ledger? | No. The lead does; workers get the task prompt. A worker that needs it can be told in the prompt. |
| 25 | Cost estimates with unknown prices. | Show tokens only and `cost: n/a`; never guess a price. |
