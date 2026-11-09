# Security

This is where the threat table of `docs/spec/09-security.md` section 10 meets the tests. Every row names the test (or the CI job) that exercises its mitigation, so a row with nothing under it is a bug in this file. I wrote the tests under `crates/aibus-core/tests/security/` for C2.1, and the older ones are listed by name so they are easy to find from here.

The short version of what aibus promises: the tool policy is a real boundary at the aibus process, and nothing more. A command that starts through `bash` has your rights unless you turn the optional sandbox on (below). `--permission-mode bypass` says so in its `--help`.

## Threat table

Paths are relative to `crates/`. `security/` is `aibus-core/tests/security/`.

| Threat | Mitigation | Test |
|---|---|---|
| Key leaks to disk | keychain-only storage, `Secret<T>`, the redactor, no secret in the graph | Every integration test builds its directories with `aibus_testkit::TempDir`, which greps the whole tree (zstd frames unpacked, base64 form too) for the fake key when the test ends, and holds a log capture that fails on the key. Named ones: `aibus/tests/session_cmd.rs::a_run_leaves_the_07_layout_and_no_credential`, `aibus/tests/run_cli.rs::the_log_file_is_private_and_holds_no_credential`, `aibus-core/tests/persist.rs::a_registered_secret_never_reaches_the_transcript_or_msgs`, `security/redaction.rs::a_registered_key_is_replaced_in_a_body_even_when_no_pattern_knows_its_shape`, and `aibus-testkit/tests/provider_logs.rs` for the real provider crates |
| Model exfiltrates a key via a tool | keys are not in the environment or in files the tools can read; secret-shaped variables are scrubbed from hook and tool environments, `[hooks] pass_env` is the one way back | `security/environment.rs::a_hook_and_a_bash_call_both_start_from_the_scrubbed_environment`, `security/environment.rs::without_pass_env_the_listed_name_is_scrubbed_too` |
| Model runs a destructive command | allow-lists, deny patterns, permission prompts, worktrees, the path deny-list, and the optional OS sandbox | `aibus-core/tests/bash.rs` (allow-list and splitter table), `aibus-core/tests/write_tools.rs::the_deny_list_holds_even_in_bypass`, `security/boundaries.rs::write_and_edit_cannot_reach_git_settings_or_the_user_config_in_any_mode`, `security/sandbox.rs::the_sandbox_hides_the_user_config_and_keeps_the_project_and_tmp`, `security/sandbox.rs::sandbox_network_false_leaves_only_the_loopback_interface`, `security/sandbox.rs::a_configured_sandbox_that_is_missing_refuses_the_call_and_never_runs_outside_it`; the deny-list parser is fuzzed (`tool_pattern`) |
| Worker output hijacks the lead | wrapped as data, markers escaped, capped; hook stdout labelled; web content wrapped | `security/boundaries.rs::a_worker_summary_is_wrapped_as_data_with_markers_escaped_and_capped`, `security/boundaries.rs::hook_stdout_reaches_the_model_labelled_with_the_hook_that_wrote_it`, `security/boundaries.rs::web_content_is_wrapped_as_data_and_escaped_in_a_tool_result` |
| Malicious agent definition in a cloned repo | hooks of `agents.extra_dirs` definitions are stripped unless the directory is trusted, and their `permission_mode` is reset to `default`; `agent import` disables hooks | `security/boundaries.rs::an_extra_dir_definition_loses_its_hooks_unless_trusted_and_never_keeps_bypass`, `aibus-core/tests/agent_import.rs`; the definition parser is fuzzed (`agent_def`) |
| Malicious dependency | `cargo deny`, `cargo audit`, locked builds | no test in the tree: the `supply-chain` job in `.github/workflows/ci.yml` and the gate list in the PR. `aibus-testkit/tests/workspace.rs::every_crate_forbids_unsafe_code` and `provider_crates_use_rustls_and_never_openssl` pin what a dependency may bring in |
| Two aibus processes corrupt shared files | advisory file locks, atomic writes | `aibus-core/src/persist/fs.rs` unit test `locked_writers_in_two_threads_never_interleave`, `aibus-core/tests/persist.rs::a_snapshot_survives_a_write_and_a_read` |
| Hook hangs | timeouts, fail closed where the event can block | `aibus-core/tests/hooks.rs::a_timeout_blocks_a_blocking_event_and_is_an_error_for_the_rest` |
| Runaway spend | `max_tokens_total`, `max_cost_usd`, `max_turns`, concurrency caps; exit 8 | `aibus-core/tests/run_loop.rs::max_tokens_total_ends_the_run_partial_before_the_next_tool_runs`, `aibus-core/tests/run_loop.rs::max_cost_usd_ends_the_run_with_a_budget_error`, `aibus-core/tests/delegation.rs::past_the_concurrent_limit_the_tool_result_is_the_error`, `aibus/tests/exit_codes.rs::eight_is_a_budget_and_ten_is_everything_else_that_ended_early` |

## The redactor

The pattern half is best effort, and I keep it honest with two corpora. `aibus-core/tests/redact.rs` has a positive and a negative line for every pattern. `security/redaction.rs` runs the redactor over the recorded provider cassettes of both provider crates (it must leave all of them as they are) and over `fixtures/redact/provider-errors.toml`, which holds error bodies in the shapes providers and gateways echo a credential back in.

That second file is what found three gaps, now closed: OpenAI project, service-account and admin keys (`sk-proj-...`, the hyphen stopped the old `openai-key` pattern), Google API keys in a URL (`AIza...`), and gateways that echo a header or query field (`x-api-key`, `api_key=`). When a fuzz run or a real incident finds another, the order is: add the body to `provider-errors.toml`, add the pattern to `patterns.toml`, add the positive and negative lines to `redact.rs`.

## Fuzzing

`fuzz/` is a `cargo-fuzz` workspace of its own (nightly, libFuzzer) with four targets: `agent_def` (`agent::def::parse`), `handoff` (handoff YAML), `redactor` (`Redactor`, including that no registered key or key-shaped token survives) and `tool_pattern` (the `ToolPattern` text form, the specifier glob, the bash splitter and the deny list). The bodies of the targets are in `fuzz/checks.rs`, and `security/fuzz.rs` runs the same functions over every file in `fuzz/corpus/<target>/` and `fixtures/fuzz/<target>/` on every push, so the corpus is also a regression suite.

A crash from a fuzz run goes into `fixtures/fuzz/<target>/` with a descriptive name, plus a plain unit test next to the code that was wrong. The first one was found while setting this up: `ShellDeny::hit` sliced one byte past a space, which panics on a two-byte space such as U+00A0 (`fixtures/fuzz/tool_pattern/deny-hit-slices-inside-a-multibyte-space`).

Run it in a container, never on a host. The script installs nightly and `cargo-fuzz` itself:

```
docker run --rm -v "$PWD":/work -w /work rust:1.85 sh tools/fuzz.sh 60
```

The nightly job is `.github/workflows/fuzz.yml`: 120 seconds per target, and the crash artifacts are uploaded.

## The optional shell sandbox

Off by default, and never the default in v1. Turn it on in `config.toml` or `.aibus/settings.toml`:

```toml
[tools]
sandbox = "bwrap"          # or "sandbox-exec" on macOS, or "none"
sandbox_network = false    # default true; false cuts the network inside the sandbox
```

`bash` then runs inside it with the filesystem read-only except the project (or the run's worktree) and `$TMPDIR`, and your home directory is not visible. Hooks and MCP servers are not sandboxed: they are commands you configured yourself. If the program for the configured kind is not on `PATH`, `bash` refuses the call, it does not run unsandboxed, and `aibus doctor` fails the `shell sandbox` check. `aibus doctor` prints `shell sandbox: none` or the active one (`bwrap`, `bwrap, network off`).

The `bwrap` profile is tested in a container (`security/sandbox.rs`, with `AIBUS_REQUIRE_SANDBOX=1` so the test fails instead of skipping). The `sandbox-exec` profile is written but I have not run it: nothing in this repository's gates runs on macOS, so the same test is compiled for macOS and has to be run there by hand.
