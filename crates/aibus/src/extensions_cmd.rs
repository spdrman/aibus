//! `aibus extensions` (#286, spec 6.2): search, install, verify, list, update, disable, remove and
//! test, plus enable, pin, unpin, rollback and publish. Everything reads and writes files: an
//! extension is data, no package code runs here, and nothing is fetched over a network (the
//! registry is a directory). Every string that comes from a package or a registry is cleaned for
//! the terminal before it is printed.

use std::io::Write;
use std::path::PathBuf;

use aibus_core::AibusError;
use aibus_extension::conformance;
use aibus_extension::package;
use aibus_extension::policy::Policy;
use aibus_extension::registry::Registry;
use aibus_extension::sandbox::OFFICIAL_CLIS;
use aibus_extension::store::{Store, StoreError};
use aibus_extension::version::exact;
use aibus_trace::view::clean;

use crate::cli::{ExtensionsArgs, ExtensionsCmd, Global};

fn usage(e: impl std::fmt::Display) -> AibusError {
    AibusError::Usage(clean(&e.to_string(), 400))
}

/// A quarantine, an escalation or a revocation is a refusal the user has to act on (exit 6); a
/// missing thing or a bad name is a usage error (exit 2).
fn store_error(e: StoreError) -> AibusError {
    let text = clean(&e.to_string(), 600);
    match e {
        StoreError::Quarantined { .. }
        | StoreError::Escalation(_)
        | StoreError::Pinned(_)
        | StoreError::Disabled(_)
        | StoreError::ActiveQuarantined(_)
        | StoreError::Downgrade { .. } => AibusError::Denied(text),
        StoreError::Io(e) => AibusError::Io(e),
        _ => AibusError::Usage(text),
    }
}

fn registry_dir(a: &ExtensionsArgs, g: &Global) -> Result<PathBuf, AibusError> {
    if let Some(r) = &a.registry {
        return Ok(r.clone());
    }
    if let Some(r) = std::env::var_os("AIBUS_EXTENSION_REGISTRY").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(r));
    }
    Ok(g.paths()?.config.join("extension-registry"))
}

fn open_registry(a: &ExtensionsArgs, g: &Global) -> Result<Registry, AibusError> {
    let dir = registry_dir(a, g)?;
    Registry::open(&dir).map_err(|e| {
        usage(format!(
            "registry {}: {e}",
            clean(&dir.display().to_string(), 200)
        ))
    })
}

fn policy(g: &Global) -> Result<Policy, AibusError> {
    Policy::load(&g.paths()?.config.join("extension-policy.toml"))
        .map_err(|e| AibusError::Usage(format!("policy: {}", clean(&e, 300))))
}

/// What an install or update lets the host run, so the user sees it: each program, whether the
/// host would run it, and the argv. Nothing here is run; a package has no setup step.
fn show_grant(
    out: &mut impl Write,
    store: &Store,
    id: &str,
    policy: &Policy,
) -> Result<(), AibusError> {
    let pkg = store.resolve(id, policy).map_err(store_error)?;
    for p in &pkg.manifest.cli.executable_candidates {
        let on = OFFICIAL_CLIS.contains(&p.as_str()) || policy.extra_official_clis.contains(p);
        let line = if on {
            "on the host's allowlist".to_owned()
        } else {
            "not on the host's allowlist, so it will not run (add it to extra_official_clis in extension-policy.toml if you trust it)".to_owned()
        };
        writeln!(out, "  program {}: {line}", clean(p, 64))?;
    }
    writeln!(
        out,
        "  argv: {}",
        clean(&pkg.manifest.execution.argv.join(" "), 300)
    )?;
    Ok(())
}

pub fn extensions(a: &ExtensionsArgs, g: &Global) -> Result<u8, AibusError> {
    let store = Store::open(&g.paths()?.data.join("extensions"));
    let mut out = std::io::stdout().lock();
    match &a.cmd {
        ExtensionsCmd::Search { query } => {
            let reg = open_registry(a, g)?;
            let hits = reg.search(query);
            if g.json {
                writeln!(
                    out,
                    "{}",
                    serde_json::to_string(&hits).map_err(std::io::Error::other)?
                )?;
            } else if hits.is_empty() {
                writeln!(out, "no extension matches {}", clean(query, 80))?;
            }
            for e in hits.iter().filter(|_| !g.json) {
                writeln!(out, "{}@{}  {}", e.id, e.version, e.hash)?;
            }
        }
        ExtensionsCmd::Install { spec, accept } => {
            let (id, version) = match spec.split_once('@') {
                Some((id, v)) => (
                    id,
                    Some(exact(v).ok_or_else(|| {
                        usage(format!("{} is not a version x.y.z", clean(v, 40)))
                    })?),
                ),
                None => (spec.as_str(), None),
            };
            let pol = policy(g)?;
            let rec = store
                .install(&open_registry(a, g)?, &pol, id, version.as_ref(), accept)
                .map_err(store_error)?;
            writeln!(out, "installed {id} {}", rec.active.unwrap_or_default())?;
            show_grant(&mut out, &store, id, &pol)?;
        }
        ExtensionsCmd::Update { id, accept } => {
            let pol = policy(g)?;
            let rec = store
                .update(&open_registry(a, g)?, &pol, id, accept)
                .map_err(store_error)?;
            writeln!(out, "{id} is now {}", rec.active.unwrap_or_default())?;
            show_grant(&mut out, &store, id, &pol)?;
        }
        ExtensionsCmd::Verify { id } => {
            // The registry's revocations are part of the check, so a registry that cannot be read is
            // a failure here, never a quiet "ok".
            let revoked = open_registry(a, g)
                .map_err(|e| {
                    usage(format!(
                        "{e}; verify could not check the registry's revocations"
                    ))
                })?
                .revoked;
            let pkg = store
                .verify(id, &revoked, &policy(g)?)
                .map_err(store_error)?;
            writeln!(out, "ok {id} {} {}", pkg.manifest.version, pkg.hash)?;
        }
        ExtensionsCmd::List { names } => {
            let st = store.state().map_err(store_error)?;
            if *names {
                for id in st.extensions.keys() {
                    writeln!(out, "{id}")?;
                }
            } else if g.json {
                writeln!(
                    out,
                    "{}",
                    serde_json::to_string(&st).map_err(std::io::Error::other)?
                )?;
            } else if st.extensions.is_empty() {
                writeln!(out, "no extensions installed")?;
            } else {
                for (id, rec) in &st.extensions {
                    let mut flags = Vec::new();
                    if rec.pinned {
                        flags.push("pinned".to_owned());
                    }
                    if rec.disabled {
                        flags.push("disabled".to_owned());
                    }
                    if !rec.quarantined.is_empty() {
                        flags.push(format!("quarantined:{}", rec.quarantined.len()));
                    }
                    let active = rec.active.as_deref().unwrap_or("none");
                    let line = format!(
                        "{id}  active {active}  installed {}  {}",
                        rec.versions.len(),
                        flags.join(",")
                    );
                    writeln!(out, "{}", line.trim_end())?;
                }
            }
        }
        ExtensionsCmd::Disable { id } => {
            store.set_disabled(id, true).map_err(store_error)?;
            writeln!(out, "disabled {id}")?;
        }
        ExtensionsCmd::Enable { id } => {
            store.set_disabled(id, false).map_err(store_error)?;
            writeln!(out, "enabled {id}")?;
        }
        ExtensionsCmd::Pin { id } => {
            store.set_pinned(id, true).map_err(store_error)?;
            writeln!(out, "pinned {id}")?;
        }
        ExtensionsCmd::Unpin { id } => {
            store.set_pinned(id, false).map_err(store_error)?;
            writeln!(out, "unpinned {id}")?;
        }
        ExtensionsCmd::Rollback { id } => {
            let rec = store.rollback(id, &policy(g)?).map_err(store_error)?;
            writeln!(out, "{id} is back to {}", rec.active.unwrap_or_default())?;
        }
        ExtensionsCmd::Remove { id } => {
            store.remove(id).map_err(store_error)?;
            writeln!(out, "removed {id}")?;
        }
        ExtensionsCmd::Publish { dir } => {
            let e = Registry::publish(&registry_dir(a, g)?, dir).map_err(usage)?;
            writeln!(out, "published {}@{}  {}", e.id, e.version, e.hash)?;
        }
        ExtensionsCmd::Test {
            target,
            fixtures: _,
        } => {
            let path = std::path::Path::new(target);
            let pkg = if path.exists() {
                package::read(path).map_err(usage)?
            } else if target.contains('/') {
                return Err(usage(format!("{} is not a directory", clean(target, 120))));
            } else {
                store.resolve(target, &policy(g)?).map_err(store_error)?
            };
            let report = conformance::run(&pkg);
            if g.json {
                let rows: Vec<_> = report
                    .checks
                    .iter()
                    .map(|c| serde_json::json!({ "name": c.name, "passed": c.passed, "detail": c.detail }))
                    .collect();
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({ "id": pkg.manifest.id, "hash": pkg.hash, "passed": report.passed(), "checks": rows })
                )?;
            } else {
                writeln!(
                    out,
                    "{} {} {}",
                    pkg.manifest.id, pkg.manifest.version, pkg.hash
                )?;
                for c in &report.checks {
                    writeln!(
                        out,
                        "{}  {}: {}",
                        if c.passed { "pass" } else { "FAIL" },
                        clean(&c.name, 120),
                        clean(&c.detail, 300)
                    )?;
                }
            }
            return Ok(u8::from(!report.passed()));
        }
    }
    Ok(0)
}
