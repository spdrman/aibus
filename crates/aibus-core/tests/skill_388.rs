//! H1.1 (#388): skills the model invokes. Discovery, the bounded index, the `skill` tool on the
//! fake provider, and what `skills:` on an agent still does.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aibus_core::agent::discover::discover as discover_agents;
use aibus_core::config::Hooks;
use aibus_core::skill::{self, SkillSet, INDEX_BUDGET_TOKENS};
use aibus_core::tools::SkillTool;
use aibus_core::*;
use aibus_provider::fake::{FakeProvider, Script, Step};
use aibus_provider::{ContentBlock, StopReason};
use aibus_testkit::{fake_credential, xdg_temp};
use serde_json::{json, Value};
use tokio::sync::mpsc;

fn general(cwd: &Path) -> AgentDef {
    let name: AgentName = "general".parse().unwrap();
    discover_agents(None, cwd, None, &[], &[]).unwrap().agents[&name].clone()
}

fn skill_file(root: &Path, name: &str, description: &str, body: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("SKILL.md");
    fs::write(
        &file,
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
    )
    .unwrap();
    file
}

/// A project (it has `.aibus`) and a user skills directory beside it.
fn world() -> (aibus_testkit::Xdg, PathBuf, PathBuf) {
    let d = xdg_temp();
    let project_skills = d.path().join(".aibus/skills");
    fs::create_dir_all(&project_skills).unwrap();
    let user = d.path().join("user-skills");
    fs::create_dir_all(&user).unwrap();
    (d, project_skills, user)
}

#[test]
fn discovery_reads_name_and_description_from_the_frontmatter() {
    let (d, project, user) = world();
    skill_file(
        &project,
        "deploy",
        "Ship a release to staging",
        "Run the deploy script.",
    );
    skill_file(
        &user,
        "review",
        "Review a diff\n  for style",
        "Check names.",
    );
    let set = skill::discover(d.path(), Some(&user), &[]);
    assert!(set.findings.ok, "{:?}", set.findings);
    let names: Vec<_> = set.skills.keys().map(String::as_str).collect();
    assert_eq!(names, ["deploy", "review"], "project first, then user");
    assert_eq!(
        set.skills["deploy"].description,
        "Ship a release to staging"
    );
    assert_eq!(
        set.skills["review"].description, "Review a diff for style",
        "folded to one line"
    );
    assert_eq!(set.skills["deploy"].scope, Scope::Project);
    assert_eq!(set.skills["review"].scope, Scope::User);
    assert_eq!(
        set.body("deploy").unwrap(),
        "Run the deploy script.",
        "no frontmatter in the body"
    );
}

#[test]
fn project_overrides_user_and_the_duplicate_is_an_error() {
    let (d, project, user) = world();
    let winner = skill_file(&project, "lint", "Project lint", "project body");
    let hidden = skill_file(&user, "lint", "User lint", "user body");
    let set = skill::discover(d.path(), Some(&user), &[]);
    assert_eq!(
        set.skills["lint"].path, winner,
        "the winning path is the project's"
    );
    assert_eq!(set.body("lint").unwrap(), "project body");
    assert_eq!(set.shadowed.len(), 1);
    assert_eq!(set.shadowed[0].path, hidden);
    assert!(!set.findings.ok);
    let msg = &set.findings.errors[0].message;
    assert!(
        msg.contains("duplicate skill name") && msg.contains("lint"),
        "{msg}"
    );
}

#[test]
fn extra_dirs_come_last() {
    let (d, project, user) = world();
    let extra = d.path().join("extra");
    skill_file(&extra, "only-extra", "From an extra dir", "x");
    skill_file(&extra, "shared", "Extra copy", "x");
    skill_file(&project, "shared", "Project copy", "x");
    let set = skill::discover(d.path(), Some(&user), std::slice::from_ref(&extra));
    assert_eq!(set.skills["only-extra"].scope, Scope::Extra(extra));
    assert_eq!(set.skills["shared"].scope, Scope::Project);
}

#[test]
fn a_file_without_a_usable_frontmatter_is_reported() {
    let (d, project, _) = world();
    let dir = project.join("bare");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), "just a body").unwrap();
    let wrong = project.join("wrong");
    fs::create_dir_all(&wrong).unwrap();
    fs::write(
        wrong.join("SKILL.md"),
        "---\nname: other\ndescription: d\n---\nb",
    )
    .unwrap();
    let nodesc = project.join("nodesc");
    fs::create_dir_all(&nodesc).unwrap();
    fs::write(nodesc.join("SKILL.md"), "---\nname: nodesc\n---\nb").unwrap();

    let set = skill::discover(d.path(), None, &[]);
    // A bare file is a warning: it still preloads whole, so `agent validate` stays green on it.
    assert!(
        set.findings.warnings.iter().any(|f| f.field == "frontmatter"
            && f.path.as_ref().unwrap().ends_with("bare/SKILL.md")),
        "{:?}",
        set.findings
    );
    assert!(
        !set.findings.errors.iter().any(|f| f.field == "frontmatter"),
        "{:?}",
        set.findings
    );
    let fields: Vec<(&str, &str)> = set
        .findings
        .errors
        .iter()
        .map(|f| {
            (
                f.path
                    .as_ref()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap(),
                f.field.as_str(),
            )
        })
        .collect();
    assert!(fields.contains(&("wrong", "name")), "{fields:?}");
    assert!(fields.contains(&("nodesc", "description")), "{fields:?}");
    // A bare file still preloads, as it did before frontmatter existed.
    assert_eq!(set.body("bare").unwrap(), "just a body");
}

#[test]
fn the_index_is_bounded_and_says_what_it_left_out() {
    let (d, project, _) = world();
    for i in 0..200 {
        skill_file(
            &project,
            &format!("skill-{i:03}"),
            &format!("Does thing number {i} when the task is about topic {i}"),
            "body",
        );
    }
    let set = skill::discover(d.path(), None, &[]);
    assert_eq!(set.skills.len(), 200);
    let (text, left_out) = skill::index(&set, INDEX_BUDGET_TOKENS);
    let tokens = (text.chars().count() as u64).div_ceil(4);
    assert!(tokens <= INDEX_BUDGET_TOKENS, "{tokens} tokens");
    assert!(left_out > 0 && left_out < 200);
    assert!(
        text.contains(&format!("{left_out} more skills are not listed")),
        "{}",
        text.lines().last().unwrap()
    );
    assert!(text.contains("- skill-000: Does thing number 0"));
    assert!(!text.contains("skill-199"));
    // A small set is listed whole, with no footer.
    let (all, none) = skill::index(&set, 1_000_000);
    assert_eq!(none, 0);
    assert!(all.contains("skill-199") && !all.contains("not listed"));
}

#[test]
fn an_agent_naming_a_missing_skill_is_an_error() {
    let (d, project, _) = world();
    skill_file(&project, "present", "Here", "b");
    let agents = d.path().join(".aibus/agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        agents.join("a.md"),
        "---\nname: a\ndescription: d\nskills: [present, mispelled]\n---\nDo.\n",
    )
    .unwrap();
    let found = discover_agents(None, d.path(), None, &[], &[]).unwrap();
    let set = skill::discover(d.path(), None, &[]);
    let report = skill::check_agents(found.agents.values(), &set);
    assert!(!report.ok);
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert_eq!(report.errors[0].field, "skills");
    assert!(report.errors[0].message.contains("mispelled"));
}

#[test]
fn import_copies_only_the_skill_file_and_never_overwrites() {
    let (d, _, _) = world();
    let from = d.path().join(".claude/skills");
    skill_file(&from, "good", "A good one", "body");
    fs::write(from.join("good/helper.sh"), "echo scripts stay behind").unwrap();
    let bare = from.join("bare");
    fs::create_dir_all(&bare).unwrap();
    fs::write(bare.join("SKILL.md"), "no frontmatter").unwrap();
    let to = d.path().join(".aibus/skills");
    let done = skill::import(&from, &to).unwrap();
    assert_eq!(done.len(), 2);
    assert!(to.join("good/SKILL.md").is_file());
    assert!(!to.join("good/helper.sh").exists());
    assert!(!to.join("bare").exists());
    let again = skill::import(&from, &to).unwrap();
    assert!(again
        .iter()
        .all(|(_, o)| matches!(o, aibus_core::agent::import::Outcome::Skipped(_))));
}

#[test]
fn preloading_pastes_the_body_without_the_frontmatter_and_keeps_bare_files_whole() {
    use aibus_core::context::{build, ContextInput};
    let (d, project, _) = world();
    skill_file(&project, "framed", "Has frontmatter", "framed body");
    let bare = project.join("bare");
    fs::create_dir_all(&bare).unwrap();
    fs::write(bare.join("SKILL.md"), "bare body").unwrap();
    let mut def = general(d.path());
    def.skills = vec!["framed".into(), "bare".into(), "gone".into()];
    let conv = build(&ContextInput {
        def: &def,
        prompt: "task",
        cwd: d.path(),
        platform: "linux",
        date: "2026-11-25",
        git: None,
        data_home: None,
    });
    assert!(
        conv.system.contains("## Skill: framed\nframed body\n"),
        "{}",
        conv.system
    );
    assert!(!conv.system.contains("description: Has frontmatter"));
    assert!(conv.system.contains("## Skill: bare\nbare body\n"));
    assert!(!conv.system.contains("## Skill: gone"));
}

// ---- the tool, and a run on the fake provider --------------------------------------------

fn tool_set(d: &Path) -> Arc<SkillSet> {
    Arc::new(skill::discover(d, None, &[]))
}

#[tokio::test]
async fn the_tool_description_carries_the_index_and_a_call_loads_the_body() {
    use aibus_core::tool::{Tool, ToolCx, ToolLimits};
    let (d, project, _) = world();
    skill_file(
        &project,
        "deploy",
        "Ship a release to staging",
        "Run the deploy script.",
    );
    let tool = SkillTool::new(tool_set(d.path()));
    assert!(tool
        .spec()
        .description
        .contains("- deploy: Ship a release to staging"));
    let (tx, _rx) = mpsc::channel(4);
    let cx = ToolCx {
        run: RunId::new(),
        cwd: d.path().to_path_buf(),
        cancel: tokio_util::sync::CancellationToken::new(),
        limits: ToolLimits::default(),
        tx,
        memory: None,
    };
    let out = tool.call(&cx, json!({"name": "deploy"})).await.unwrap();
    assert_eq!(out.content, "Run the deploy script.");
    let err = tool.call(&cx, json!({"name": "nope"})).await.unwrap_err();
    assert!(err.to_string().contains("no skill named"), "{err}");
}

struct Ran {
    fake: Arc<FakeProvider>,
}

async fn run_lead(d: &Path, script: Script) -> Ran {
    let fake = Arc::new(FakeProvider::new(script));
    let (tx, mut rx) = mpsc::channel(MSG_CAPACITY);
    let mut def = general(d);
    def.permission_mode = PermissionMode::Bypass;
    let mut registry = ToolRegistry::default();
    registry.register(Arc::new(SkillTool::new(tool_set(d))));
    let env = vec![("PATH".to_owned(), std::env::var("PATH").unwrap_or_default())];
    let hooks = Arc::new(HookRunner::new(
        &Hooks::default(),
        env,
        d.to_path_buf(),
        "01SESSION".into(),
        None,
        None,
    ));
    let handle = spawn(
        RunConfig {
            run: RunId::new(),
            def,
            prompt: "ship the release".into(),
            parent: None,
            depth: 0,
            model: ModelId("fake-small".into()),
            account: AccountName("fake".into()),
            cwd: d.to_path_buf(),
            runs_dir: d.join("runs"),
            limits: ToolLimits::default(),
            budgets: Budgets::default(),
            date: "2026-11-25".into(),
            data_home: None,
            context: Vec::new(),
            background: false,
            max_spawn_depth: 3,
            forced: None,
            start: Start::New,
            worktree: None,
        },
        RunDeps {
            provider: fake.clone(),
            creds: Arc::new(fake_credential()),
            tools: Arc::new(ToolExecutor::new(registry)),
            tx,
            hooks,
            compactor: None,
            assist: None,
            fallback: None,
        },
    );
    while let Some(m) = rx.recv().await {
        if m.name() == "RunFinished" {
            break;
        }
    }
    handle.join.await.unwrap();
    Ran { fake }
}

fn tool_results(ran: &Ran, request: usize) -> Vec<String> {
    ran.fake.requests()[request]
        .request
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect()
}

fn call(name: &str, input: Value) -> [Step; 2] {
    [
        Step::ToolCall {
            name: name.into(),
            input,
        },
        Step::Stop(StopReason::ToolUse),
    ]
}

#[tokio::test]
async fn a_matching_task_loads_the_body_through_the_tool_and_an_unrelated_one_loads_nothing() {
    let (d, project, _) = world();
    skill_file(
        &project,
        "deploy",
        "Ship a release to staging",
        "Run the deploy script.",
    );

    // The model recognises the task in the index and asks for the skill.
    let steps = call("skill", json!({"name": "deploy"}))
        .into_iter()
        .chain([Step::Text("done".into())]);
    let ran = run_lead(d.path(), steps.fold(Script::new(), Script::then)).await;
    let first = &ran.fake.requests()[0].request;
    let spec = first
        .tools
        .iter()
        .find(|t| t.name == "skill")
        .expect("the skill tool is offered");
    assert!(spec
        .description
        .contains("- deploy: Ship a release to staging"));
    assert!(tool_results(&ran, 1)
        .iter()
        .any(|r| r.contains("Run the deploy script.")));

    // An unrelated task: the model answers, and no skill body is in the conversation.
    let ran = run_lead(d.path(), Script::new().then(Step::Text("hello".into()))).await;
    assert_eq!(ran.fake.requests().len(), 1);
    assert!(!ran.fake.requests()[0]
        .request
        .messages
        .iter()
        .flat_map(|m| &m.blocks)
        .any(|b| matches!(b, ContentBlock::ToolResult { .. })));
    assert!(!ran.fake.requests()[0]
        .request
        .system
        .contains("Run the deploy script."));
}

// ---- links and size at read time (review of #395) -------------------------------------------

#[cfg(unix)]
mod confinement {
    use super::*;
    use std::os::unix::fs::symlink;

    fn secret(d: &Path) -> PathBuf {
        let outside = d.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("id_ed25519"), "PRIVATE KEY MATERIAL").unwrap();
        outside
    }

    #[test]
    fn a_skill_file_that_is_a_link_out_of_the_scope_is_refused() {
        let (d, project, _) = world();
        let outside = secret(d.path());
        fs::create_dir_all(project.join("notes")).unwrap();
        symlink(outside.join("id_ed25519"), project.join("notes/SKILL.md")).unwrap();
        let set = skill::discover(d.path(), None, &[]);
        assert!(!set.skills.contains_key("notes"), "{:?}", set.skills);
        assert!(set.body("notes").is_none());
        assert!(
            set.findings
                .errors
                .iter()
                .any(|f| f.message.contains("outside the skill directory")),
            "{:?}",
            set.findings
        );
        assert!(!skill::index(&set, INDEX_BUDGET_TOKENS).0.contains("notes"));
    }

    #[test]
    fn a_skill_directory_that_is_a_link_out_of_the_scope_is_refused() {
        let (d, project, _) = world();
        let outside = secret(d.path());
        fs::write(outside.join("SKILL.md"), "---\nname: linked\ndescription: d\n---\nsecret body").unwrap();
        symlink(&outside, project.join("linked")).unwrap();
        let set = skill::discover(d.path(), None, &[]);
        assert!(!set.skills.contains_key("linked"), "{:?}", set.skills);
        assert!(set.body("linked").is_none());
        assert!(!set.findings.ok);
    }

    #[test]
    fn a_scope_directory_that_is_itself_a_link_still_works() {
        let (d, _, _) = world();
        let real = d.path().join("dotfiles-skills");
        skill_file(&real, "ok", "Fine", "body");
        let user = d.path().join("user-link");
        symlink(&real, &user).unwrap();
        let set = skill::discover(d.path(), Some(&user), &[]);
        assert_eq!(set.body("ok").unwrap(), "body");
        assert!(set.findings.ok, "{:?}", set.findings);
    }

    #[test]
    fn a_link_swapped_in_after_discovery_is_not_followed() {
        let (d, project, _) = world();
        let outside = secret(d.path());
        let file = skill_file(&project, "swap", "Fine", "body");
        let set = skill::discover(d.path(), None, &[]);
        assert_eq!(set.body("swap").unwrap(), "body");
        fs::remove_file(&file).unwrap();
        symlink(outside.join("id_ed25519"), &file).unwrap();
        assert!(set.body("swap").is_none());
    }

    #[test]
    fn import_skips_a_link_out_of_the_claude_skills_directory() {
        let d = xdg_temp();
        let outside = secret(d.path());
        let from = d.path().join(".claude/skills");
        fs::create_dir_all(from.join("stolen")).unwrap();
        symlink(outside.join("id_ed25519"), from.join("stolen/SKILL.md")).unwrap();
        let to = d.path().join(".aibus/skills");
        let done = skill::import(&from, &to).unwrap();
        assert!(done
            .iter()
            .all(|(_, o)| matches!(o, aibus_core::agent::import::Outcome::Skipped(_))));
        assert!(!to.join("stolen").exists());
    }
}

#[test]
fn a_file_that_grew_past_the_cap_after_discovery_is_not_loaded() {
    let (d, project, _) = world();
    let file = skill_file(&project, "grows", "Fine", "small");
    let set = skill::discover(d.path(), None, &[]);
    assert_eq!(set.body("grows").unwrap(), "small");
    fs::write(&file, "x".repeat(300 * 1024)).unwrap();
    assert!(set.body("grows").is_none());
}
