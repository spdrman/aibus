# Compatibility matrix (A3.5, #295)

Date: 2026-11-25. This says what I tried and what I only read. A row marked `unverified` means no test or run I did exercised it, and a claim in an extension manifest does not move it out of that column. Nothing here is a support promise, and the aibus version is still 0.2.1: whether the gate means a 1.0 is the owner's call (docs/V1-GATE.md).

## Vendor CLI versions

"Declared" is the range the extension's manifest asks for (`[compat] cli_versions`), which the host checks against `--version` before it launches anything. "Exercised" is the version a recorded stream or a live run came from. "On this Mac" is what `--version` printed on the machine I wrote this on.

| CLI | Extension id | Declared | Exercised | On this Mac | Status |
|---|---|---|---|---|---|
| Claude Code (`claude`) | `org.devshell.aibus.reference-cli` | `>=2.1.0` | 2.1.311 only (A1.1 spike #281, reference pilot #285, fixtures `fixtures/claude-code/spike-281/`) | 2.1.325 | Verified on 2.1.311. 2.1.325 is inside the declared range and not the tested version, so the host would run it with a `trace_warning` (`cli::TESTED_VERSION`); I did not run it. Anything older than 2.1.0 is refused (the stale 1.0.34 npm build the spike found) |
| Codex CLI (`codex`) | `org.devshell.aibus.codex-cli` | `>=0.167.0, <0.168.0` | 0.167.0 (recorded stream and `codex exec --help`, #357) | 0.169.0 | Verified on 0.167.0. The Mac has 0.169.0, outside the declared range, so by the range check the host would report the extension incompatible there (`negotiate.rs::a_cli_outside_the_written_for_range_is_incompatible`); I did not launch it to see. Moving the range is one person reading 0.169's help and streams |
| Gemini CLI (`gemini`) | `org.devshell.aibus.gemini-cli` | `>=0.1.0, <1.0.0` (a placeholder) | none: not installed | not installed | Unverified. Rules come from the CLI's source and the fixtures are synthetic (D-358) |
| Kimi Code (`kimi`) | `org.devshell.aibus.kimi-cli` | `>=0.1.0, <1000.0.0` (a placeholder) | none | not installed | Unverified, fixtures synthetic (docs/EXTENSION-SCORECARD.md) |
| Copilot CLI (`copilot`) | `org.devshell.aibus.copilot-cli` | `>=0.1.0, <1000.0.0` (a placeholder) | none | not installed | Unverified, and refused with `NotOfficial` until the user adds it to `extra_official_clis` |
| Cursor Agent (`cursor-agent`) | `org.devshell.aibus.cursor-cli` | `>=0.1.0, <1000.0.0` (a placeholder) | none | not installed | Same as Copilot |
| Qwen Code (`qwen`) | `org.devshell.aibus.qwen-cli` | `>=0.1.0, <1000.0.0` (a placeholder) | none | not installed | Same as Copilot |
| Grok Build (`grok`), DeepSeek Harness (`dsh`) | none | n/a | none | not installed | `disabled`: a first-party CLI exists for each, terms and login unread (docs/PROVIDER-FEASIBILITY.md) |
| OpenRouter | no extension: a direct provider in aibus (`openrouter` preset, OAuth PKCE login) | n/a | cassettes and the login tests; a live call was never made for #359 | n/a | Verified offline only. The nightly live provider run is a manual workflow (Actions are off) and I did not run it for this gate |

The ranges for Kimi, Copilot, Cursor and Qwen are as wide as the schema allows on purpose, because no version was tried. They say nothing about what works.

What a vendor CLI version change does in aibus (tested, with fixtures and stand-in CLIs, not with real CLIs):

* A version outside the declared range, an OS the extension does not list or a protocol range that excludes the host is `incompatible` and nothing launches (`aibus-extension/tests/negotiate.rs`: `a_cli_outside_the_written_for_range_is_incompatible`, `an_os_the_extension_does_not_list_is_incompatible`, `a_protocol_range_that_excludes_the_host_is_incompatible`; `sandbox.rs::a_cli_that_prints_no_version_is_not_launched`).
* A capability that depends on a flag is decided from the CLI's own `--help`, never assumed (`a_probe_required_capability_follows_the_help_text`, `a_probe_with_a_minimum_version_needs_that_version`). A capability declared `unsupported` stays so.
* A package whose ranges do not match is quarantined as `incompatible` and nothing is activated (`store.rs::an_incompatible_package_is_quarantined_as_incompatible`). I did not find a test of an already installed extension that stops matching after a CLI upgrade, so what happens to it is by reading `resolve`, not by a test.
* An unknown or malformed line from a CLI is a gap or a `trace_warning`, never a dropped line and never a crash (`host.rs::invalid_utf8_and_broken_json_are_gaps_and_do_not_end_the_attempt`, `the_declarative_engine_labels_what_it_cannot_map_and_never_drops_it`, and the seeded 40,000-line fuzz run in `aibus-supervisor/tests/fuzz.rs`). That is the event parser drift test I can honestly name: it shows drift is visible. It does not show a new CLI release still parses; only a recording of that release does.
* The A1.1 spike warned that `--bare` may become the default for `claude -p`, which would silently break subscription users. The host probes every version and does not fall back (condition 4 in docs/spikes/A1.1-claude-code/README.md); I did not re-read Claude Code's changelog for this gate.

## Platforms

| Platform | What ran | Status |
|---|---|---|
| Linux x86_64 (Debian bookworm container, Rust 1.89), MARS | The gate: clippy, the crate tests and the security suites (docs/V1-GATE.md has the tip's log). This is the only place the whole test set runs | Verified |
| macOS arm64 (Darwin, the owner's Mac) | The spikes and the ignored live tests ran here, with the real Claude Code and Codex. The `sandbox-exec` shell sandbox profile and the keychain steps of docs/RELEASE.md were not run for this gate | Partly: live runs from earlier phases only |
| macOS x86_64 | Built by the release workflow, never smoke-run | Unverified |
| Linux aarch64 | Built by the release workflow, never smoke-run | Unverified |
| Windows x86_64 | Built by the release workflow, never smoke-run; no machine (docs/RELEASE.md, "Windows (unverified so far)"). The extension manifests list `macos` and `linux` only, so no extension claims Windows | Unverified |
| Linux, supervisor child-subreaper (#352) | Tested in the container (`subreaper_352.rs`, `subreaper_mode_352.rs`) | Verified on Linux; macOS and Windows have none |

## Behaviours the gate names, and where each is tested

The issue lists a set of failures to test. I did not write new tests for them in this PR. Each is covered by a test from an earlier phase, and the gap is said in the last column.

| Case | Test | Gap |
|---|---|---|
| Crashes | `aibus-supervisor/tests/host.rs`: `a_cli_killed_by_someone_else_is_interrupted_never_completed`, `a_nonzero_exit_is_failed`, `a_store_error_mid_run_still_ends_the_cli_and_its_children`; `aibus-core/tests/durable_289.rs::a_crash_after_an_irreversible_action_requires_reconciliation` | Stand-in CLIs, not a real vendor crash |
| Retries | `durable_289.rs::a_retry_is_a_new_attempt_and_the_old_trace_stays_readable`, `a_not_applied_answer_runs_the_step_again_only_after_its_approval` | A vendor step is never resumed, only started again after a yes |
| Approvals | `aibus-core/tests/sched_288.rs`, `handoff_360.rs`, `openrouter_359.rs` | No command takes the answer yet |
| Worktree cleanup | unit tests in `aibus-core/src/worktree.rs`: `a_clean_worktree_is_removed_and_a_dirty_one_kept`, `cleanup_keeps_uncommitted_work_even_when_a_tracked_file_is_edited`, `a_stale_base_ref_from_a_cut_off_attempt_is_replaced`, `reject_keeps_the_patch_and_leaves_the_project_alone` | Library only, no host calls it. Windows untested; case-insensitive volumes and a swapped `.git` file are open (docs/DEFERRED.md) |
| Clean uninstall and rollback | `aibus-extension/tests/store.rs`: `remove_deletes_the_record_and_every_installed_version_and_nothing_else`, `rollback_without_an_earlier_version_is_an_error`, `update_moves_to_the_newest_and_keeps_the_old_for_rollback`; `registry.rs` | Extensions only. aibus itself has an installer, tested in `tests/installers/`; I did not re-run those |
| Official runtime version changes | `negotiate.rs` (above) | No real second version of any CLI was run |
| OS portability | The platform table above | Windows, Linux aarch64 and macOS x86_64 never run |
| Event parser drift | `fuzz.rs`, `conformance.rs`, the declarative engine tests | Fixtures only |
