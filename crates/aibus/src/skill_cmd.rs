//! The `skill` subcommands: list, show, validate and import (EPIC H, H1.1). Skills are text the
//! model loads on demand; nothing here starts a run.

use std::path::Path;

use aibus_core::agent::import::Outcome;
use aibus_core::persist::paths::project_dir;
use aibus_core::skill::{self, SkillSet};
use aibus_core::AibusError;
use serde_json::{json, Value};

use crate::agent_cmd::{print_report, scope_name, Env};
use crate::cli::{Global, SkillCmd};

pub fn skill(cmd: &SkillCmd, g: &Global) -> Result<u8, AibusError> {
    let env = Env::load(g)?;
    match cmd {
        SkillCmd::List { names } => list(&env, g, *names),
        SkillCmd::Show { name } => show(&env, g, name),
        SkillCmd::Validate => validate(&env, g),
        SkillCmd::Import { dir } => import(&env, g, dir),
    }
}

fn row(set: &SkillSet, name: &str) -> Value {
    let s = &set.skills[name];
    json!({
        "name": s.name,
        "scope": scope_name(&s.scope),
        "path": s.path,
        "description": s.description,
        "shadowed": set.shadowed.iter().filter(|o| o.name == name).map(|o| &o.path).collect::<Vec<_>>(),
    })
}

fn list(env: &Env, g: &Global, names: bool) -> Result<u8, AibusError> {
    let set = env.skill_set();
    if names {
        for name in set.skills.keys() {
            println!("{name}");
        }
        return Ok(0);
    }
    let rows: Vec<Value> = set.skills.keys().map(|n| row(&set, n)).collect();
    if g.json {
        println!("{}", Value::Array(rows));
        return Ok(0);
    }
    for r in &rows {
        let text = |k: &str| r[k].as_str().unwrap_or("-").to_owned();
        println!(
            "{}\t{}\t{}\t{}",
            text("name"),
            text("scope"),
            text("path"),
            text("description")
        );
        for hidden in r["shadowed"].as_array().into_iter().flatten() {
            println!("  shadows {}", hidden.as_str().unwrap_or("-"));
        }
    }
    Ok(0)
}

fn show(env: &Env, g: &Global, name: &str) -> Result<u8, AibusError> {
    let set = env.skill_set();
    let body = set
        .body(name)
        .ok_or_else(|| AibusError::Usage(format!("no skill named {name}")))?;
    if g.json {
        let mut v = row(&set, name);
        v["body"] = Value::String(body);
        println!(
            "{}",
            serde_json::to_string_pretty(&v).map_err(std::io::Error::other)?
        );
    } else {
        println!("{body}");
    }
    Ok(0)
}

fn validate(env: &Env, g: &Global) -> Result<u8, AibusError> {
    let mut report = env.skill_set().findings;
    report.merge(env.agent_skill_report(None));
    report.ok = report.errors.is_empty();
    if g.json {
        println!(
            "{}",
            serde_json::to_string(&report).map_err(std::io::Error::other)?
        );
    } else {
        print_report(&report);
    }
    Ok(if report.ok { 0 } else { 12 })
}

/// `skill import <dir>`: copies the `SKILL.md` of each skill in `dir` (`.claude/skills`) into
/// `.aibus/skills/`. Nothing else in a skill directory is copied.
fn import(env: &Env, g: &Global, dir: &Path) -> Result<u8, AibusError> {
    let from = env.cwd.join(dir);
    let to = project_dir(&env.cwd).join("skills");
    let done = skill::import(&from, &to).map_err(AibusError::Usage)?;
    if g.json {
        let rows: Vec<Value> = done
            .iter()
            .map(|(file, outcome)| {
                let (status, detail) = match outcome {
                    Outcome::Copied(p) => ("copied", p.display().to_string()),
                    Outcome::Skipped(why) => ("skipped", why.clone()),
                };
                json!({ "file": file, "status": status, "detail": detail })
            })
            .collect();
        println!("{}", Value::Array(rows));
        return Ok(0);
    }
    for (file, outcome) in &done {
        match outcome {
            Outcome::Copied(p) => println!("imported {} -> {}", file.display(), p.display()),
            Outcome::Skipped(why) => println!("skipped {}: {why}", file.display()),
        }
    }
    if done.is_empty() {
        println!("no skills (<name>/SKILL.md) in {}", from.display());
    }
    Ok(0)
}
