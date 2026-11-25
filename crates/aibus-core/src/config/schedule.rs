//! `[[schedule]]` (#391): a run a scheduler outside aibus starts. aibus only prints the crontab
//! line, launchd plist and systemd units for it (`aibus schedule print`); it never installs one.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::AgentName;
use crate::workflow::valid_name;

/// The most launchd `StartCalendarInterval` entries one schedule may expand to.
const MAX_ENTRIES: usize = 128;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Schedule {
    /// Names the schedule, the launchd label and the systemd units.
    pub name: String,
    pub agent: String,
    pub prompt: String,
    /// Five fields: minute hour day-of-month month day-of-week (numbers, `*`, `a-b`, `a,b`, `*/n`).
    pub cron: String,
    /// Absolute path of the project the run works in (`--cwd`).
    pub project: PathBuf,
    /// Handed to the run as `AIBUS_MAX_COST_USD`.
    pub max_cost_usd: Option<f64>,
}

/// The five cron fields as sorted values; `None` is `*`. Day of week is 0 (Sunday) to 6.
#[derive(Clone, PartialEq, Debug)]
pub struct Cron {
    pub minute: Option<Vec<u32>>,
    pub hour: Option<Vec<u32>>,
    pub day: Option<Vec<u32>>,
    pub month: Option<Vec<u32>>,
    pub weekday: Option<Vec<u32>>,
}

fn field(text: &str, lo: u32, hi: u32, what: &str) -> Result<Option<Vec<u32>>, String> {
    let bad = |why: &str| format!("cron {what} {text:?}: {why}");
    let num = |s: &str| {
        s.parse::<u32>()
            .ok()
            .filter(|n| (lo..=hi).contains(n))
            .ok_or_else(|| bad(&format!("want a number from {lo} to {hi}")))
    };
    let mut out = Vec::new();
    for item in text.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => (
                r,
                Some(
                    s.parse::<u32>()
                        .ok()
                        .filter(|s| *s > 0)
                        .ok_or_else(|| bad("the step is a number above 0"))?,
                ),
            ),
            None => (item, None),
        };
        let (from, to) = match range {
            "*" => (lo, hi),
            r => match r.split_once('-') {
                Some((a, b)) => (num(a)?, num(b)?),
                None => {
                    let n = num(r)?;
                    (n, if step.is_some() { hi } else { n })
                }
            },
        };
        if from > to {
            return Err(bad("the range runs backwards"));
        }
        out.extend((from..=to).step_by(step.unwrap_or(1) as usize));
    }
    out.sort_unstable();
    out.dedup();
    Ok(Some(out).filter(|v| v.len() as u32 != hi - lo + 1))
}

impl Schedule {
    /// The cron field sets, or why the expression is not one aibus can print for all three forms.
    pub fn cron_fields(&self) -> Result<Cron, String> {
        let parts: Vec<&str> = self.cron.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = parts[..] else {
            return Err(format!(
                "cron {:?}: want five fields, minute hour day-of-month month day-of-week",
                self.cron
            ));
        };
        let mut weekday = field(weekday, 0, 7, "day of week")?;
        if let Some(w) = &mut weekday {
            // 7 is Sunday too.
            w.iter_mut().for_each(|d| *d %= 7);
            w.sort_unstable();
            w.dedup();
            if w.len() == 7 {
                weekday = None;
            }
        }
        let cron = Cron {
            minute: field(minute, 0, 59, "minute")?,
            hour: field(hour, 0, 23, "hour")?,
            day: field(day, 1, 31, "day of month")?,
            month: field(month, 1, 12, "month")?,
            weekday,
        };
        if cron.day.is_some() && cron.weekday.is_some() {
            return Err("cron sets a day of month and day of week; cron runs on either, launchd and systemd only on both, so use one of them".into());
        }
        let n = [
            &cron.minute,
            &cron.hour,
            &cron.day,
            &cron.month,
            &cron.weekday,
        ]
        .iter()
        .map(|f| f.as_ref().map_or(1, Vec::len))
        .product::<usize>();
        if n > MAX_ENTRIES {
            return Err(format!(
                "cron expands to {n} launchd entries, at most {MAX_ENTRIES}; use shorter lists"
            ));
        }
        Ok(cron)
    }

    /// Field, message pairs for the config check.
    pub fn problems(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if !valid_name(&self.name) {
            out.push(("name", "want [a-z0-9][a-z0-9_-]{0,63}".into()));
        }
        if AgentName::new(&self.agent).is_err() {
            out.push((
                "agent",
                "want an agent name, [a-z0-9][a-z0-9_-]{0,63}".into(),
            ));
        }
        if self.prompt.trim().is_empty() {
            out.push(("prompt", "must not be empty".into()));
        } else if self.prompt.chars().any(char::is_control) {
            out.push(("prompt", "no line breaks or control characters (a crontab line and a unit file hold one line)".into()));
        }
        if let Err(why) = self.cron_fields() {
            out.push(("cron", why));
        }
        let path = self.project.to_string_lossy();
        if !self.project.is_absolute() || path.chars().any(char::is_control) {
            out.push((
                "project",
                "want an absolute path without control characters".into(),
            ));
        }
        if self
            .max_cost_usd
            .is_some_and(|c| !c.is_finite() || c <= 0.0)
        {
            out.push(("max_cost_usd", "want a number above 0".into()));
        }
        out
    }
}
