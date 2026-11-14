//! The 06 section 7 variables that change behaviour but are not paths (`XDG_*` and
//! `AIBUS_CONFIG` live in `Paths`, `AIBUS_I_KNOW` next to the permission mode, `NO_COLOR` and
//! `TERM` in `term`). A variable is read here and nowhere else, and the environment as a whole
//! is never listed or logged.

use aibus_core::config::Config;
use aibus_core::AibusError;

/// `AIBUS_MAX_SPAWN_DEPTH`, `AIBUS_MAX_CONCURRENT_RUNS` and `AIBUS_MAX_COST_USD` replace the config's limits (01
/// section 4.4). An empty value counts as unset.
pub fn apply_limits(
    cfg: &mut Config,
    get: impl Fn(&str) -> Option<String>,
) -> Result<(), AibusError> {
    let number = |var: &str| -> Result<Option<u32>, AibusError> {
        match get(var).filter(|v| !v.trim().is_empty()) {
            None => Ok(None),
            Some(v) => v.trim().parse::<u32>().map(Some).map_err(|_| {
                AibusError::Usage(format!("{var}={v}: want a whole number"))
            }),
        }
    };
    if let Some(n) = number("AIBUS_MAX_SPAWN_DEPTH")? {
        cfg.limits.max_spawn_depth = u8::try_from(n).map_err(|_| {
            AibusError::Usage(format!("AIBUS_MAX_SPAWN_DEPTH={n}: at most {}", u8::MAX))
        })?;
    }
    if let Some(n) = number("AIBUS_MAX_CONCURRENT_RUNS")? {
        if n == 0 {
            return Err(AibusError::Usage(
                "AIBUS_MAX_CONCURRENT_RUNS=0: at least 1".into(),
            ));
        }
        cfg.limits.max_concurrent_runs = n;
    }
    // The most one run may cost, in dollars: how `aibus mcp --allow run` passes the budget that is
    // left to the run it starts.
    if let Some(v) = get("AIBUS_MAX_COST_USD").filter(|v| !v.trim().is_empty()) {
        cfg.limits.max_cost_usd = v
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|c| c.is_finite() && *c > 0.0)
            .ok_or_else(|| AibusError::Usage(format!("AIBUS_MAX_COST_USD={v}: want a positive number")))?;
    }
    Ok(())
}

/// The outcome for the handoff: `--outcome`, else `AIBUS_OUTCOME` (it answers the outcome prompt
/// headlessly). Returned in its canonical form, `SUCCEEDED`, `PARTIAL_PLUS`, `PARTIAL_MINUS` or
/// `FAILED`; anything else is a usage error.
pub fn outcome(
    flag: Option<&str>,
    get: impl Fn(&str) -> Option<String>,
) -> Result<Option<String>, AibusError> {
    let (source, raw) = match flag {
        Some(f) => ("--outcome", f.to_owned()),
        None => match get("AIBUS_OUTCOME").filter(|v| !v.trim().is_empty()) {
            Some(v) => ("AIBUS_OUTCOME", v),
            None => return Ok(None),
        },
    };
    let canonical = raw.trim().to_ascii_uppercase().replace(['-', ' '], "_");
    if ["SUCCEEDED", "PARTIAL_PLUS", "PARTIAL_MINUS", "FAILED"].contains(&canonical.as_str()) {
        Ok(Some(canonical))
    } else {
        Err(AibusError::Usage(format!(
            "{source} {raw}: want SUCCEEDED, PARTIAL_PLUS, PARTIAL_MINUS or FAILED"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| (*v).to_owned())
    }

    #[test]
    fn limits_come_from_the_environment_and_bad_values_are_usage_errors() {
        let mut cfg = Config::default();
        apply_limits(&mut cfg, env(&[])).unwrap();
        assert_eq!(cfg, Config::default());

        apply_limits(
            &mut cfg,
            env(&[("AIBUS_MAX_SPAWN_DEPTH", "1"), ("AIBUS_MAX_CONCURRENT_RUNS", " 2 ")]),
        )
        .unwrap();
        assert_eq!((cfg.limits.max_spawn_depth, cfg.limits.max_concurrent_runs), (1, 2));

        for bad in [
            ("AIBUS_MAX_SPAWN_DEPTH", "deep"),
            ("AIBUS_MAX_SPAWN_DEPTH", "300"),
            ("AIBUS_MAX_CONCURRENT_RUNS", "0"),
            ("AIBUS_MAX_CONCURRENT_RUNS", "-1"),
            ("AIBUS_MAX_COST_USD", "free"),
            ("AIBUS_MAX_COST_USD", "0"),
            ("AIBUS_MAX_COST_USD", "NaN"),
        ] {
            let e = apply_limits(&mut Config::default(), env(&[bad])).unwrap_err();
            assert_eq!(e.exit_code(), 2, "{bad:?}");
        }
        apply_limits(&mut cfg, env(&[("AIBUS_MAX_COST_USD", " 0.25 ")])).unwrap();
        assert_eq!(cfg.limits.max_cost_usd, 0.25);
        // An empty variable is unset.
        apply_limits(&mut cfg, env(&[("AIBUS_MAX_SPAWN_DEPTH", "")])).unwrap();
    }

    #[test]
    fn the_flag_wins_over_the_variable_and_names_are_normalised() {
        let vars = env(&[("AIBUS_OUTCOME", "failed")]);
        assert_eq!(outcome(None, &vars).unwrap().as_deref(), Some("FAILED"));
        assert_eq!(
            outcome(Some("partial-plus"), &vars).unwrap().as_deref(),
            Some("PARTIAL_PLUS")
        );
        assert_eq!(outcome(None, env(&[])).unwrap(), None);
        assert_eq!(outcome(Some("great"), &vars).unwrap_err().exit_code(), 2);
        assert_eq!(
            outcome(None, env(&[("AIBUS_OUTCOME", "meh")])).unwrap_err().exit_code(),
            2
        );
    }
}
