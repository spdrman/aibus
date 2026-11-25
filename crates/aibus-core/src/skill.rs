//! Skills (EPIC H, H1.1): a directory with a `SKILL.md` whose frontmatter carries a `name` and a
//! `description`. The model sees an index of those and loads a body with the `skill` tool when the
//! task matches; `skills:` on an agent still preloads bodies into its system prompt.
//!
//! Scopes, first match by name wins: the project (`.aibus/skills`), the user
//! (`~/.config/aibus/skills`), then `skills.extra_dirs`. A skill is `<dir>/<name>/SKILL.md`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use indexmap::IndexMap;
use yaml_rust2::{Yaml, YamlLoader};

use crate::agent::discover::project_root;
use crate::agent::import::Outcome;
use crate::agent::roster::estimate_tokens;
use crate::agent::validate::{Finding, Report};
use crate::agent::{AgentDef, Scope};
use crate::tools::policy::resolve;

/// The index in the `skill` tool description stays under this many tokens.
pub const INDEX_BUDGET_TOKENS: u64 = 1_500;
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_NAME: usize = 64;
const MAX_DESCRIPTION: usize = 1_024;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Skill {
    /// The directory name, which is also what `skills:` and the tool say.
    pub name: String,
    /// One line. Empty for a file with no frontmatter (those still preload, as before).
    pub description: String,
    pub scope: Scope,
    pub path: PathBuf,
    /// The scope directory the skill was found in. Reads stay under it, whatever links are inside.
    pub root: PathBuf,
}

#[derive(Debug, Default)]
pub struct SkillSet {
    /// The winners, in precedence order.
    pub skills: IndexMap<String, Skill>,
    /// Skills hidden by a higher scope (or an earlier extra dir) with the same name.
    pub shadowed: Vec<Skill>,
    /// Everything wrong with a file, as `skill validate` reports it.
    pub findings: Report,
}

/// The frontmatter of a `SKILL.md` and what follows it.
pub fn split(text: &str) -> Result<(Option<String>, Option<String>, &str), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    if lines.next().is_none_or(|l| l.trim_end() != "---") {
        return Err("no frontmatter: the file must start with a `---` line".into());
    }
    let mut offset = text.find('\n').map_or(0, |n| n + 1);
    let start = offset;
    for line in lines {
        if line.trim_end() == "---" {
            let yaml = &text[start..offset];
            let docs = YamlLoader::load_from_str(yaml)
                .map_err(|e| format!("frontmatter is not valid YAML: {e}"))?;
            let field = |key: &str| match docs.first() {
                Some(Yaml::Hash(h)) => match h.get(&Yaml::String(key.into())) {
                    Some(Yaml::String(s)) => {
                        Some(s.split_whitespace().collect::<Vec<_>>().join(" "))
                    }
                    _ => None,
                },
                _ => None,
            };
            if !matches!(docs.first(), None | Some(Yaml::Null | Yaml::Hash(_))) {
                return Err("frontmatter is not a mapping".into());
            }
            return Ok((
                field("name"),
                field("description"),
                &text[offset + line.len()..],
            ));
        }
        offset += line.len();
    }
    Err("the frontmatter is not closed with a second `---` line".into())
}

/// A name that is safe as a directory and in a tool call.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

static SESSION_DIRS: OnceLock<(Option<PathBuf>, Vec<PathBuf>)> = OnceLock::new();

/// Records where this process looks for user and extra skills, once, at startup, so that a run's
/// preloaded skills (`context::build`) resolve in the same scopes as the `skill` tool. A second call
/// is ignored.
pub fn set_session_dirs(user_dir: Option<PathBuf>, extra_dirs: Vec<PathBuf>) {
    let _ = SESSION_DIRS.set((user_dir, extra_dirs));
}

/// What `set_session_dirs` recorded; nothing before it ran, which is the project scope only.
pub fn session_dirs() -> (Option<&'static Path>, &'static [PathBuf]) {
    SESSION_DIRS
        .get()
        .map_or((None, &[][..]), |(u, e)| (u.as_deref(), e.as_slice()))
}

/// The text of `<root>/<name>/SKILL.md`. The file is resolved through every link and must still be
/// under `root` (the `read` tool's rule, `tools::policy::resolve`), so a link that leaves the scope
/// is refused. The size cap is applied to what is read now, not to what discovery saw.
fn read_skill(root: &Path, name: &str) -> Result<String, String> {
    let (real, inside) =
        resolve(root, &format!("{name}/SKILL.md")).map_err(|e| format!("cannot read: {e}"))?;
    if !inside {
        return Err(format!(
            "resolves to {}, outside the skill directory {}",
            real.display(),
            root.display()
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(&real)
        .and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("cannot read: {e}"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("larger than {MAX_FILE_BYTES} bytes"));
    }
    String::from_utf8(bytes).map_err(|e| format!("cannot read: {e}"))
}

/// Reads every scope: `.aibus/skills` of the project, the user directory, then the extra ones.
/// A bad file is a finding and never stops the walk.
pub fn discover(cwd: &Path, user_dir: Option<&Path>, extra_dirs: &[PathBuf]) -> SkillSet {
    /// Most skill directories read from one scope.
    const MAX_SKILLS_PER_SCOPE: usize = 1_000;
    let mut set = SkillSet::default();
    let scopes = project_root(cwd)
        .map(|r| (Scope::Project, r.join(".aibus/skills")))
        .into_iter()
        .chain(user_dir.map(|d| (Scope::User, d.to_owned())))
        .chain(
            extra_dirs
                .iter()
                .map(|d| (Scope::Extra(d.clone()), d.clone())),
        );
    for (scope, dir) in scopes {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = read
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        entries.sort();
        if entries.len() > MAX_SKILLS_PER_SCOPE {
            set.findings.warnings.push(Finding {
                path: Some(dir.clone()),
                field: "path".into(),
                message: format!("more than {MAX_SKILLS_PER_SCOPE} skills, reading the first {MAX_SKILLS_PER_SCOPE}"),
            });
            entries.truncate(MAX_SKILLS_PER_SCOPE);
        }
        for entry in entries {
            let file = entry.join("SKILL.md");
            if !file.is_file() {
                continue;
            }
            let dir_name = entry
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut finding = |field: &str, message: String, error: bool| {
                let f = Finding {
                    path: Some(file.clone()),
                    field: field.into(),
                    message,
                };
                if error {
                    set.findings.errors.push(f)
                } else {
                    set.findings.warnings.push(f)
                }
            };
            if !valid_name(&dir_name) {
                finding("name", format!("directory name `{dir_name}` is not a skill name (letters, digits, `-_.`, at most {MAX_NAME})"), true);
                continue;
            }
            let text = match read_skill(&dir, &dir_name) {
                Ok(t) => t,
                Err(why) => {
                    finding("path", why, true);
                    continue;
                }
            };
            let description = match split(&text) {
                Err(why) => {
                    // A file with no frontmatter keeps preloading whole, as it always did, so
                    // that alone is a warning; a frontmatter that is broken is an error.
                    let bare = why.starts_with("no frontmatter");
                    finding("frontmatter", why, !bare);
                    String::new()
                }
                Ok((name, description, _)) => {
                    match name {
                        None => finding("name", "missing required field `name`".into(), true),
                        Some(n) if n != dir_name => finding(
                            "name",
                            format!("`{n}` does not match the directory `{dir_name}`"),
                            true,
                        ),
                        Some(_) => {}
                    }
                    match description {
                        None => {
                            finding(
                                "description",
                                "missing required field `description`".into(),
                                true,
                            );
                            String::new()
                        }
                        Some(d) if d.is_empty() => {
                            finding("description", "`description` is empty".into(), true);
                            d
                        }
                        Some(d) if d.chars().count() > MAX_DESCRIPTION => {
                            finding(
                                "description",
                                format!("longer than {MAX_DESCRIPTION} characters"),
                                true,
                            );
                            d
                        }
                        Some(d) => d,
                    }
                }
            };
            let skill = Skill {
                name: dir_name.clone(),
                description,
                scope: scope.clone(),
                path: file.clone(),
                root: dir.clone(),
            };
            match set.skills.get(&dir_name) {
                Some(winner) => {
                    set.findings.errors.push(Finding {
                        path: Some(file.clone()),
                        field: "name".into(),
                        message: format!(
                            "duplicate skill name `{dir_name}`, `{}` wins",
                            winner.path.display()
                        ),
                    });
                    set.shadowed.push(skill);
                }
                None => {
                    set.skills.insert(dir_name, skill);
                }
            }
        }
    }
    set.findings.ok = set.findings.errors.is_empty();
    set
}

impl SkillSet {
    /// The body of `name` without its frontmatter. A file with no frontmatter is returned whole.
    pub fn body(&self, name: &str) -> Option<String> {
        let skill = self.skills.get(name)?;
        let text = read_skill(&skill.root, name).ok()?;
        Some(match split(&text) {
            Ok((_, _, body)) => body.trim().to_owned(),
            Err(_) => text.trim_end().to_owned(),
        })
    }
}

/// The index for the tool description: `- name: description` lines until the budget is spent,
/// then a line saying how many were left out. Returns the text and the number left out.
pub fn index(set: &SkillSet, budget_tokens: u64) -> (String, usize) {
    const FOOTER_TOKENS: u64 = 60;
    /// The longest description the index shows for one skill.
    const INDEX_LINE_CHARS: usize = 200;
    let mut out = String::new();
    let mut listed = 0;
    for skill in set.skills.values() {
        let mut line = format!("- {}", skill.name);
        if !skill.description.is_empty() {
            let d: String = skill.description.chars().take(INDEX_LINE_CHARS).collect();
            line.push_str(&format!(": {d}"));
        }
        line.push('\n');
        if estimate_tokens(&out) + estimate_tokens(&line) + FOOTER_TOKENS > budget_tokens {
            break;
        }
        out.push_str(&line);
        listed += 1;
    }
    let left_out = set.skills.len() - listed;
    if left_out > 0 {
        out.push_str(&format!(
            "[{left_out} more skills are not listed to keep this under {budget_tokens} tokens; skill(name) still loads any of them by name, and `aibus skill list` shows all]\n"
        ));
    }
    (out, left_out)
}

/// What a definition's `skills:` promise against what exists: an unresolved name is an error.
pub fn check_agents<'a>(defs: impl IntoIterator<Item = &'a AgentDef>, set: &SkillSet) -> Report {
    let mut r = Report::default();
    for def in defs {
        for name in def
            .skills
            .iter()
            .filter(|n| !set.skills.contains_key(n.as_str()))
        {
            r.errors.push(Finding {
                path: def.source.path.clone(),
                field: "skills".into(),
                message: format!(
                    "skill `{name}` not found in the project, user or extra skill directories"
                ),
            });
        }
    }
    r.ok = r.errors.is_empty();
    r
}

/// Copies `<from>/<name>/SKILL.md` into `<to>/<name>/` for every skill in `from` (a `.claude/skills`
/// directory). A skill that is already there or does not parse is skipped, and so is anything
/// next to `SKILL.md`: skills that carry scripts are not part of this step.
pub fn import(from: &Path, to: &Path) -> Result<Vec<(PathBuf, Outcome)>, String> {
    let read = fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    let mut dirs: Vec<PathBuf> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let mut done = Vec::new();
    for dir in dirs {
        let file = dir.join("SKILL.md");
        if !file.is_file() {
            continue;
        }
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let outcome = (|| {
            if !valid_name(&name) {
                return Outcome::Skipped(format!("`{name}` is not a skill name"));
            }
            let text = match read_skill(from, &name) {
                Ok(t) => t,
                Err(why) => return Outcome::Skipped(why),
            };
            match split(&text) {
                Ok((Some(n), Some(d), _)) if n == name && !d.is_empty() => {}
                Ok(_) => {
                    return Outcome::Skipped(
                        "needs a `name` matching its directory and a `description`".into(),
                    )
                }
                Err(why) => return Outcome::Skipped(why),
            }
            let target = to.join(&name);
            if target.exists() {
                return Outcome::Skipped(format!("`{}` already exists", target.display()));
            }
            match fs::create_dir_all(&target)
                .and_then(|()| fs::write(target.join("SKILL.md"), &text))
            {
                Ok(()) => Outcome::Copied(target.join("SKILL.md")),
                Err(e) => Outcome::Skipped(format!("cannot write: {e}")),
            }
        })();
        done.push((file, outcome));
    }
    Ok(done)
}
