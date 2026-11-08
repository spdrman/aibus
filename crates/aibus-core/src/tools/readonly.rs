//! `read`, `glob`, `ls` and `grep`. Every path is canonicalised and confined to the call's
//! `cwd`; filesystem work runs on the blocking pool and polls the cancel token.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use aibus_provider::ToolSpec;
use globset::GlobBuilder;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::Lossy;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::policy::confine;
use crate::agent::PermissionMode;
use crate::tool::{Need, Tool, ToolCx, ToolError, ToolOutput};

const READ_DEFAULT_LINES: usize = 2000;
const MAX_HITS: usize = 1000;

fn input<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, ToolError> {
    serde_json::from_value(v).map_err(|e| ToolError::Invalid(e.to_string()))
}

/// Runs `f` on the blocking pool.
async fn blocking<F>(f: F) -> Result<ToolOutput, ToolError>
where
    F: FnOnce() -> Result<ToolOutput, ToolError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ToolError::Io(std::io::Error::other(e)))?
}

fn check(cancel: &CancellationToken) -> Result<(), ToolError> {
    if cancel.is_cancelled() {
        Err(ToolError::Cancelled)
    } else {
        Ok(())
    }
}

/// Regular files under `root` in walk order. `.git` is skipped and symlinks are never followed
/// or yielded, so a link cannot lead a walk out of the project.
fn files(root: &Path) -> impl Iterator<Item = PathBuf> {
    WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).to_string_lossy().into_owned()
}

fn glob_matcher(pattern: &str) -> Result<globset::GlobMatcher, ToolError> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| ToolError::Invalid(e.to_string()))
}

fn path_default() -> String {
    ".".into()
}

pub struct Read;

#[derive(Deserialize)]
struct ReadInput {
    path: String,
    /// 1-based first line.
    offset: Option<usize>,
    limit: Option<usize>,
}

#[async_trait::async_trait]
impl Tool for Read {
    fn name(&self) -> &'static str {
        "read"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "Read a file with line numbers. 2000 lines by default; `offset` is the 1-based first line.".into(),
            schema: json!({"type":"object","required":["path"],"properties":{
                "path":{"type":"string"},
                "offset":{"type":"integer","minimum":1},
                "limit":{"type":"integer","minimum":1}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: ReadInput = input(v)?;
        let path = confine(&cx.cwd, &i.path)?;
        let cancel = cx.cancel.clone();
        blocking(move || {
            let first = i.offset.unwrap_or(1).max(1);
            let limit = i.limit.unwrap_or(READ_DEFAULT_LINES);
            let mut reader = BufReader::new(File::open(path)?);
            let mut out = ToolOutput::default();
            let mut buf = Vec::new();
            let mut n = 0usize;
            loop {
                check(&cancel)?;
                buf.clear();
                if reader.read_until(b'\n', &mut buf)? == 0 {
                    break;
                }
                n += 1;
                if n < first {
                    continue;
                }
                if n - first >= limit {
                    out.truncated = true;
                    break;
                }
                let line = String::from_utf8_lossy(&buf);
                out.content.push_str(&format!("{n:>6}\t{}\n", line.trim_end_matches(['\n', '\r'])));
            }
            Ok(out)
        })
        .await
    }
}

pub struct Ls;

#[derive(Deserialize)]
struct LsInput {
    #[serde(default = "path_default")]
    path: String,
}

#[async_trait::async_trait]
impl Tool for Ls {
    fn name(&self) -> &'static str {
        "ls"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ls".into(),
            description: "List a directory. Directories end in `/`, symlinks in `@`.".into(),
            schema: json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: LsInput = input(v)?;
        let dir = confine(&cx.cwd, &i.path)?;
        blocking(move || {
            let mut names = Vec::new();
            for e in std::fs::read_dir(dir)? {
                let e = e?;
                let t = e.file_type()?;
                let mut n = e.file_name().to_string_lossy().into_owned();
                if t.is_symlink() {
                    n.push('@');
                } else if t.is_dir() {
                    n.push('/');
                }
                names.push(n);
            }
            names.sort();
            Ok(ToolOutput {
                content: names.join("\n"),
                ..Default::default()
            })
        })
        .await
    }
}

pub struct Glob;

#[derive(Deserialize)]
struct GlobInput {
    pattern: String,
}

#[async_trait::async_trait]
impl Tool for Glob {
    fn name(&self) -> &'static str {
        "glob"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob".into(),
            description: "Find files by glob relative to the project, e.g. `src/**/*.rs`.".into(),
            schema: json!({"type":"object","required":["pattern"],"properties":{"pattern":{"type":"string"}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: GlobInput = input(v)?;
        let outside = Path::new(&i.pattern).is_absolute()
            || Path::new(&i.pattern)
                .components()
                .any(|c| c == std::path::Component::ParentDir);
        if outside {
            return Err(ToolError::Denied {
                reason: "path outside project".into(),
            });
        }
        let matcher = glob_matcher(&i.pattern)?;
        let root = confine(&cx.cwd, ".")?;
        let cancel = cx.cancel.clone();
        blocking(move || {
            let mut hits = Vec::new();
            for f in files(&root) {
                check(&cancel)?;
                let r = rel(&root, &f);
                if matcher.is_match(&r) {
                    hits.push(r);
                }
            }
            hits.sort();
            let truncated = hits.len() > MAX_HITS;
            hits.truncate(MAX_HITS);
            Ok(ToolOutput {
                content: hits.join("\n"),
                truncated,
                ..Default::default()
            })
        })
        .await
    }
}

pub struct Grep;

#[derive(Deserialize)]
struct GrepInput {
    pattern: String,
    #[serde(default = "path_default")]
    path: String,
    glob: Option<String>,
}

#[async_trait::async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents with a regex (ripgrep semantics). Output is `path:line:text`.".into(),
            schema: json!({"type":"object","required":["pattern"],"properties":{
                "pattern":{"type":"string"},
                "path":{"type":"string"},
                "glob":{"type":"string"}}}),
        }
    }

    fn needs_permission(&self, _: &Value, _: PermissionMode) -> Need {
        Need::Never
    }

    async fn call(&self, cx: &ToolCx, v: Value) -> Result<ToolOutput, ToolError> {
        let i: GrepInput = input(v)?;
        let target = confine(&cx.cwd, &i.path)?;
        let root = confine(&cx.cwd, ".")?;
        let matcher = RegexMatcherBuilder::new()
            .build(&i.pattern)
            .map_err(|e| ToolError::Invalid(e.to_string()))?;
        let name_glob = i.glob.as_deref().map(glob_matcher).transpose()?;
        let cancel = cx.cancel.clone();
        blocking(move || {
            let mut searcher = SearcherBuilder::new()
                .line_number(true)
                .binary_detection(BinaryDetection::quit(0))
                .build();
            let candidates: Vec<PathBuf> = if target.is_file() {
                vec![target]
            } else {
                files(&target).collect()
            };
            let mut lines = Vec::new();
            let mut truncated = false;
            for f in candidates {
                check(&cancel)?;
                let r = rel(&root, &f);
                if name_glob
                    .as_ref()
                    .is_some_and(|g| !(g.is_match(&r) || f.file_name().is_some_and(|n| g.is_match(n))))
                {
                    continue;
                }
                let sink = Lossy(|n, text| {
                    if lines.len() >= MAX_HITS {
                        truncated = true;
                        return Ok(false);
                    }
                    lines.push(format!("{r}:{n}:{}", text.trim_end_matches(['\n', '\r'])));
                    Ok(true)
                });
                // Unreadable files (permissions, races) are skipped like ripgrep does.
                let _ = searcher.search_path(&matcher, &f, sink);
                if truncated {
                    break;
                }
            }
            Ok(ToolOutput {
                content: lines.join("\n"),
                truncated,
                ..Default::default()
            })
        })
        .await
    }
}
