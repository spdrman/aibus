//! The manifest contract (#286): the template and the reference extension parse, the A1.5
//! amendments are enforced, and a hostile or sloppy manifest is refused with the field named.

mod common;

use aibus_extension::manifest::{Capability, Engine, Manifest, ManifestError, Support};
use common::{edit, reference_dir, template_manifest};

fn refused(text: &str) -> (String, String) {
    match Manifest::parse(text) {
        Err(ManifestError::Invalid { field, why }) => (field, why),
        other => panic!("expected an Invalid refusal, got {other:?}"),
    }
}

fn refused_field(from: &str, to: &str) -> String {
    refused(&edit(&template_manifest(), from, to)).0
}

#[test]
fn the_template_parses_and_declares_every_capability() {
    let m = Manifest::parse(&template_manifest()).unwrap();
    assert_eq!(m.id, "org.example.aibus.examplecli");
    assert_eq!(m.normalizer.engine, Engine::DeclarativeJsonl);
    for c in Capability::ALL {
        assert!(m.capabilities.contains_key(&c), "{c:?} is not declared");
    }
}

#[test]
fn the_reference_extension_parses_with_the_ratified_id_and_the_builtin_engine() {
    let text = std::fs::read_to_string(reference_dir().join("aibus-extension.toml")).unwrap();
    let m = Manifest::parse(&text).unwrap();
    assert_eq!(m.id, "org.devshell.aibus.reference-cli");
    // Amendment E1: the stateful Claude normaliser is the host's, not a declarative table.
    assert_eq!(m.normalizer.engine, Engine::ClaudeStreamJson);
    assert!(m.normalizer.rule.is_empty());
    // Amendment E5 and C1: no session resume for native children.
    assert_eq!(
        m.capabilities[&Capability::SessionResume],
        Support::Unsupported
    );
    assert_eq!(
        m.capabilities[&Capability::NativeChildLineage],
        Support::Supported
    );
}

#[test]
fn a_schema_version_other_than_1_is_refused() {
    assert_eq!(
        refused_field("schema_version = \"1\"", "schema_version = \"2\""),
        "schema_version"
    );
}

#[test]
fn an_unknown_key_is_a_syntax_refusal_not_ignored() {
    let m = format!("{}\nsurprise = true\n", template_manifest());
    assert!(matches!(Manifest::parse(&m), Err(ManifestError::Syntax(_))));
}

#[test]
fn a_manifest_over_the_size_cap_is_not_read() {
    let m = format!("{}\n# {}\n", template_manifest(), "x".repeat(70_000));
    assert!(matches!(
        Manifest::parse(&m),
        Err(ManifestError::TooLarge(_))
    ));
}

#[test]
fn the_id_has_to_be_reverse_dns_lower_case() {
    for bad in [
        "examplecli",
        "Org.Example.X",
        "org.example",
        "org..example.x",
        "org.example.x/../y",
        "",
    ] {
        let field = refused_field(
            "id = \"org.example.aibus.examplecli\"",
            &format!("id = \"{bad}\""),
        );
        assert_eq!(field, "id", "{bad:?}");
    }
}

#[test]
fn the_version_is_exactly_major_minor_patch() {
    for bad in ["1.0", "1.0.0-beta", "01.0.0", "1.0.0+x", "v1.0.0", ""] {
        assert_eq!(
            refused_field("version = \"0.1.0\"", &format!("version = \"{bad}\"")),
            "version",
            "{bad:?}"
        );
    }
}

/// Amendment E6: the id and the name must not have the vendor's product name in them.
#[test]
fn an_id_or_name_with_claude_code_in_it_is_refused() {
    for id in [
        "org.aibus.claude-code",
        "org.example.claudecode",
        "org.example.claude_code",
    ] {
        let field = refused_field(
            "id = \"org.example.aibus.examplecli\"",
            &format!("id = \"{id}\""),
        );
        assert_eq!(field, "id", "{id}");
    }
    assert_eq!(
        refused_field("name = \"Example CLI\"", "name = \"Claude Code for aibus\""),
        "name"
    );
}

#[test]
fn only_https_urls_without_credentials_are_accepted() {
    for bad in [
        "http://example.org",
        "ftp://example.org",
        "https://user:pw@example.org",
        "javascript:alert(1)",
        "https://exa mple.org",
    ] {
        assert_eq!(
            refused_field(
                "source_url = \"https://example.org/aibus-examplecli\"",
                &format!("source_url = \"{bad}\"")
            ),
            "source_url",
            "{bad}"
        );
    }
    assert_eq!(
        refused_field(
            "vendor_urls = [\"https://example.org/docs/cli\"]",
            "vendor_urls = [\"http://example.org\"]"
        ),
        "cli.vendor_urls"
    );
}

#[test]
fn compat_ranges_must_parse_and_an_os_must_be_known() {
    assert_eq!(
        refused_field("protocol = \">=1.0.0, <2.0.0\"", "protocol = \"^1\""),
        "compat.protocol"
    );
    assert_eq!(
        refused_field("cli_versions = \">=1.0.0\"", "cli_versions = \"latest\""),
        "compat.cli_versions"
    );
    assert_eq!(
        refused_field("os = [\"macos\", \"linux\"]", "os = [\"plan9\"]"),
        "compat.os"
    );
    assert_eq!(
        refused_field("os = [\"macos\", \"linux\"]", "os = []"),
        "compat.os"
    );
}

#[test]
fn an_executable_is_a_file_name_never_a_path() {
    for bad in [
        "/bin/sh",
        "../examplecli",
        "bin/examplecli",
        "~/examplecli",
        ".hidden",
        "ex ample",
        "",
    ] {
        assert_eq!(
            refused_field(
                "executable_candidates = [\"examplecli\"]",
                &format!("executable_candidates = [\"{bad}\"]")
            ),
            "cli.executable_candidates",
            "{bad:?}"
        );
    }
    assert_eq!(
        refused_field(
            "executable_candidates = [\"examplecli\"]",
            "executable_candidates = []"
        ),
        "cli.executable_candidates"
    );
}

/// Amendment E2: the prompt goes on stdin. A placeholder, a path, a shell fragment or a bypass flag
/// in argv is refused.
#[test]
fn argv_is_static_tokens_with_no_placeholder_path_or_bypass() {
    for bad in [
        "{prompt}",
        "$HOME",
        "`id`",
        "a b",
        "../x",
        "/etc/passwd",
        "~",
        "--dangerously-skip-permissions",
        "--yolo",
        "a\\nb",
    ] {
        let field = refused_field(
            "argv = [\"run\", \"--json\"]",
            &format!("argv = [\"run\", \"{bad}\"]"),
        );
        assert_eq!(field, "execution.argv", "{bad:?}");
    }
    assert_eq!(
        refused_field("prompt = \"stdin\"", "prompt = \"argv\""),
        "execution.prompt"
    );
    assert_eq!(
        refused_field("stdout = \"jsonl\"", "stdout = \"text\""),
        "execution.stdout"
    );
}

/// Amendment E3: the environment allowlist refuses credential-shaped, host-private and loader names.
#[test]
fn env_allow_refuses_credentials_aibus_names_loader_names_and_session_names() {
    for bad in [
        "ANTHROPIC_API_KEY",
        "OPENROUTER_TOKEN",
        "AWS_SECRET_ACCESS_KEY",
        "GH_PASSWORD",
        "AIBUS_MASTER",
        "LD_PRELOAD",
        "DYLD_INSERT_LIBRARIES",
        "NODE_OPTIONS",
        "PATH",
        "CLAUDECODE",
        "CLAUDE_CODE_OAUTH",
        "lower",
        "A B",
    ] {
        assert_eq!(
            refused_field("env_allow = []", &format!("env_allow = [\"{bad}\"]")),
            "execution.env_allow",
            "{bad}"
        );
    }
    let ok = edit(
        &template_manifest(),
        "env_allow = []",
        "env_allow = [\"EXAMPLE_CONFIG_DIR\"]",
    );
    assert!(Manifest::parse(&ok).is_ok());
}

#[test]
fn an_optional_flag_needs_a_probe_that_lists_it() {
    let m = edit(
        &template_manifest(),
        "flags = [\"--usage\"]",
        "flags = [\"--other\"]",
    );
    assert_eq!(refused(&m).0, "execution.optional");
    assert_eq!(
        refused_field("flag = \"--usage\"", "flag = \"usage\""),
        "execution.optional"
    );
    assert_eq!(
        refused_field(
            "flag = \"--usage\"",
            "flag = \"--dangerously-skip-permissions\""
        ),
        "execution.optional"
    );
}

#[test]
fn requested_limits_cannot_be_zero() {
    assert_eq!(
        refused_field("wall_time_secs = 600", "wall_time_secs = 0"),
        "limits.wall_time_secs"
    );
}

#[test]
fn a_declarative_engine_needs_rules_and_a_builtin_one_takes_none() {
    let none = template_manifest()
        .split("[[normalizer.rule]]")
        .next()
        .unwrap()
        .to_owned()
        + "[capabilities]\n"
        + template_manifest()
            .split("[capabilities]\n")
            .nth(1)
            .unwrap();
    assert_eq!(refused(&none).0, "normalizer.rule");
    let builtin = edit(
        &template_manifest(),
        "engine = \"declarative-jsonl\"",
        "engine = \"builtin:claude-stream-json\"",
    );
    assert_eq!(refused(&builtin).0, "normalizer.rule");
}

#[test]
fn a_rule_pointer_must_be_a_short_json_pointer_and_a_field_fits_its_kind() {
    assert_eq!(
        refused_field(
            "pointer = \"/type\"\nequals = \"text\"",
            "pointer = \"type\"\nequals = \"text\""
        ),
        "normalizer.rule"
    );
    assert_eq!(
        refused_field(
            "kind = \"message_chunk\"",
            "kind = \"message_chunk\"\nid_pointer = \"/id\""
        ),
        "normalizer.rule"
    );
    assert_eq!(
        refused_field(
            "kind = \"message_chunk\"",
            "kind = \"message_chunk\"\ntokens_in_pointer = \"/n\""
        ),
        "normalizer.rule"
    );
}

/// A WASI component is not hosted in this release; a manifest that names one is refused rather
/// than run as if the host could sandbox it.
#[test]
fn a_wasi_engine_is_not_hosted_and_is_refused() {
    let m = edit(
        &template_manifest(),
        "engine = \"declarative-jsonl\"",
        "engine = \"wasi-component\"",
    );
    assert!(matches!(Manifest::parse(&m), Err(ManifestError::Syntax(_))));
}

#[test]
fn every_capability_must_be_declared_and_probe_required_needs_a_probe() {
    assert_eq!(
        refused_field("hook_channel = \"unsupported\"\n", ""),
        "capabilities"
    );
    assert_eq!(
        refused_field(
            "usage_report = \"probe_required\"",
            "usage_report = \"supported\""
        ),
        "probe"
    );
    assert_eq!(
        refused_field(
            "session_resume = \"unsupported\"",
            "session_resume = \"probe_required\""
        ),
        "probe"
    );
}

/// Amendment E5 and C1: a CLI whose children live inside the lead process cannot resume them.
#[test]
fn native_child_lineage_and_session_resume_do_not_go_together() {
    let m = edit(
        &template_manifest(),
        "native_child_lineage = \"unsupported\"",
        "native_child_lineage = \"supported\"",
    );
    let m = edit(
        &m,
        "session_resume = \"unsupported\"",
        "session_resume = \"supported\"",
    );
    assert_eq!(refused(&m).0, "capabilities.session_resume");
}

#[test]
fn the_permission_declaration_has_one_acceptable_value_per_field() {
    for (from, to) in [
        (
            "spawn_only_declared_cli = true",
            "spawn_only_declared_cli = false",
        ),
        (
            "read_provider_credentials = false",
            "read_provider_credentials = true",
        ),
        (
            "external_network_from_parser = false",
            "external_network_from_parser = true",
        ),
    ] {
        assert!(
            refused_field(from, to).starts_with("permissions."),
            "{from}"
        );
    }
}

#[test]
fn auth_stays_with_the_vendor_cli() {
    assert_eq!(
        refused_field("owner = \"vendor_cli\"", "owner = \"aibus\""),
        "auth.owner"
    );
    assert_eq!(
        refused_field(
            "credential_access = \"forbidden\"",
            "credential_access = \"read\""
        ),
        "auth.credential_access"
    );
}

#[test]
fn fixtures_are_relative_jsonl_files_under_fixtures() {
    for bad in [
        "../x.jsonl",
        "/etc/x.jsonl",
        "fixtures/../../x.jsonl",
        "other/x.jsonl",
        "fixtures/x.txt",
    ] {
        assert_eq!(
            refused_field(
                "files = [\"fixtures/basic.jsonl\", \"fixtures/failed-tool.jsonl\"]",
                &format!("files = [\"{bad}\"]")
            ),
            "fixtures.files",
            "{bad}"
        );
    }
}

#[test]
fn the_policy_block_needs_an_owner_and_a_real_review_date() {
    assert_eq!(
        refused_field(
            "review_date = \"2026-11-15\"",
            "review_date = \"15/11/2026\""
        ),
        "policy.review_date"
    );
    assert_eq!(
        refused_field(
            "review_date = \"2026-11-15\"",
            "review_date = \"2026-13-45\""
        ),
        "policy.review_date"
    );
}

/// An update that asks for more than the installed version is an escalation, named item by item.
#[test]
fn escalations_name_everything_a_new_version_asks_for_that_the_old_did_not() {
    let old = Manifest::parse(&template_manifest()).unwrap();
    assert!(aibus_extension::manifest::escalations(&old, &old).is_empty());
    let more = edit(
        &template_manifest(),
        "executable_candidates = [\"examplecli\"]",
        "executable_candidates = [\"examplecli\", \"othercli\"]",
    );
    let more = edit(&more, "env_allow = []", "env_allow = [\"EXAMPLE_HOME\"]");
    let more = edit(&more, "wall_time_secs = 600", "wall_time_secs = 6000");
    let more = edit(
        &more,
        "argv = [\"run\", \"--json\"]",
        "argv = [\"run\", \"--json\", \"--extra\"]",
    );
    let more = edit(
        &more,
        "workspace_control = \"unsupported\"",
        "workspace_control = \"supported\"",
    );
    let new = Manifest::parse(&more).unwrap();
    let got = aibus_extension::manifest::escalations(&old, &new);
    for want in [
        "executable:othercli",
        "env:EXAMPLE_HOME",
        "limit:wall_time_secs",
        "argv:--extra",
        "capability:workspace_control",
    ] {
        assert!(got.iter().any(|g| g == want), "{want} missing from {got:?}");
    }
    // Asking for less is not an escalation.
    assert!(aibus_extension::manifest::escalations(&new, &old).is_empty());
}
