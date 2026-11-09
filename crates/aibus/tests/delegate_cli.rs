//! B2.1 from the command line: `--agent`, `@agent-<name>` and the limits in the environment.

mod common;

use std::fs;

use common::{aibus, command, ndjson, stderr};
use aibus_testkit::TempDir;

#[test]
fn an_unknown_agent_exits_3() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["run", "--agent", "nope", "hi"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("no agent named nope"), "{}", stderr(&out));

    let out = aibus(home.path(), &["run", "@agent-nope look at this"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("no agent named nope"), "{}", stderr(&out));
}

#[test]
fn a_mention_with_nothing_to_do_is_a_usage_error() {
    let home = TempDir::new().unwrap();
    let out = aibus(home.path(), &["run", "@agent-explore"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[test]
fn a_mention_delegates_and_the_lead_answers_with_the_result_in_hand() {
    let home = TempDir::new().unwrap();
    let script = home.path().join("s.ron");
    // The scripted fake is shared: the child takes the first turn, the lead the second.
    fs::write(&script, r#"[Text("the child report"), Stop(EndTurn), Text("all done")]"#).unwrap();
    let out = command(home.path())
        .args(["run", "--json", "--script"])
        .arg(&script)
        .arg("@agent-explore map the repo")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));

    let lines = ndjson(&out);
    let agents: Vec<&str> = lines
        .iter()
        .filter(|l| l["type"] == "RunStarted")
        .map(|l| l["agent"].as_str().unwrap())
        .collect();
    assert_eq!(agents, ["lead", "explore"]);
    let done: Vec<(&str, &str)> = lines
        .iter()
        .filter(|l| l["type"] == "RunFinished")
        .map(|l| (l["status"].as_str().unwrap(), l["summary"].as_str().unwrap()))
        .collect();
    assert_eq!(done, [("completed", "the child report"), ("completed", "all done")]);
    // The lead called `delegate`, once, and got the child's report back as its result.
    let req = lines.iter().find(|l| l["type"] == "ToolCallRequested").unwrap();
    assert_eq!(req["call"]["name"], "delegate");
    assert_eq!(req["call"]["input"]["agent"], "explore");
    assert_eq!(req["call"]["input"]["prompt"], "map the repo");
    assert_eq!(lines.last().unwrap()["exit_code"], 0);
}

#[test]
fn agent_submits_its_initial_prompt() {
    let home = TempDir::new().unwrap();
    let agents = home.path().join("proj/.aibus/agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        agents.join("greeter.md"),
        "---\nname: greeter\ndescription: Greets.\ninitial_prompt: say hello to the user\n---\nGreet.\n",
    )
    .unwrap();
    let script = home.path().join("s.ron");
    fs::write(
        &script,
        r#"[MatchPrompt(contains: "say hello to the user", then: [Text("matched"), Stop(EndTurn)]), Text("unmatched")]"#,
    )
    .unwrap();
    // No prompt on the command line: the definition's is sent.
    let out = command(home.path())
        .args(["run", "--json", "--agent", "greeter", "--script"])
        .arg(&script)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let lines = ndjson(&out);
    let done = lines.iter().find(|l| l["type"] == "RunFinished").unwrap();
    assert_eq!(done["summary"], "matched");

    // Without `--agent` it is not sent, and an empty prompt is still a usage error.
    let out = command(home.path()).args(["run", "--json", "--script"]).arg(&script).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[test]
fn a_prompt_on_the_command_line_follows_the_initial_prompt() {
    let home = TempDir::new().unwrap();
    let agents = home.path().join("proj/.aibus/agents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        agents.join("greeter.md"),
        "---\nname: greeter\ndescription: Greets.\ninitial_prompt: say hello to the user\n---\nGreet.\n",
    )
    .unwrap();
    let script = home.path().join("s.ron");
    fs::write(
        &script,
        r#"[MatchPrompt(contains: "say hello to the user\n\nand be brief", then: [Text("both"), Stop(EndTurn)]), Text("neither")]"#,
    )
    .unwrap();
    let out = command(home.path())
        .args(["run", "--json", "--agent", "greeter", "--script"])
        .arg(&script)
        .arg("and be brief")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let done = ndjson(&out).into_iter().find(|l| l["type"] == "RunFinished").unwrap();
    assert_eq!(done["summary"], "both");
}

#[test]
fn a_limit_in_the_environment_has_to_be_a_number() {
    let home = TempDir::new().unwrap();
    for var in ["AIBUS_MAX_SPAWN_DEPTH", "AIBUS_MAX_CONCURRENT_RUNS"] {
        let out = command(home.path())
            .env(var, "lots")
            .args(["run", "--agent", "explore", "hi"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{var}: {}", stderr(&out));
        assert!(stderr(&out).contains(var), "{}", stderr(&out));
    }
}

#[test]
fn a_depth_of_zero_takes_delegate_away_from_the_lead() {
    let home = TempDir::new().unwrap();
    let script = home.path().join("s.ron");
    fs::write(&script, r#"[Text("on my own")]"#).unwrap();
    let out = command(home.path())
        .env("AIBUS_MAX_SPAWN_DEPTH", "0")
        .args(["run", "--json", "--script"])
        .arg(&script)
        .arg("hi")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let done = ndjson(&out).into_iter().find(|l| l["type"] == "RunFinished").unwrap();
    assert_eq!(done["summary"], "on my own");
}
