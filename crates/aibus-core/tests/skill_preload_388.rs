//! The user and extra scopes reach a run's preload through `set_session_dirs`. It sets a
//! process-wide value, so it lives in its own test binary with a single test.

use std::fs;

use aibus_core::agent::discover::discover as discover_agents;
use aibus_core::context::{build, ContextInput};
use aibus_core::skill;
use aibus_core::AgentName;
use aibus_testkit::xdg_temp;

#[test]
fn preload_reads_the_user_and_extra_scopes_set_at_startup() {
    let d = xdg_temp();
    fs::create_dir_all(d.path().join(".aibus/skills")).unwrap();
    let user = d.path().join("user-skills");
    let extra = d.path().join("extra-skills");
    for (dir, name) in [(&user, "from-user"), (&extra, "from-extra")] {
        fs::create_dir_all(dir.join(name)).unwrap();
        fs::write(
            dir.join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d\n---\n{name} body\n"),
        )
        .unwrap();
    }
    skill::set_session_dirs(Some(user), vec![extra]);
    let name: AgentName = "general".parse().unwrap();
    let mut def = discover_agents(None, d.path(), None, &[], &[])
        .unwrap()
        .agents[&name]
        .clone();
    def.skills = vec!["from-user".into(), "from-extra".into()];
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
        conv.system
            .contains("## Skill: from-user\nfrom-user body\n"),
        "{}",
        conv.system
    );
    assert!(
        conv.system
            .contains("## Skill: from-extra\nfrom-extra body\n"),
        "{}",
        conv.system
    );
}
