# Changelog

All notable changes go here, newest first. The section for a version is what the release workflow
uses as the release notes, so every release needs its entry before it is merged into `release`.

## [0.2.0] - unreleased

### Models

* The built-in model tables moved from the provider crates to `catalog/<provider>.json`, and every priced row now writes all four buckets (`null` for none). Nothing about `models()` or `price()` changes.
* `catalog-sync` (in `aibus-testkit`) refreshes the catalog from a saved models.dev snapshot as a reviewable diff. It never fetches and never writes without `--write`.
* A model row can carry `price_from`, so an announced price sits beside the current one and starts on its date.
* For OpenRouter the response's `usage.cost` replaces the estimate, and `Usage.cost_source` says which one it was.

### Build

* The TUI is on ratatui 0.30 and crossterm 0.29, and `deny.toml` has no ignored advisories any more: `paste` (RUSTSEC-2024-0436) left the tree with ratatui 0.29. `crossterm` joins `ratatui` in the one-version-only ban list.
* `aibus-tui` has an optional `flow-diagram` feature (off by default, forwarded by the `aibus` binary) that draws the centre column through rataflow 0.1. It adds one crate to the lock and nothing to a default build. See `docs/spikes/D1.1-rataflow.md`.
* With `flow-diagram` on, the centre column reproduces the classic frames cell for cell: the same boxes, the same dotted connectors with their travelling dots and pulsing arrow head, the same card hit rects.
* The card row pans with the mouse: a drag from empty space in the centre column moves it a card at a time, Shift plus the wheel still does, and with more than three cards the row stops at the first and the last card. With `flow-diagram` on, the cards beside the window are on the canvas too and come in as it pans.

## [0.1.0] - 2026-11-09

First release. What is in it:

* The `aibus` binary: a ratatui dashboard (advisor rail, lead session, fork layer, worker cards,
  session log, status bar) and a headless `aibus run`.
* A lead agent that delegates to sub-agents defined as Markdown files with YAML frontmatter, an
  advisor that speaks at checkpoints, a router for small decisions, and ledgers, handoffs and
  memory that carry state from one session to the next.
* The whole application state as one Causl graph, with the commit log as the audit trail and
  snapshots as the session save.
* Anthropic and OpenAI-compatible (OpenAI, Kimi/Moonshot) providers, several accounts per
  provider, credentials in the OS keychain. The OAuth method and the Bedrock and Vertex
  transports are in the code but not usable yet (see Known issues).
* `aibus version` prints the release tag and commit, `aibus doctor` checks the setup.
* Release builds for aarch64 and x86_64 macOS, x86_64 and aarch64 Linux (glibc), x86_64 Linux
  (musl) and x86_64 Windows, with a CycloneDX SBOM, `SHA256SUMS` and keyless cosign signatures.

### Known issues

* `aibus account add --oauth` is refused until a provider offers an OAuth login; none does yet.
* The Bedrock and Vertex transports exist as constructors, but `auth = "sigv4"` and
  `auth = "gcloud"` cannot be selected from the config file yet.

Not proven yet: no real provider has been exercised against a live account by the release
pipeline, and the Windows build has not been run on a Windows machine. `docs/RELEASE.md` lists
the manual checks.

### Build

* MSRV is Rust 1.89 (`rust-toolchain.toml`), up from 1.85, because `rusqlite` 0.41 needs it. I moved to 0.41 for the bundled SQLite 3.54 (RUSTSEC-2026-0459).
