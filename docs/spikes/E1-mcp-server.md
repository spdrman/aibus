# E1: rmcp 3.9 under MSRV 1.89, `cargo deny`, the lock, and where the MCP crate lives

This is the spike for #96 (epic #95). Everything below was measured in docker on MARS (`rust:1.89-bookworm`, `CARGO_TARGET_DIR=/tmp/target`), with a throwaway `crates/aibus-mcp` and a scratch `aibus mcp` subcommand that I did not keep. Only this doc and the decision in `DECISIONS.md` merge. I did not run the workspace test suites (they run once on the release PR), so the two workspace tests the issue asks about are described under "What #99 has to change", not run.

## Result in one line

`rmcp` 3.9.0 builds on 1.89 with no `--precise` pin, passes `cargo deny check` with the current `deny.toml` unchanged, adds no network or UI crate to the closure, and answers both a `2026-07-28` `server/discover` and a legacy `2025-11-25` `initialize` out of the box. Use it, in a new `aibus-mcp` crate.

## The scratch crate

```toml
rmcp = { version = "3.9", default-features = false, features = ["server", "macros", "transport-io"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["rt", "macros", "io-std"] }
aibus-core.workspace = true
```

One tool, served with `#[tool_router(server_handler)]` and `Aibus.serve(stdio()).await?.waiting().await?` on a current-thread runtime. `aibus mcp` was a new `Command::Mcp` arm calling it. `schemars` is not a separate dependency: `server` pulls it in and `rmcp::schemars` re-exports it.

Features to enable: `server`, `macros`, `transport-io`. Never enable `client`, `auth*`, `reqwest`, `transport-streamable-http-*` or `transport-child-process` (the first four bring `reqwest` 0.13 or an HTTP stack, and `reqwest` is `deny-multiple-versions`). `default-features = false` matters: the defaults add `base64`, which we do not need.

## MSRV

rmcp declares `rust-version = "1.88"`, edition 2024. rustc 1.89.0 compiles it, `rmcp-macros`, and the rest. The tightest crate in the new set is `uuid` 1.29.2 at exactly 1.89.0, so a newer uuid is the first thing that would break the pin. Cargo resolved every new crate to a version that fits, so there is no `--precise` pin to add today. `docs/BUILD.md` already says a bare `cargo update` can undo pins; if a later update pulls a uuid that wants 1.90, pin `uuid` with `--precise 1.29.2`.

## What the lock gained

Existing entries were untouched (the diff is additions only). 28 crates, licences straight from `cargo metadata`:

| Crate | Version | Licence | Needs |
|---|---|---|---|
| `rmcp`, `rmcp-macros` | 3.9.0 | Apache-2.0 | `rmcp` has a `build.rs`; `rmcp-macros` is a proc macro |
| `schemars`, `schemars_derive`, `serde_derive_internals` | 1.2.3, 1.2.3, 0.30.0 | MIT, MIT, MIT/Apache-2.0 | `schemars_derive` is a proc macro |
| `pastey` | 0.2.3 | MIT/Apache-2.0 | proc macro |
| `chrono`, `iana-time-zone`, `iana-time-zone-haiku`, `android_system_properties` | 0.4.45, 0.1.65, 0.1.2, 0.1.6 | MIT/Apache-2.0 | `iana-time-zone-haiku` has a `build.rs` (Haiku only) |
| `uuid` | 1.29.2 | Apache-2.0/MIT | rustc 1.89.0 |
| `sha2`, `digest`, `block-buffer`, `crypto-common`, `const-oid`, `hybrid-array` | 0.11.1, 0.11.3, 0.12.1, 0.2.2, 0.10.3, 0.4.15 | MIT/Apache-2.0 | edition 2024, rustc 1.85 to 1.88 |
| `ref-cast`, `ref-cast-impl` | 1.0.27 | MIT/Apache-2.0 | `ref-cast` has a `build.rs`; `ref-cast-impl` is a proc macro |
| `darling`, `darling_core`, `darling_macro` | 0.24.1 | MIT | `darling_macro` is a proc macro, rustc 1.88 |
| `dyn-clone` | 1.0.20 | MIT/Apache-2.0 | none |
| `windows-core`, `windows-implement`, `windows-interface`, `windows-result`, `windows-strings` | 0.62.2, 0.60.2, 0.59.3, 0.4.1, 0.5.1 | MIT/Apache-2.0 | Windows targets only, two proc macros |

No new licence, so the `[licenses]` allow-list is enough (Apache-2.0 was already on it).

## `cargo deny check` (0.20.2, current `deny.toml`)

`advisories ok, bans ok, licenses ok, sources ok`. Warnings only, and the only new ones are seven `multiple-versions` duplicates, all from rmcp's newer crypto and `darling` line:

| Duplicate | Why | To clear it |
|---|---|---|
| `sha2` (0.10.9 and 0.11.1), `digest`, `block-buffer`, `crypto-common` | rmcp wants the 0.11 RustCrypto line, `keyring` and the rest of the tree are on 0.10 | wait for `keyring`'s dependency to move, or leave it as a warning (it is `warn`, not `deny`) |
| `darling`, `darling_core`, `darling_macro` | rmcp-macros and an existing derive crate use different minors | same, nothing to do on our side |

Nothing is denied, and `tokio`, `reqwest`, `serde` and `ratatui` still have one version each. The two `license-not-encountered` warnings are allow-list entries nothing uses; every new crate is MIT or Apache-2.0 so they are not new from rmcp. `deny.toml` needs no change.

## Closure of `aibus-mcp`

`cargo tree -p aibus-mcp -e normal --target all` gives 173 crates (most of them `aibus-core`'s). `reqwest`, `hyper`, `h2`, `rustls`, `ratatui`, `crossterm` and all three `aibus-provider-*` crates are absent. `sha2`, `chrono`, `uuid` and `schemars` are present, as expected.

## Release-shaped numbers

`cargo build --release --locked -p aibus --target x86_64-unknown-linux-musl`, one run each on a cold container (registry already fetched), `rust:1.89-bookworm` plus `musl-tools`:

| | build time | binary | stripped |
|---|---|---|---|
| without `aibus-mcp` | 93 s | 25,215,112 B | 20,781,240 B |
| with `aibus-mcp` (one tool) | 99 s | 27,925,392 B | 22,854,840 B |
| delta | +6 s | +2.71 MB | +2.07 MB |

One run each, so the time delta is within noise; the size delta is stable. The growth is rmcp's model types, `schemars`, `chrono` and a second `sha2`, and it will grow with real tools. That is about 11 % of the unstripped binary, which I think is fine for a first-class feature, but it is the number to watch.

## Handshake and wire behaviour

I drove the scratch server over stdio with a script (not the red harness, which this PR does not include). With `RUST_LOG=trace` set, stdout carried only JSON-RPC lines and stderr was empty (the scratch subcommand did not install a subscriber, so #99 has to keep tracing on stderr).

* `2026-07-28`: `server/discover` answers `supportedVersions` (`2024-11-05` through `2026-07-28`), capabilities and `_meta.io.modelcontextprotocol/serverInfo`. Every request needs `_meta` with `io.modelcontextprotocol/protocolVersion` **and** `io.modelcontextprotocol/clientCapabilities`; without the second the server answers `-32602` ("request _meta is missing or has malformed required fields"). The harness client has to send both. Results carry `resultType`, `ttlMs` and `cacheScope`.
* `2025-11-25`: `initialize`, `notifications/initialized`, `tools/list`, `tools/call` all work.
* Both: `tools/list` returns `inputSchema` and an `outputSchema` tagged `https://json-schema.org/draft/2020-12/schema`, `tools/call` returns `content` plus `structuredContent`, and an unknown tool is `-32602` ("tool not found").
* `serverInfo.name` defaults to `rmcp` and the version to `3.9.0`. #99 must override `get_info` and `discover` to say `aibus` and our version.

## Recommendation: rmcp or hand-rolled

rmcp. The protocol surface that costs the most to get right (the two lifecycles, the `_meta` checks, paging, `outputSchema` generation, `-32602` mapping) works on the first try, and the build fits the MSRV, `deny.toml` and the no-network-crates rules without any change. The price is about 2 MB, 28 lock entries and one new Apache-2.0 dependency whose release cadence we do not control. The fallback is a hand-rolled stdio server in the style of `crates/aibus-core/src/mcp.rs` (newline JSON-RPC on plain threads, hand-written schemas). I did not build it: it would be a few hundred lines for `tools/list` and `tools/call` alone, and it would own both protocol versions forever. Switch to it only if rmcp stops fitting the MSRV or a duplicate becomes a `deny` hit, because the tool handlers in #99 can sit behind a plain function table either way.

## Recommendation: where the crate lives

A new `crates/aibus-mcp`, depending on `aibus-core` (and nothing above it), with `aibus` calling a single `aibus_mcp::serve` from a new `mcp` subcommand. Reasons: rmcp's tokio, schemars and chrono then stay out of `aibus-core` and `aibus-tui`, so their existing closure tests keep meaning something; the crate can carry its own `forbid(unsafe_code)` and a closure test that bans transports and UI; and the crate is easy to drop if the fallback is ever needed. Putting it inside `aibus-core` would put the dependencies in front of every crate. Putting it in `aibus` would make the handler untestable without the binary.

## What #99 has to change (not done here)

* `Cargo.toml`: add `aibus-mcp` to `[workspace.dependencies]`; `crates/aibus-mcp/Cargo.toml` as above; `aibus` depends on it. Commit the resulting `Cargo.lock` additions (28 entries, no pins).
* `crates/aibus-testkit/tests/workspace.rs`: `every_crate_forbids_unsafe_code` counts 10 root files and goes to 11, and the new `lib.rs` needs `#![forbid(unsafe_code)]`. Add `mcp_crate_never_sees_a_transport_or_ui_crate` banning `reqwest`, `hyper`, `h2`, `rustls`, `ratatui` and the `aibus-provider-*` crates (today's closure passes that).
* `docs/BUILD.md` rows for build-time code: `rmcp` (`build.rs`), `ref-cast` (`build.rs`), `iana-time-zone-haiku` (`build.rs`, Haiku only); proc macros `rmcp-macros`, `schemars_derive`, `pastey`, `ref-cast-impl`, `darling_macro`, `windows-implement`, `windows-interface`. I checked the targets in `cargo metadata`, not the unpacked sources of each `build.rs`, so read `rmcp`'s before writing its reason.
* `deny.toml`: no change. Expect the seven new duplicate warnings.
* Override `serverInfo`, keep tracing on stderr, and make the harness send `clientCapabilities` in `_meta`.

## Not covered

The red conformance harness from the issue body (client, transcripts, schema checker, `mcp_server.rs`) is not in this PR. The hand-rolled fallback was not built. Musl was measured for one target only; the Windows and macOS builds were not tried.
