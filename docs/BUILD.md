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
| `proc-macro2`, `quote` | `build.rs` for compiler feature detection, under every proc macro below |
| `serde_derive`, `thiserror-impl`, `async-trait`, `tokio-macros`, `tracing-attributes` | proc macros for the derives and attributes the provider trait and the tests use |

## YAML crate decision (Q15)

`serde_yaml` is unmaintained, so the plan is `serde_yml`, unless the smoke fuzz fails. The fuzz runs when agent definitions land (A2.1), and this is the default until then. If it fails, the fallback is `yaml-rust2`, and I record the swap here and in DECISIONS.md. No YAML crate is a dependency in the skeleton.

## CI

`.github/workflows/ci.yml` runs `cargo build --locked`, `clippy --all-targets -D warnings`, `cargo test --locked`, `cargo deny check` and `cargo audit` on push and PR. GitHub Actions are switched off for this repo, so I run the same commands locally before merging.
