# aibus v1 specification

This is the index. The spec is split into sections under `docs/spec/`; read them in order the first time.

| # | Section | What it covers |
|---|---|---|
| 00 | [Overview](spec/00-overview.md) | What aibus is, the three sources it recreates, vocabulary, design stance |
| 01 | [Agents and continuity](spec/01-agents-and-continuity.md) | Agent definitions, scopes, roles, model selection, delegation, context isolation, results, resume, compaction, hooks, advisor checkpoints, the router, ledgers, handoffs, memory, workflows, tools |
| 02 | [Architecture](spec/02-architecture.md) | Workspace, runtime, the Elm loop with actors, module tree, core types with signatures, error model, startup |
| 03 | [State: the Causl graph](spec/03-state-causl.md) | The Causl contract, what I verified about the Rust engine, the trait and two backends, graph layout, effects boundary, streaming, bounds |
| 04 | [Providers and accounts](spec/04-providers-accounts.md) | `Provider` trait, provider crates and dialects, accounts, selection chain, credential storage, config, security sign-off |
| 05 | [UI](spec/05-ui.md) | The recorded screen panel by panel with frame references, extra screens, key map, permission modal, rendering rules, theme |
| 06 | [CLI](spec/06-cli.md) | Subcommands, flags, `run` NDJSON, exit codes, `--json` shapes, non-TTY behaviour, env vars |
| 07 | [Persistence and config](spec/07-persistence-config.md) | XDG layout, snapshot and commit log, transcripts, SQLite index, config file, retention, multi-process |
| 08 | [Performance](spec/08-performance.md) | Rendering budget, streaming pipeline, bounded channels, commit costs, memory caps, tracing |
| 09 | [Security](spec/09-security.md) | Credentials, boundary vs advisory, shell execution and confirmation, redaction, injection, supply chain, threat model |
| 10 | [Testing](spec/10-testing.md) | Layers, fake provider, integration set, replay, `TestBackend` snapshots, conformance, benchmark gates |
| 11 | [Delivery](spec/11-delivery.md) | Milestones M0 to M6 with exit criteria, out of scope for v1, dependency risks |
| 12 | [Open questions](spec/12-open-questions.md) | Each with the panel's recommended default |

Contested decisions and why they went the way they did: [DECISIONS.md](DECISIONS.md).

## The short version

aibus is a Rust ratatui terminal app that runs a lead agent, delegates to sub-agents defined as Markdown files with YAML frontmatter, keeps an advisor reading the whole session and speaking only at checkpoints, routes small decisions through a cheap model (the fork layer), and carries state across sessions with ledgers, handoffs and memory. The whole application state is one Causl graph: every event is one atomic commit, every widget is a pure function of a derived node, the commit log is the audit trail, and a snapshot is the session save. Providers are compile-time crates behind one trait; accounts are logins, several per provider; credentials live in the OS keychain and nowhere else.
