# Changelog

All notable changes go here, newest first. The section for a version is what the release workflow
uses as the release notes, so every release needs its entry before it is merged into `release`.

## [0.1.0] - 2026-11-09

First release. What is in it:

* The `aibus` binary: a ratatui dashboard (advisor rail, lead session, fork layer, worker cards,
  session log, status bar) and a headless `aibus run`.
* A lead agent that delegates to sub-agents defined as Markdown files with YAML frontmatter, an
  advisor that speaks at checkpoints, a router for small decisions, and ledgers, handoffs and
  memory that carry state from one session to the next.
* The whole application state as one Causl graph, with the commit log as the audit trail and
  snapshots as the session save.
* Anthropic, OpenAI-compatible (OpenAI, Kimi/Moonshot) and Bedrock/Vertex providers, several
  accounts per provider, credentials in the OS keychain, OAuth sign-in.
* `aibus version` prints the release tag and commit, `aibus doctor` checks the setup.
* Release builds for aarch64 and x86_64 macOS, x86_64 and aarch64 Linux (glibc), x86_64 Linux
  (musl) and x86_64 Windows, with a CycloneDX SBOM, `SHA256SUMS` and keyless cosign signatures.

Not proven yet: no real provider has been exercised against a live account by the release
pipeline, and the Windows build has not been run on a Windows machine. `docs/RELEASE.md` lists
the manual checks.

### Build

* MSRV is Rust 1.85 (`rust-toolchain.toml`).
