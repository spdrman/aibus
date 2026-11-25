//! `aibus schedule print` (#391, H1.4): renders each `[[schedule]]` of the config as a crontab
//! line, a launchd plist and a systemd service with its timer. It prints and installs nothing, and
//! never starts a run: the scheduler is the user's.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use aibus_core::config::{self, Cron, Schedule};
use aibus_core::persist::paths::project_dir;
use aibus_core::AibusError;
use serde_json::json;

use crate::cli::{Global, ScheduleCmd};

fn env(s: &Schedule) -> BTreeMap<&'static str, String> {
    s.max_cost_usd
        .map(|c| ("AIBUS_MAX_COST_USD", c.to_string()))
        .into_iter()
        .collect()
}

fn crontab(s: &Schedule, argv: &[String]) -> String {
    /// One word for a POSIX shell: bare when it is plain, else in single quotes.
    fn shell_word(w: &str) -> String {
        if !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c))
        {
            return w.to_owned();
        }
        format!("'{}'", w.replace('\'', "'\\''"))
    }

    let when = s.cron.split_whitespace().collect::<Vec<_>>().join(" ");
    let env: String = env(s)
        .iter()
        .map(|(k, v)| format!("{k}={} ", shell_word(v)))
        .collect();
    let words: Vec<String> = argv.iter().map(|w| shell_word(w)).collect();
    // A `%` in a crontab line is a newline unless it is escaped.
    format!("{when} {env}{}\n", words.join(" ")).replace('%', "\\%")
}

fn launchd(s: &Schedule, argv: &[String], cron: &Cron) -> String {
    fn xml(t: &str) -> String {
        t.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n");
    out.push_str("<plist version=\"1.0\">\n<dict>\n");
    let _ = writeln!(
        out,
        "  <key>Label</key>\n  <string>aibus.{}</string>\n  <key>ProgramArguments</key>\n  <array>",
        xml(&s.name)
    );
    for a in argv {
        let _ = writeln!(out, "    <string>{}</string>", xml(a));
    }
    out.push_str("  </array>\n");
    let env = env(s);
    if !env.is_empty() {
        out.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
        for (k, v) in env {
            let _ = writeln!(out, "    <key>{k}</key>\n    <string>{}</string>", xml(&v));
        }
        out.push_str("  </dict>\n");
    }
    out.push_str("  <key>StartCalendarInterval</key>\n  <array>\n");
    let each = |f: &Option<Vec<u32>>| {
        f.as_ref()
            .map_or_else(|| vec![None], |v| v.iter().map(|n| Some(*n)).collect())
    };
    for month in each(&cron.month) {
        for day in each(&cron.day) {
            for weekday in each(&cron.weekday) {
                for hour in each(&cron.hour) {
                    for minute in each(&cron.minute) {
                        out.push_str("    <dict>\n");
                        for (key, n) in [
                            ("Month", month),
                            ("Day", day),
                            ("Weekday", weekday),
                            ("Hour", hour),
                            ("Minute", minute),
                        ] {
                            if let Some(n) = n {
                                let _ = writeln!(
                                    out,
                                    "      <key>{key}</key>\n      <integer>{n}</integer>"
                                );
                            }
                        }
                        out.push_str("    </dict>\n");
                    }
                }
            }
        }
    }
    out.push_str("  </array>\n</dict>\n</plist>\n");
    out
}

fn systemd(s: &Schedule, argv: &[String], cron: &Cron) -> (String, String) {
    /// One word of a systemd `ExecStart=`: `%` and `$` are specifiers there, so they double.
    fn unit_word(w: &str) -> String {
        if !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_@+=:,./-".contains(c))
        {
            return w.to_owned();
        }
        let escaped = w
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$");
        format!("\"{escaped}\"")
    }

    let mut service = format!(
        "[Unit]\nDescription=aibus scheduled run {}\n\n[Service]\nType=oneshot\n",
        s.name
    );
    for (k, v) in env(s) {
        let _ = writeln!(service, "Environment={}", unit_word(&format!("{k}={v}")));
    }
    let words: Vec<String> = argv.iter().map(|w| unit_word(w)).collect();
    let _ = writeln!(service, "ExecStart={}", words.join(" "));

    let list = |f: &Option<Vec<u32>>| {
        f.as_ref().map_or_else(
            || "*".to_owned(),
            |v| {
                v.iter()
                    .map(|n| format!("{n:02}"))
                    .collect::<Vec<_>>()
                    .join(",")
            },
        )
    };
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let weekdays = cron.weekday.as_ref().map(|v| {
        v.iter()
            .map(|d| DAYS[*d as usize])
            .collect::<Vec<_>>()
            .join(",")
    });
    let on = format!(
        "{}*-{}-{} {}:{}:00",
        weekdays.map_or_else(String::new, |w| format!("{w} ")),
        list(&cron.month),
        list(&cron.day),
        list(&cron.hour),
        list(&cron.minute)
    );
    let timer = format!(
        "[Unit]\nDescription=aibus schedule {}\n\n[Timer]\nOnCalendar={on}\n\n[Install]\nWantedBy=timers.target\n",
        s.name
    );
    (service, timer)
}

pub fn schedule(cmd: &ScheduleCmd, g: &Global) -> Result<u8, AibusError> {
    let ScheduleCmd::Print { name, bin } = cmd;
    let paths = g.paths()?;
    let project = project_dir(&g.project_dir()?).join("settings.toml");
    let loaded = config::load(&paths.config_file(), Some(&project))?;
    for w in &loaded.warnings {
        eprintln!("aibus: warning: {w}");
    }
    let bin: PathBuf = match bin {
        Some(b) => b.clone(),
        None => std::env::current_exe()?,
    };
    if !bin.is_absolute() {
        return Err(AibusError::Usage(format!(
            "--bin {}: a scheduler needs an absolute path",
            bin.display()
        )));
    }
    let bin = bin.to_string_lossy();
    let picked: Vec<&Schedule> = loaded
        .config
        .schedules
        .iter()
        .filter(|s| name.as_ref().is_none_or(|n| *n == s.name))
        .collect();
    if let (Some(n), true) = (name, picked.is_empty()) {
        return Err(AibusError::Usage(format!("no [[schedule]] named {n}")));
    }
    if picked.is_empty() {
        eprintln!("aibus: no [[schedule]] in the config (docs/MCP.md, \"Scheduled runs\")");
        return Ok(0);
    }
    let mut rows = Vec::new();
    let mut text = Vec::new();
    for s in picked {
        let cron = s.cron_fields().map_err(AibusError::Usage)?;
        let project = s.project.to_string_lossy();
        // The command a schedule runs, the binary first.
        let argv: Vec<String> = [
            &*bin,
            "run",
            "--agent",
            &s.agent,
            "--permission-mode",
            "dont-ask",
            "--cwd",
            &project,
            "--",
            &s.prompt,
        ]
        .map(str::to_owned)
        .into();
        let (crontab, plist, (service, timer)) = (
            crontab(s, &argv),
            launchd(s, &argv, &cron),
            systemd(s, &argv, &cron),
        );
        text.push(format!(
            "==> crontab line <==\n{crontab}\n==> ~/Library/LaunchAgents/aibus.{n}.plist <==\n{plist}\n==> ~/.config/systemd/user/aibus-{n}.service <==\n{service}\n==> ~/.config/systemd/user/aibus-{n}.timer <==\n{timer}",
            n = s.name
        ));
        rows.push(json!({
            "name": s.name,
            "argv": argv,
            "env": env(s),
            "crontab": crontab,
            "launchd": plist,
            "systemd": { "service": service, "timer": timer },
        }));
    }
    if g.json {
        println!(
            "{}",
            serde_json::to_string(&rows).map_err(std::io::Error::other)?
        );
    } else {
        print!("{}", text.join("\n"));
    }
    eprintln!("aibus: schedule print only prints, nothing is installed");
    Ok(0)
}
