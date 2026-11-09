# aibus

A terminal app for running AI agents with sub-agents, an advisor and continuity across sessions. Rust, ratatui, crossterm.

**Status: specification stage.** There is no code yet. The spec is at [docs/SPEC.md](docs/SPEC.md) and the contested decisions are in [docs/DECISIONS.md](docs/DECISIONS.md).

What it will do, in one paragraph: a lead agent talks to you and delegates to sub-agents defined as Markdown files with YAML frontmatter (project or user scope, tool allow-lists, model per agent, background or foreground, resumable). An advisor reads the whole session and speaks only at checkpoints. A router answers small decisions with a cheap model and escalates the uncertain ones. Ledgers, handoffs and memory carry state from one session to the next... compound, don't compact. Several providers and several accounts per provider from day one, credentials in the OS keychain. The whole application state is one Causl graph, so every event is one atomic commit, the commit log is the audit trail, and a snapshot is the session save.

The dashboard is modelled on a screen recording: advisor rail on the left, lead session box, fork layer, swarm worker cards with progress, a session log and a status bar. Section 05 of the spec describes it frame by frame.

## Layout (planned)

```
crates/aibus                      the binary
crates/aibus-core                 engine: definitions, runs, tools, hooks, continuity
crates/aibus-graph                Causl contract, mini engine, causl-engine-core host
crates/aibus-provider             Provider trait, accounts, credentials, fake provider
crates/aibus-provider-anthropic
crates/aibus-provider-openai      OpenAI, Kimi/Moonshot and OpenAI-compatible dialects
crates/aibus-tui                  ratatui widgets and view model
crates/aibus-testkit
```

## Licence

MIT, see [LICENSE](LICENSE).
