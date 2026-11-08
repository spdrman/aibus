# Building aibus

I started this file with the two things the spec says have to live here from day one: what runs at build time, and which YAML crate we use.

## Toolchain

`rust-toolchain.toml` pins 1.85, which is also the MSRV (`rust-version = "1.85"` in the workspace manifest). I only raise it with a changelog note. Everything builds with `--locked`, and `Cargo.lock` is committed.

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

## CI

`.github/workflows/ci.yml` runs `cargo build --locked`, `clippy --all-targets -D warnings`, `cargo test --locked`, `cargo deny check` and `cargo audit` on push and PR. GitHub Actions are switched off for this repo, so I run the same commands locally before merging.
