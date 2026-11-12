//! Build-time facts about the workspace: dependency direction and `forbid(unsafe_code)`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Normal (non-dev, non-build) dependency closure of `name`, by package name, for every target.
///
/// This is `cargo tree`, not `cargo metadata`: metadata's resolve lists a crate's optional
/// dependencies whether or not a feature turns them on (`ratatui` 0.30 shows `ratatui-termwiz`
/// and `ratatui-termina` that way, and neither is ever compiled), so a guard built on it blames
/// crates for code that is not in the build. `cargo tree` prints what the feature set links.
fn closure(name: &str) -> BTreeSet<String> {
    let out = Command::new(env!("CARGO"))
        .args(["tree", "--locked", "--target", "all", "-e", "normal", "--prefix", "none", "-f", "{p}", "-p", name])
        .current_dir(root())
        .output()
        .expect("run cargo tree");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("cargo tree is utf-8");
    // The first line is `name` itself; every other line is `name vX.Y.Z [(path)] [(*)]`.
    text.lines().skip(1).filter_map(|l| l.split_whitespace().next()).map(str::to_owned).collect()
}

#[test]
fn core_never_sees_ratatui_or_a_concrete_provider() {
    let deps = closure("aibus-core");
    for banned in ["ratatui", "crossterm", "aibus-tui", "aibus-provider-anthropic", "aibus-provider-openai", "aibus-provider-gemini", "aibus-provider-http", "reqwest"] {
        assert!(!deps.contains(banned), "aibus-core depends on {banned}");
    }
    let providers: Vec<_> = deps.iter().filter(|d| d.starts_with("aibus-provider-")).collect();
    assert!(providers.is_empty(), "aibus-core depends on provider crates {providers:?}");
}

#[test]
fn provider_crate_has_no_network_crates() {
    // Not socket2 or mio: tokio has its `net` feature on workspace-wide because reqwest needs it,
    // and cargo metadata shows that feature set to every crate that depends on tokio.
    let deps = closure("aibus-provider");
    for banned in ["reqwest", "hyper", "h2", "ureq", "isahc", "surf", "curl", "native-tls", "rustls", "openssl"] {
        assert!(!deps.contains(banned), "aibus-provider depends on {banned}");
    }
}

#[test]
fn tui_never_sees_reqwest_or_a_provider_crate() {
    let deps = closure("aibus-tui");
    for banned in ["reqwest", "aibus-provider-anthropic", "aibus-provider-openai", "aibus-provider-gemini", "aibus-provider-http"] {
        assert!(!deps.contains(banned), "aibus-tui depends on {banned}");
    }
}

/// TLS is rustls with ring (docs/BUILD.md): no OpenSSL and no platform TLS stack. `schannel` and
/// `security-framework` stay allowed, because rustls-native-certs reads the system's root
/// certificates through them; they do no TLS.
#[test]
fn provider_crates_use_rustls_and_never_openssl() {
    for krate in ["aibus-provider-anthropic", "aibus-provider-openai", "aibus-provider-gemini", "aibus-provider-http"] {
        let deps = closure(krate);
        assert!(deps.contains("rustls") && deps.contains("reqwest"), "{krate} lost its TLS stack");
        for banned in ["openssl", "openssl-sys", "openssl-src", "native-tls", "hyper-tls"] {
            assert!(!deps.contains(banned), "{krate} depends on {banned}");
        }
    }
}

#[test]
fn only_the_provider_http_crates_open_sockets() {
    for krate in ["aibus-core", "aibus-graph", "aibus-tui", "aibus-provider", "aibus-mcp"] {
        let deps = closure(krate);
        for banned in ["reqwest", "hyper", "h2", "rustls"] {
            assert!(!deps.contains(banned), "{krate} depends on {banned}");
        }
    }
}

/// D-13: rmcp comes in with `server`, `macros` and `transport-io` only. Anything that opens a
/// socket, draws a terminal or talks to a provider stays out of the crate that serves MCP, and
/// `aibus-mcp` sits on `aibus-core` and nothing above it.
#[test]
fn mcp_crate_never_sees_a_transport_or_ui_crate() {
    let deps = closure("aibus-mcp");
    assert!(deps.contains("rmcp") && deps.contains("aibus-core"), "aibus-mcp lost its parts");
    for banned in ["reqwest", "hyper", "h2", "rustls", "ratatui", "crossterm", "aibus-tui", "aibus"] {
        assert!(!deps.contains(banned), "aibus-mcp depends on {banned}");
    }
    let providers: Vec<_> = deps.iter().filter(|d| d.starts_with("aibus-provider-")).collect();
    assert!(providers.is_empty(), "aibus-mcp depends on provider crates {providers:?}");
}

#[test]
fn graph_has_no_workspace_deps() {
    let deps: Vec<_> = closure("aibus-graph").into_iter().filter(|d| d.starts_with("aibus")).collect();
    assert!(deps.is_empty(), "aibus-graph depends on {deps:?}");
}

#[test]
fn every_crate_forbids_unsafe_code() {
    let (mut checked, mut crates) = (0, 0);
    for entry in std::fs::read_dir(root().join("crates")).unwrap() {
        let dir = entry.unwrap().path();
        crates += 1;
        for file in ["src/lib.rs", "src/main.rs"] {
            let path = dir.join(file);
            if path.exists() {
                let src = std::fs::read_to_string(&path).unwrap();
                assert!(
                    src.lines().any(|l| l.trim() == "#![forbid(unsafe_code)]"),
                    "{} lacks #![forbid(unsafe_code)]",
                    path.display()
                );
                checked += 1;
            }
        }
    }
    // Every directory under crates/ is a crate, aibus-graph-causl (outside the workspace) included.
    assert_eq!(checked, crates, "expected one root file per crate");
}

#[test]
fn msrv_is_pinned_to_1_89() {
    let toolchain = std::fs::read_to_string(root().join("rust-toolchain.toml")).unwrap();
    assert!(toolchain.contains("channel = \"1.89\""));
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).unwrap();
    assert!(manifest.contains("rust-version = \"1.89\""));
}

#[test]
fn tui_is_on_ratatui_030_and_crossterm_029() {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(root())
        .output()
        .expect("run cargo metadata");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let meta: Value = serde_json::from_slice(&out.stdout).expect("metadata json");
    let versions = |name: &str| -> Vec<String> {
        meta["packages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["name"] == name)
            .map(|p| p["version"].as_str().unwrap().to_string())
            .collect()
    };
    for (name, line) in [("ratatui", "0.30."), ("crossterm", "0.29.")] {
        let got = versions(name);
        assert_eq!(got.len(), 1, "{name} must appear once in Cargo.lock, got {got:?}");
        assert!(got[0].starts_with(line), "{name} is {}, want {line}x", got[0]);
    }
}

#[test]
fn rataflow_is_in_the_default_build() {
    for krate in ["aibus", "aibus-tui"] {
        assert!(closure(krate).contains("rataflow"), "{krate} lacks rataflow");
    }
    for krate in ["aibus-core", "aibus-graph", "aibus-provider"] {
        assert!(!closure(krate).contains("rataflow"), "{krate} sees rataflow");
    }
}

#[test]
fn rataflow_is_a_plain_dependency_of_the_tui() {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--locked", "--no-deps"])
        .current_dir(root())
        .output()
        .expect("run cargo metadata");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let meta: Value = serde_json::from_slice(&out.stdout).expect("metadata json");
    let mut owners = Vec::new();
    for p in meta["packages"].as_array().unwrap() {
        for d in p["dependencies"].as_array().unwrap() {
            if d["name"] == "rataflow" {
                assert_eq!(d["optional"], false, "{} has an optional rataflow", p["name"]);
                owners.push(p["name"].as_str().unwrap().to_owned());
            }
        }
        assert!(p["features"].as_object().unwrap().keys().all(|k| k != "flow-diagram"), "{} still has the feature", p["name"]);
    }
    assert_eq!(owners, ["aibus-tui"], "only the TUI names rataflow");
}

#[test]
fn rataflow_brings_no_layout_or_network_crates() {
    let tui = closure("aibus-tui");
    let rataflow = closure("rataflow");
    assert!(!rataflow.is_empty(), "rataflow has dependencies");
    for banned in ["rust-sugiyama", "petgraph", "ratzilla", "termion", "termwiz", "serde_json"] {
        assert!(!rataflow.contains(banned), "rataflow pulls in {banned}");
    }
    assert!(tui.contains("rataflow"));
}

/// docs/TESTING.md promises one ci.yml job per layer row, so the capability matrix and every
/// provider crate's cassettes have to be named there, not only reached through the workspace job.
#[test]
fn ci_has_the_capability_matrix_job_and_every_provider_crate() {
    let ci = std::fs::read_to_string(root().join(".github/workflows/ci.yml")).expect("read ci.yml");
    let has = |cmd: &str| ci.lines().any(|l| l.trim_start().strip_prefix("- run: ").is_some_and(|c| c == cmd));
    assert!(has("cargo test --locked -p aibus-testkit --test capability_matrix"), "ci.yml has no capability-matrix command");
    let provider = ci
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("- run: cargo test --locked -p aibus-provider "))
        .next()
        .expect("provider-contract command");
    for krate in ["aibus-provider-http", "aibus-provider-anthropic", "aibus-provider-openai", "aibus-provider-gemini"] {
        assert!(provider.split_whitespace().any(|w| w == krate), "provider-contract leaves out {krate}");
    }
}
