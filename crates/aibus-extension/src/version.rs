//! Versions and ranges. A version is exactly `major.minor.patch` (no pre-release, no build
//! metadata, no leading zeros); a range is comparators joined by commas: `>=1.0.0, <2.0.0`.

pub use aibus_supervisor::cli::Version;

/// `1.2.3` and nothing else.
pub fn exact(s: &str) -> Option<Version> {
    let mut parts = s.split('.');
    let mut next = || {
        let p = parts.next()?;
        let ok = !p.is_empty()
            && p.len() <= 6
            && p.bytes().all(|b| b.is_ascii_digit())
            && (p == "0" || !p.starts_with('0'));
        ok.then(|| p.parse::<u32>().ok()).flatten()
    };
    let v = Version(next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
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
        let mut parts = Vec::new();
        for raw in s.split(',') {
            let t = raw.trim();
            let (op, rest) = if let Some(r) = t.strip_prefix(">=") {
                (Op::Ge, r)
            } else if let Some(r) = t.strip_prefix("<=") {
                (Op::Le, r)
            } else if let Some(r) = t.strip_prefix('>') {
                (Op::Gt, r)
            } else if let Some(r) = t.strip_prefix('<') {
                (Op::Lt, r)
            } else if let Some(r) = t.strip_prefix('=') {
                (Op::Eq, r)
            } else {
                return Err(format!("{t:?} does not start with >=, >, <=, < or ="));
            };
            let v = exact(rest.trim()).ok_or_else(|| format!("{t:?} has no x.y.z version"))?;
            parts.push((op, v));
        }
        Ok(Self(parts))
    }

    pub fn contains(&self, v: &Version) -> bool {
        self.0.iter().all(|(op, w)| match op {
            Op::Ge => v >= w,
            Op::Gt => v > w,
            Op::Le => v <= w,
            Op::Lt => v < w,
            Op::Eq => v == w,
        })
    }
}
