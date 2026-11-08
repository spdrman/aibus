//! Build-time facts about the workspace: dependency direction and `forbid(unsafe_code)`.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Normal (non-dev, non-build) dependency closure of `name`, by package name.
fn closure(name: &str) -> BTreeSet<String> {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(root())
        .output()
        .expect("run cargo metadata");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let meta: Value = serde_json::from_slice(&out.stdout).expect("metadata json");

    let names: HashMap<&str, &str> = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    let edges: HashMap<&str, Vec<&str>> = meta["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            let deps = n["deps"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| {
                    d["dep_kinds"].as_array().unwrap().iter().any(|k| k["kind"].is_null())
                })
                .map(|d| d["pkg"].as_str().unwrap())
                .collect();
            (n["id"].as_str().unwrap(), deps)
        })
        .collect();

    let start = names.iter().find(|(_, n)| **n == name).map(|(id, _)| *id).expect("crate exists");
    let mut seen = BTreeSet::new();
    let mut todo = vec![start];
    while let Some(id) = todo.pop() {
        for dep in &edges[id] {
            if seen.insert(names[dep].to_string()) {
                todo.push(dep);
            }
        }
    }
    seen
}

#[test]
fn core_never_sees_ratatui_or_a_concrete_provider() {
    let deps = closure("aibus-core");
    for banned in ["ratatui", "crossterm", "aibus-tui", "aibus-provider-anthropic", "aibus-provider-openai", "aibus-provider-http", "reqwest"] {
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
    for banned in ["reqwest", "aibus-provider-anthropic", "aibus-provider-openai", "aibus-provider-http"] {
        assert!(!deps.contains(banned), "aibus-tui depends on {banned}");
    }
}

/// TLS is rustls with ring (docs/BUILD.md): no OpenSSL and no platform TLS stack. `schannel` and
/// `security-framework` stay allowed, because rustls-native-certs reads the system's root
/// certificates through them; they do no TLS.
#[test]
fn provider_crates_use_rustls_and_never_openssl() {
    for krate in ["aibus-provider-anthropic", "aibus-provider-openai", "aibus-provider-http"] {
        let deps = closure(krate);
        assert!(deps.contains("rustls") && deps.contains("reqwest"), "{krate} lost its TLS stack");
        for banned in ["openssl", "openssl-sys", "openssl-src", "native-tls", "hyper-tls"] {
            assert!(!deps.contains(banned), "{krate} depends on {banned}");
        }
    }
}

#[test]
fn only_the_provider_http_crates_open_sockets() {
    for krate in ["aibus-core", "aibus-graph", "aibus-tui", "aibus-provider"] {
        let deps = closure(krate);
        for banned in ["reqwest", "hyper", "h2", "rustls"] {
            assert!(!deps.contains(banned), "{krate} depends on {banned}");
        }
    }
}

#[test]
fn graph_has_no_workspace_deps() {
    let deps: Vec<_> = closure("aibus-graph").into_iter().filter(|d| d.starts_with("aibus")).collect();
    assert!(deps.is_empty(), "aibus-graph depends on {deps:?}");
}

#[test]
fn every_crate_forbids_unsafe_code() {
    let mut checked = 0;
    for entry in std::fs::read_dir(root().join("crates")).unwrap() {
        let dir = entry.unwrap().path();
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
    // Nine workspace members plus aibus-graph-causl, which sits outside the workspace.
    assert_eq!(checked, 10, "expected one root file per crate");
}

#[test]
fn msrv_is_pinned_to_1_85() {
    let toolchain = std::fs::read_to_string(root().join("rust-toolchain.toml")).unwrap();
    assert!(toolchain.contains("channel = \"1.85\""));
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).unwrap();
    assert!(manifest.contains("rust-version = \"1.85\""));
}
