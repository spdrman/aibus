//! Snapshot migrations (03 section 9). A snapshot written by an older aibus is brought up to
//! `SCHEMA` on the raw `Snapshot`, before it is hydrated; each step is a pure function and
//! `fixtures/snapshots/v*/` holds a snapshot of every version to prove the chain still runs.

use aibus_graph::{Snapshot, Value};

use super::{SCHEMA, SCHEMA_VERSION};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MigrateError {
    #[error("the snapshot is from a newer aibus (schema {found}, this build reads up to {SCHEMA})")]
    Newer { found: i64 },
    #[error("no migration from schema {0}")]
    Missing(i64),
    #[error("migrating from schema {from}: {message}")]
    Step { from: i64, message: String },
}

type Step = fn(Snapshot) -> Result<Snapshot, MigrateError>;

/// `STEPS[n - 1]` takes schema `n` to `n + 1`. Schema 1 is the first, so the table is empty
/// until a change to the node set needs one.
const STEPS: [Step; (SCHEMA - 1) as usize] = [];

/// The schema a snapshot was written with; a snapshot without the node is schema 1.
pub fn version_of(snap: &Snapshot) -> i64 {
    match snap.inputs.get(SCHEMA_VERSION) {
        Some(Value::I64(n)) => *n,
        _ => 1,
    }
}

/// Runs every step from `from` up to `SCHEMA` and stamps the result with the current version.
pub fn chain(from: i64, mut snap: Snapshot) -> Result<Snapshot, MigrateError> {
    if from > SCHEMA {
        return Err(MigrateError::Newer { found: from });
    }
    for v in from..SCHEMA {
        let step = usize::try_from(v - 1)
            .ok()
            .and_then(|i| STEPS.get(i))
            .ok_or(MigrateError::Missing(v))?;
        snap = step(snap)?;
    }
    snap.inputs.insert(SCHEMA_VERSION.into(), Value::I64(SCHEMA));
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn snap(version: Option<i64>) -> Snapshot {
        let mut inputs = IndexMap::new();
        if let Some(v) = version {
            inputs.insert(SCHEMA_VERSION.into(), Value::I64(v));
        }
        Snapshot { now: 7, inputs }
    }

    #[test]
    fn the_current_version_passes_through_and_a_missing_one_is_version_1() {
        assert_eq!(version_of(&snap(None)), 1);
        assert_eq!(version_of(&snap(Some(SCHEMA))), SCHEMA);
        let out = chain(version_of(&snap(None)), snap(None)).unwrap();
        assert_eq!(out.inputs.get(SCHEMA_VERSION), Some(&Value::I64(SCHEMA)));
        assert_eq!(out.now, 7);
    }

    #[test]
    fn a_newer_snapshot_is_refused_and_version_zero_has_no_path() {
        assert_eq!(
            chain(SCHEMA + 1, snap(Some(SCHEMA + 1))),
            Err(MigrateError::Newer { found: SCHEMA + 1 })
        );
        assert_eq!(chain(0, snap(Some(0))), Err(MigrateError::Missing(0)));
    }
}
