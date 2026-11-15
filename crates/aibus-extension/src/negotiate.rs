//! Compatibility negotiation and capability probes. The manifest declares; the real CLI decides:
//! a capability marked `probe_required` is supported only when the probe flags are in `--help`
//! and the version is high enough, and anything the host cannot deliver says so with a reason.

use std::collections::BTreeMap;

use aibus_supervisor::cli::Probe as CliProbe;

use crate::manifest::{Capability, Manifest, Support};
use crate::version::{Range, Version};

/// The SDK protocol this host speaks. A manifest's `compat.protocol` has to contain it. 1.1.0 is the
/// one that reads schema 2 (typed flags, #334); a host at 1.0.0 reads schema 1 only, and a schema 2
/// manifest asks for 1.1.0 or later. (A host from before schema 2 refuses it earlier, as an unknown
/// field in its strict parser; it fails closed, with its own error.)
pub const PROTOCOL: Version = Version(1, 1, 0);

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Incompatible {
    #[error("the extension speaks SDK protocol {wants}, this host speaks {have}")]
    Protocol { wants: String, have: Version },
    #[error("the extension does not support {os} (it lists {lists})")]
    Os { os: String, lists: String },
    #[error("the CLI is version {version}, the extension was written for {wants}")]
    CliVersion { version: Version, wants: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Supported,
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Negotiated {
    pub capabilities: BTreeMap<Capability, Verdict>,
}

impl Negotiated {
    pub fn supports(&self, c: Capability) -> bool {
        matches!(self.capabilities.get(&c), Some(Verdict::Supported))
    }
}

/// The checks that need no CLI: the protocol range and the operating system.
pub fn check_host(m: &Manifest, os: &str) -> Result<(), Incompatible> {
    let wants = Range::parse(&m.compat.protocol).expect("validated by Manifest::parse");
    if !wants.contains(&PROTOCOL) {
        return Err(Incompatible::Protocol {
            wants: m.compat.protocol.clone(),
            have: PROTOCOL,
        });
    }
    if !m.compat.os.iter().any(|o| o == os) {
        return Err(Incompatible::Os {
            os: os.to_owned(),
            lists: m.compat.os.join(", "),
        });
    }
    Ok(())
}

/// Everything `check_host` says, then the CLI's version range, then each capability.
pub fn negotiate(m: &Manifest, os: &str, cli: &CliProbe) -> Result<Negotiated, Incompatible> {
    check_host(m, os)?;
    let wants = Range::parse(&m.compat.cli_versions).expect("validated by Manifest::parse");
    if !wants.contains(&cli.version) {
        return Err(Incompatible::CliVersion {
            version: cli.version,
            wants: m.compat.cli_versions.clone(),
        });
    }
    let mut capabilities = BTreeMap::new();
    for (c, declared) in &m.capabilities {
        let verdict = match declared {
            Support::Unsupported => {
                Verdict::Unsupported("the extension declares it unsupported".into())
            }
            Support::Supported => Verdict::Supported,
            Support::ProbeRequired => 'probe: {
                for p in m.probe.iter().filter(|p| p.capability == *c) {
                    if let Some(missing) = p.flags.iter().find(|f| !cli.flags.contains(*f)) {
                        break 'probe Verdict::Unsupported(format!(
                            "the CLI's --help does not list {missing}"
                        ));
                    }
                    if let Some(min) = p.min_version.as_deref().and_then(crate::version::exact) {
                        if cli.version < min {
                            break 'probe Verdict::Unsupported(format!(
                                "needs CLI {min}, this is {}",
                                cli.version
                            ));
                        }
                    }
                }
                Verdict::Supported
            }
        };
        capabilities.insert(*c, verdict);
    }
    // Amendment E4: the host has no live way to receive hook payloads yet, so the capability is not
    // offered whatever the manifest declares or the CLI lists. The stream is the only evidence.
    capabilities.insert(
        Capability::HookChannel,
        Verdict::Unsupported(
            "the host has no live way to receive hook payloads yet (#281, inline --settings is untried); the stream is the only evidence".into(),
        ),
    );
    Ok(Negotiated { capabilities })
}
