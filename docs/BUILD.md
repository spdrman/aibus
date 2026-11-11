# Building aibus

I started this file with the two things the spec says have to live here from day one: what runs at build time, and which YAML crate we use.

## Toolchain

`rust-toolchain.toml` pins 1.89, which is also the MSRV (`rust-version = "1.89"` in the workspace manifest). I only raise it with a changelog note. Everything builds with `--locked`, and `Cargo.lock` is committed.

## Dependencies that run code at build time

These are the crates with a `build.rs` (or a proc macro we rely on) in our tree, with the reason each is there. `aibus-provider` brought in the first batch (A1.4), and I add a line here in the same PR that brings in the next one.

| Crate | Reason |
|---|---|
| `serde`, `serde_core`, `serde_json`, `thiserror` | `build.rs` probes the compiler version to pick features |
| `libc`, `getrandom`, `zerocopy`, `typeid`, `zmij` | `build.rs` probes for platform or compiler features (transitive, via tokio, ulid, ron and serde_json) |
| `signal-hook` | `build.rs` probes the target for the signal numbers it exposes (the `aibus` binary uses it for SIGINT and SIGTERM; tokio's own `signal` feature is off because it enables `mio` workspace-wide) |
| `libdbus-sys` | the `vendored` feature compiles libdbus from source with `cc`, so the Linux keychain (Secret Service) needs no system package, only a C compiler (B1.4, via `keyring`) |
| `generic-array`, `rustix` | `build.rs` version and cfg probes: `generic-array` comes with the AES and SHA-2 that `keyring` uses to talk to Secret Service, `rustix` was already there through `crossterm` and `tempfile` and `aibus-core` now uses it directly for the owner check on key files (B1.4) |
| `ring` | compiles its C and assembler sources with the `cc` crate, so a C compiler is needed on Linux and macOS (B1.5, TLS for the provider crates, see "HTTP and TLS" below) |
| `rustls`, `httparse` | `build.rs` for compiler feature detection (`rustls` only sets a cfg on nightly; `httparse` reads `rustc --version` to switch on SIMD paths) |
| `icu_normalizer_data`, `icu_properties_data` | `build.rs` only looks at the `ICU4X_DATA_DIR` environment variable (transitive, via `reqwest` and `url`) |
| `proc-macro2`, `quote` | `build.rs` for compiler feature detection, under every proc macro below |
| `serde_derive`, `thiserror-impl`, `async-trait`, `tokio-macros`, `tracing-attributes`, `clap_derive` | proc macros for the derives and attributes the provider trait, the CLI and the tests use |

`aibus-graph` (A1.2) adds no row of its own: its `serde`, `serde_derive`, `proc-macro2` and `quote` are the ones above, and `smol_str`, `indexmap` and `base64` have no build script. Its dev-dependency `proptest` pulls in a few more (`getrandom`, `libc`, `zerocopy`, `num-traits`), which only run in tests.

The MSRV pins two versions in `Cargo.lock`: `smol_str` 0.3.2 (newer ones need rustc 1.89, older ones fail `cargo deny`) and `proptest` 1.11 (1.12 needs rustc 1.88). A bare `cargo update` undoes that, so use `--precise` until the MSRV moves.

## YAML crate decision (Q15)

`serde_yaml` is unmaintained, so the plan is `serde_yml`, unless the smoke fuzz fails. The fuzz runs when agent definitions land (A2.1), and this is the default until then. If it fails, the fallback is `yaml-rust2`, and I record the swap here and in DECISIONS.md. No YAML crate is a dependency in the skeleton.

## Config and CLI crates (A2.4)

`toml` 0.8 reads the config (with `serde_ignored` to find unknown keys, `regex` for the redactor patterns) and `clap` 4 with `derive` parses the command line. None of them has a `build.rs` (checked in the unpacked sources, as is `signal-hook-registry`); `clap_derive` is the one proc macro, listed above. I picked `toml` 0.8 as the conservative line; it has the `Deserializer` and error spans the line numbers need, and I did not try 1.x. `etcetera` is not used: the XDG layout is a few lines in `persist/paths.rs` and I did not want a dependency for it.

## HTTP and TLS (B1.5)

Only the `aibus-provider-*` crates open sockets. `aibus-provider-http` holds the one `reqwest` 0.12 client setup, and `aibus-provider-anthropic` and `aibus-provider-openai` use it, so `aibus-provider`, `aibus-core` and `aibus-tui` stay free of it (`crates/aibus-testkit/tests/workspace.rs` checks that, and that no OpenSSL or native TLS crate is in the provider crates' tree).

`reqwest` is built with `default-features = false` and `rustls-tls-native-roots`: rustls with the `ring` crypto provider, and the root certificates read from the system's store (so a company CA or a proxy's CA that is installed system-wide works). I picked `ring` over `aws-lc-rs` because `aws-lc-sys` can want cmake or a prebuilt binary and `ring` only wants a C compiler. I picked the native roots over `webpki-roots` because the latter's data is CDLA-Permissive-2.0, which is not in `deny.toml`'s allow-list, and I did not want to widen it. `HTTPS_PROXY`, `HTTP_PROXY` and `NO_PROXY` are honoured by `reqwest` itself (there is a test for `HTTP_PROXY`; `HTTPS_PROXY` goes through the same code but needs a CONNECT proxy, which I did not fake). I did not turn on `macos-system-configuration`, so the macOS system proxy settings are not read.

`cargo deny check` passes with the unchanged `deny.toml` (`ring` is `Apache-2.0 AND ISC`, which the allow-list covers). I did not build on macOS or Windows. The `ring` build script has a `nasm` path for Windows targets, so I expect a Windows build to need NASM or a prebuilt object; that is unchecked.

The MSRV pins `wiremock` (a dev-dependency of the provider crates and `aibus-testkit`) at 0.6.4 and its `deadpool` at 0.10.0 in `Cargo.lock`: wiremock 0.6.5 uses `let` chains, which need rustc 1.88. Use `cargo update -p wiremock --precise 0.6.4` if a refresh moves it.

## Credentials (B1.4)

`keyring` 3 is the keychain, and only the `aibus` binary links it (`crates/aibus/src/keychain.rs`), so `aibus-core` stays free of it and the workspace boundary tests keep passing. I turned on `apple-native`, `windows-native` and `sync-secret-service` with `crypto-rust` (no OpenSSL at run time; `Cargo.lock` still lists `openssl` and `openssl-src` because the optional feature graph does, but a Linux build never compiles them, and I did not check macOS or Windows). I did not use `linux-native` (kernel keyutils) because that one forgets keys on reboot. `rpassword` reads the key with the echo off and `toml_edit` rewrites the `[[account]]` tables without losing the user's comments. None of the three adds a `build.rs` of its own.

## Agent directory watcher (B1.1)

`notify` watches the agent directories for `agents.reload_on_change`. The only new crate with a `build.rs` on Linux is `inotify-sys`, whose script only decides whether to link `libinotify` on NetBSD and OpenBSD. I unpacked `fsevent-sys` (macOS) and `filetime` and neither has one; `kqueue` and `kqueue-sys` only build on the BSDs and I did not unpack them. The MSRV pins `notify` at 8.0.0 and `notify-types` at 2.0.0 in `Cargo.lock` (8.2 needs rustc 1.88); like the other pins, use `cargo update -p <crate> --precise` rather than a bare `cargo update` until the MSRV moves.

## Fuzzing (C2.1)

`fuzz/` is a workspace of its own (it is in the root manifest's `exclude`), with its own `Cargo.lock` started from the root one so the pins above hold. It needs a nightly toolchain and `cargo-fuzz`, and its one extra dependency is `libfuzzer-sys`, whose `build.rs` compiles libFuzzer's C++ sources with the `cc` crate (so a C++ compiler is needed, in the fuzz container only). Nothing in `cargo build --workspace`, `cargo deny` or the MSRV build sees it. `sh tools/fuzz.sh` runs the targets; see `docs/SECURITY.md`.

`aibus-testkit` now has `tempfile`, `zstd` and `base64` as normal dependencies (the `TempDir` that greps its tree for the fake key when a test ends); all three were already in the lock file through other crates, so no new build script comes with them.

## The causl-rs backend (C2.4)

`crates/aibus-graph-causl` is outside the workspace (see `exclude` in the root manifest) and has its own `Cargo.lock`, so `cargo build --workspace --locked`, `cargo deny check` and the root lock never see the private source. The default feature set does not compile the host layer; `--features causl-rs` does.

**Pin or vendor.** I pin: `causl-engine-core = { git = "https://git.opsite.ca/causl/causl-core-rs.git", rev = "a6247b36b38dda4d97017d0577210aa8c724e4f2" }` in that crate's `Cargo.toml`, with the same commit in its lock file. That is the commit the conformance suite, the differential and the numbers in `docs/spikes/A1.5-causl.md` ran against. Vendoring (a `vendor/` copy plus a `[patch]` entry) would take the token out of CI, but it needs the owner to confirm the crate's licence allows redistribution inside this repo, so I have not done it. Until one of the two happens, the crate can only be built with a token.

**Who fetches it.** Only the `causl-differential` job in `.github/workflows/causl-spike.yml` (manual dispatch, `contents: read`, SHA-pinned actions, `persist-credentials: false`). The secret `CAUSL_CI_BUILD_1` is on the single fetch step and every later step builds offline from the cargo cache. If the secret is empty or the remote refuses it, the job ends with the warning and summary line `skipped: causl-engine-core unavailable`, which is not a pass. No workflow runs on push or pull request.

**Supply chain.** `crates/aibus-graph-causl/deny.toml` sets `unknown-registry = "deny"` and `unknown-git = "deny"`, allows crates.io plus exactly `https://git.opsite.ca/causl/causl-core-rs.git`, and uses the same licence allow-list and bans as the root file. The root `deny.toml` keeps `allow-git = []`. Run it with `cargo deny --config crates/aibus-graph-causl/deny.toml --manifest-path crates/aibus-graph-causl/Cargo.toml check`; the job does that on every run, and `causl-engine-core` passes the licence allow-list there. I have not looked for a LICENSE file in the upstream repo, which matters for the vendoring question. `cargo audit` does not run on this crate.

**Running the differential locally.** You need the token, and it all happens in a container, never on a host. Give git the token as an extra header for the fetch only, then build offline:

```
cargo fetch --locked --manifest-path crates/aibus-graph-causl/Cargo.toml
cargo test --locked --offline --manifest-path crates/aibus-graph-causl/Cargo.toml --features causl-rs --test causl --test differential
cargo bench --locked --offline --manifest-path crates/aibus-graph-causl/Cargo.toml --features causl-rs --bench commit_stream
```

The differential replays every fixture in `aibus-testkit` through `MiniGraph` and `CauslGraph` and asserts equal snapshot bytes and equal commit intents. The bench prints both backends and a `side_by_side` line.

**If upstream lands `Persister`, `Reader` and `Observable`.** They are `unimplemented!()` in the pinned commit. `Reader` and `Observable` would change nothing for me (the public `State` getters do the job and I need no engine telemetry). `Persister` would let `snapshot` and `hydrate` use the engine's own state dump, but I would keep the typed-input snapshot unless the differential stayed green, since that is what keeps snapshots byte-identical with `mini`. The three engine gaps from the spike (derived retention seed and default, dynamic dependency ordering) are the ones that would let me delete host workarounds.

## The MCP server crate (E4)

`crates/aibus-mcp` depends on `aibus-core` and on `rmcp` 3.9 with exactly `server`, `macros` and `transport-io` (D-13); `crates/aibus` calls `aibus_mcp::serve_stdio` from the `mcp` subcommand and nothing else sees `rmcp`, `schemars` or `chrono`. The lock gained 27 entries (existing entries are untouched, apart from the version tag cargo now writes in a dependency list where a name became ambiguous): `rmcp`, `rmcp-macros`, `schemars`, `schemars_derive`, `serde_derive_internals`, `pastey`, `chrono`, `iana-time-zone`, `iana-time-zone-haiku`, `android_system_properties`, `sha2` 0.11 with `digest`, `block-buffer`, `crypto-common`, `const-oid` and `hybrid-array`, `ref-cast`, `ref-cast-impl`, `darling` 0.24 with `darling_core` and `darling_macro`, `dyn-clone`, and the Windows-only `windows-core`, `windows-implement`, `windows-interface`, `windows-result`, `windows-strings`. Cargo needed no `--precise` pin on 1.89. `cargo deny check` passes with `deny.toml` unchanged; the new warnings are the duplicates of `sha2`, `digest`, `block-buffer`, `crypto-common` and `darling*`, which `rmcp` brings on a newer line than the rest of the tree uses.

Crates in that set that run code at build time:

| Crate | Reason |
|---|---|
| `rmcp` | `build.rs` runs `git config core.hooksPath .githooks`, but only when two directories above the crate holds a `.githooks` directory and a `.git` (its own workspace), so as a registry dependency it does nothing; I read the file |
| `ref-cast` | `build.rs` probes the compiler for features (transitive, via `schemars`) |
| `iana-time-zone-haiku` | `build.rs` for Haiku only (transitive, via `chrono`) |
| `rmcp-macros`, `schemars_derive`, `pastey`, `ref-cast-impl`, `darling_macro` | proc macros: `rmcp`'s attribute macros (the `macros` feature is on as D-13 says; the handler writes its tool table by hand today), `schemars`' derive, identifier pasting, and the derives under them |
| `windows-implement`, `windows-interface` | proc macros, Windows targets only |

## CI

`.github/workflows/fuzz.yml` runs the fuzz targets nightly. `.github/workflows/ci.yml` runs `cargo build --locked`, `clippy --all-targets -D warnings`, `cargo test --locked`, `cargo deny check` and `cargo audit` on push and PR. GitHub Actions are switched off for this repo, so I run the same commands locally before merging.
