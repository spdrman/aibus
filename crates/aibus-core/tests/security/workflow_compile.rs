//! #283: the workflow compiler against hostile input. A workflow file is written by a person, an
//! MCP client or a bundle from someone else, so the compiler gets the same suspicion the parser
//! does: no credential gets into a plan, no field can name a command or a path, nothing it does
//! runs a gate, nothing it is fed makes it panic, hang or overflow the stack, and what it refuses
//! it refuses before anything runs.

use std::time::{Duration, Instant};

use aibus_core::agent::discover::discover;
use aibus_core::workflow::compile::{compile, plan_hash};
use aibus_core::workflow::{preview, validate, Workflow};
use aibus_core::{AgentDef, AgentName, Redactor};
use indexmap::IndexMap;

fn agents() -> IndexMap<AgentName, AgentDef> {
    let dir = tempfile::tempdir().unwrap();
    discover(None, dir.path(), None, &[], &[]).unwrap().agents
}

fn roster(agents: &IndexMap<AgentName, AgentDef>) -> Vec<AgentName> {
    agents.keys().cloned().collect()
}

#[test]
fn compile_refuses_a_credential_anywhere_in_the_workflow() {
    let key = format!("sk-ant-{}", "a1".repeat(15));
    let agents = agents();
    for text in [
        format!("[[step]]\nagent = \"plan\"\nprompt = \"use {key} for {{{{input}}}}\"\n"),
        format!("description = \"{key}\"\n[[step]]\nagent = \"plan\"\nprompt = \"p\"\n"),
        format!("[[step]]\nagent = \"plan\"\nprompt = \"p\"\ngate = \"curl -H 'x: {key}' x\"\n"),
    ] {
        let wf = Workflow::parse("t", &text).unwrap();
        let refused = compile(&wf, &text, &agents, &Redactor::new()).unwrap_err();
        let said: Vec<&str> = refused.iter().map(|p| p.message.as_str()).collect();
        assert!(
            said.iter().any(|m| m.contains("looks like a credential")),
            "{said:?}"
        );
        // And the refusal does not repeat it.
        assert!(!said.iter().any(|m| m.contains(&key)), "{said:?}");
    }
}

#[test]
fn a_huge_long_or_deeply_nested_workflow_is_refused_without_a_panic() {
    let agents = agents();
    // More steps than a plan holds: a plain chain still reads (it always did), and gets no plan.
    let mut text = String::new();
    for i in 0..300 {
        text.push_str(&format!("[[step]]\nagent = \"plan\"\nprompt = \"p{i}\"\n"));
    }
    let wf = Workflow::parse("t", &text).unwrap();
    let refused = compile(&wf, &text, &agents, &Redactor::new()).unwrap_err();
    assert!(
        refused[0].message.contains("at most 256 steps"),
        "{refused:?}"
    );
    // With any of the new fields, so many steps are refused when the file is read.
    let with_ids = text.replace("[[step]]\n", "[[step]]\nrequires = [\"repo_read\"]\n");
    let err = Workflow::parse("t", &with_ids).unwrap_err().to_string();
    assert!(err.contains("has at most 256"), "{err}");

    // Nesting deep enough to overflow a recursive parser's stack is an error, not a crash.
    for open in ["[", "{a=", "[[", "a.", "x = ["] {
        let deep = open.repeat(100_000);
        let t0 = Instant::now();
        assert!(Workflow::parse("t", &deep).is_err());
        assert!(!validate(&deep, "t", &roster(&agents)).ok);
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "{open}: {:?}",
            t0.elapsed()
        );
    }

    // A route condition past its limit, or with a thousand `!`.
    let base = "[[step]]\nid = \"a\"\nagent = \"plan\"\nprompt = \"p\"\n\n[[step]]\nid = \"b\"\nagent = \"general\"\nprompt = \"q\"\n";
    for when in [
        "a.completed && ".repeat(40) + "a.completed",
        "!".repeat(1000) + "a.completed",
    ] {
        let text = format!("{base}\n[[route]]\nfrom = \"a\"\nto = \"b\"\nwhen = \"{when}\"\n");
        let err = Workflow::parse("t", &text).unwrap_err().to_string();
        assert!(err.contains("when:") && err.contains("256"), "{err}");
    }

    // A thousand routes and a long chain of loops neither hang nor overflow anything.
    let mut text = String::new();
    for i in 0..200 {
        text.push_str(&format!(
            "[[step]]\nid = \"s{i}\"\nagent = \"plan\"\nprompt = \"p\"\n"
        ));
    }
    for i in 0..199 {
        text.push_str(&format!(
            "[[route]]\nfrom = \"s{i}\"\nto = \"s{}\"\n",
            i + 1
        ));
        text.push_str(&format!("[[route]]\nfrom = \"s{}\"\nto = \"s{i}\"\nwhen = \"s{}.failed\"\nmax_iterations = 10\n", i + 1, i + 1));
    }
    let t0 = Instant::now();
    let wf = Workflow::parse("t", &text).unwrap();
    let plan = compile(&wf, &text, &agents, &Redactor::new()).unwrap();
    assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", t0.elapsed());
    // 200 steps once, and each loop back to s(i) may run s(i) to s199 again ten times.
    let loops: u64 = (0..199u64).map(|i| 10 * (200 - i)).sum();
    assert_eq!(plan["envelope"]["worst_case_agent_runs"], 200 + loops);
    let _ = preview(&wf, "x");
}

#[test]
fn compiling_planning_and_previewing_never_run_a_gate() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("pwned");
    let text = format!(
        "[[step]]\nagent = \"plan\"\nprompt = \"p {{{{input}}}}\"\ngate = \"touch {}\"\n\n[[step]]\nid = \"b\"\nagent = \"general\"\nprompt = \"q\"\ngate = \"touch {}\"\n\n[[route]]\nfrom = \"plan\"\nto = \"b\"\n",
        marker.display(),
        marker.display()
    );
    let wf = Workflow::parse("t", &text).unwrap();
    let plan = compile(&wf, &text, &agents(), &Redactor::new()).unwrap();
    let _ = preview(&wf, "x");
    let _ = aibus_core::workflow::native::preview(&wf, &agents(), &dir.path().join("agents"));
    assert!(!marker.exists(), "a gate ran");
    // The gate is in the plan as text, and a step with one is behind the shell approval when the
    // workflow asks for that.
    assert!(plan["steps"][0]["gate"]
        .as_str()
        .unwrap()
        .starts_with("touch "));
}

#[test]
fn the_new_fields_cannot_name_a_command_a_path_or_a_vendor_flag() {
    let agents = agents();
    for (field, value) in [
        ("runtime", "\"sh -c 'touch pwned'\""),
        ("runtime", "\"/bin/sh\""),
        ("mode", "\"--dangerously-skip-permissions\""),
        ("isolation", "\"../../etc\""),
        ("output", "\"/etc/passwd\""),
        ("requires", "[\"bypassPermissions\"]"),
        ("id", "\"../x\""),
        ("id", "\"-rf\""),
    ] {
        let text = format!("[[step]]\nagent = \"plan\"\nprompt = \"p\"\n{field} = {value}\n");
        assert!(
            Workflow::parse("t", &text).is_err(),
            "{field} = {value} was accepted"
        );
    }
    // A parent that is not a step's key goes nowhere.
    let text = "[[step]]\nagent = \"plan\"\nprompt = \"p\"\n\n[[step]]\nagent = \"general\"\nparent = \"../../plan\"\nprompt = \"q\"\n";
    assert!(Workflow::parse("t", text).is_err());
    // Whatever the prompt says, the argv of a planned process is the fixed flag set, the
    // placeholders and nothing the workflow wrote.
    let text = "[[step]]\nid = \"lead\"\nagent = \"general\"\nruntime = \"cli.claude\"\nprompt = \"\\\"; rm -rf ~; --dangerously-skip-permissions {{input}}\"\n";
    let wf = Workflow::parse("t", text).unwrap();
    let plan = compile(&wf, text, &agents, &Redactor::new()).unwrap();
    for arg in plan["steps"][0]["invocation"]["argv"].as_array().unwrap() {
        let arg = arg.as_str().unwrap();
        assert!(
            !arg.contains("rm -rf") && !arg.contains("dangerously"),
            "{arg}"
        );
    }
}

/// A tiny xorshift so the run is the same on every host and needs no crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        from[self.below(from.len())]
    }
}

/// A random workflow in the new format: steps with random fields, random routes and conditions.
/// Most are invalid in some way; some are not.
fn random_workflow(rng: &mut Rng) -> String {
    let agents = [
        "plan", "general", "explore", "search", "audit", "ghost", "lead",
    ];
    let keys = ["a", "b", "c", "d", "plan", "general", "x"];
    let mut out = String::new();
    if rng.below(4) == 0 {
        out.push_str(rng.pick(&[
            "schema_version = \"1\"\n",
            "schema_version = \"9\"\n",
            "revision = \"1.0\"\n",
        ]));
    }
    let steps = 1 + rng.below(6);
    for i in 0..steps {
        out.push_str("[[step]]\n");
        if rng.below(3) > 0 {
            out.push_str(&format!("id = \"{}\"\n", keys[i % keys.len()]));
        }
        out.push_str(&format!("agent = \"{}\"\n", rng.pick(&agents)));
        let prompt = rng.pick(&[
            "p",
            "{{input}}",
            "x {{prev.summary}}",
            "{{steps.a.summary}} y",
            "{{steps.plan.summary}}",
            "{{",
            "z",
        ]);
        out.push_str(&format!("prompt = \"{prompt}\"\n"));
        for (chance, line) in [
            (5, "runtime = \"cli.claude\"\n"),
            (6, "mode = \"native\"\n"),
            (6, "mode = \"native_or_managed\"\n"),
            (5, "parent = \"a\"\n"),
            (4, "requires = [\"repo_read\"]\n"),
            (8, "requires = [\"shell\", \"repo_write\"]\n"),
            (8, "isolation = \"git_worktree\"\n"),
            (6, "output = \"json\"\n"),
            (8, "gate = \"true\"\n"),
            (8, "on_fail = \"retry:2\"\n"),
        ] {
            if rng.below(chance) == 0 {
                out.push_str(line);
            }
        }
    }
    for _ in 0..rng.below(5) {
        let (from, to) = (rng.pick(&keys), rng.pick(&keys));
        out.push_str(&format!("[[route]]\nfrom = \"{from}\"\nto = \"{to}\"\n"));
        if rng.below(2) == 0 {
            let atom = |rng: &mut Rng| {
                format!(
                    "{}{}.{}",
                    rng.pick(&["", "!"]),
                    rng.pick(&keys),
                    rng.pick(&["completed", "failed", "artifact_valid", "nope"])
                )
            };
            let mut when = atom(rng);
            for _ in 0..rng.below(3) {
                when.push_str(rng.pick(&[" && ", " || ", " & ", ""]));
                when.push_str(&atom(rng));
            }
            out.push_str(&format!("when = \"{when}\"\n"));
        }
        if rng.below(3) == 0 {
            out.push_str(&format!("max_iterations = {}\n", rng.below(12)));
        }
    }
    if rng.below(4) == 0 {
        out.push_str(&format!(
            "[limits]\nmax_agent_runs = {}\nmax_parallel = {}\n",
            rng.below(20),
            rng.below(5)
        ));
    }
    if rng.below(4) == 0 {
        out.push_str("[approvals]\nrequired_for = [\"shell\", \"git_merge\"]\n");
    }
    out
}

/// `text` with a few random byte-level edits, cut back to valid UTF-8.
fn mutate(rng: &mut Rng, text: &str) -> String {
    let mut bytes = text.as_bytes().to_vec();
    for _ in 0..1 + rng.below(4) {
        if bytes.is_empty() {
            break;
        }
        let at = rng.below(bytes.len());
        match rng.below(4) {
            0 => {
                bytes[at] = {
                    let set = b"[]{}=\"\n.#!&|";
                    set[rng.below(set.len())]
                }
            }
            1 => {
                bytes.remove(at);
            }
            2 => bytes.insert(at, rng.next() as u8),
            _ => {
                let end = (at + rng.below(40)).min(bytes.len());
                let copy = bytes[at..end].to_vec();
                bytes.splice(at..at, copy);
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn thirty_thousand_random_workflows_never_panic_the_compiler() {
    let agents = agents();
    let roster = roster(&agents);
    let redactor = Redactor::new();
    let mut seeds: Vec<String> = Vec::new();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for dir in [
        "examples/workflows",
        "fixtures/workflows/graph",
        "fixtures/workflows/graph-invalid",
        "fixtures/workflows/valid",
    ] {
        for e in std::fs::read_dir(root.join(dir)).unwrap() {
            seeds.push(std::fs::read_to_string(e.unwrap().path()).unwrap());
        }
    }
    let mut rng = Rng(0x2830_5eed_0000_0283);
    let (mut planned, mut refused) = (0u32, 0u32);
    for i in 0..30_000 {
        let text = if i % 3 == 0 {
            random_workflow(&mut rng)
        } else {
            let seed = &seeds[rng.below(seeds.len())];
            mutate(&mut rng, seed)
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let v = validate(&text, "t", &roster);
            let Ok(wf) = Workflow::parse("t", &text) else {
                assert!(!v.ok, "parse refused what validate passed:\n{text}");
                return None;
            };
            let _ = preview(&wf, "input");
            let _ = wf.plan_only();
            let plan = compile(&wf, &text, &agents, &redactor).ok()?;
            // Whatever compiles compiles the same way twice, and its hash matches its bytes.
            let again = compile(&wf, &text, &agents, &redactor).unwrap();
            assert_eq!(plan, again, "{text}");
            assert_eq!(
                plan["plan_hash"].as_str().unwrap(),
                plan_hash(&plan),
                "{text}"
            );
            Some(())
        }));
        match outcome {
            Ok(Some(())) => planned += 1,
            Ok(None) => refused += 1,
            Err(_) => panic!("input {i} panicked the compiler:\n{text}"),
        }
    }
    // The generator reaches both sides, or this test would prove little.
    eprintln!("30000 inputs: {planned} planned, {refused} refused, none panicked");
    assert!(
        planned > 500 && refused > 1_000,
        "planned {planned}, refused {refused}"
    );
}
