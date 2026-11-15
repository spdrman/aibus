//! Versions and ranges. A version is exactly `major.minor.patch` (no pre-release, no build
//! metadata, no leading zeros); a range is comparators joined by commas: `>=1.0.0, <2.0.0`.

pub use aibus_supervisor::cli::Version;

/// `1.2.3` and nothing else.
pub fn exact(s: &str) -> Option<Version> {
    todo!("{s}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
}

/// One or more comparators, all of which must hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Range(Vec<(Op, Version)>);

impl Range {
    pub fn parse(s: &str) -> Result<Self, String> {
        todo!("{s}")
    }

    pub fn contains(&self, v: &Version) -> bool {
        todo!("{v}")
    }
}
