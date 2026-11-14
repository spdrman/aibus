//! The bodies of the four fuzz targets (10 section 1). `fuzz/fuzz_targets/*.rs` call these with
//! the bytes libFuzzer made up, and `crates/aibus-core/tests/security/fuzz.rs` calls the same
//! functions with every committed corpus file and every crash fixture, so a crash found by a
//! nightly run is a plain test on the next push. A check panics when an invariant breaks; it
//! returns for any input the code under test merely rejects.

use std::sync::OnceLock;

use base64::Engine as _;

use aibus_core::agent::def::parse;
use aibus_core::continuity::handoff::Handoff;
use aibus_core::tools::{normalise_command, spec_matches, split, ShellDeny};
use aibus_core::{AccountName, AgentName, DefSource, Redactor, Scope, ToolPattern};

fn utf8(data: &[u8]) -> Option<&str> {
    std::str::from_utf8(data).ok()
}

/// `agent::def::parse`: never panics, and a definition it accepts has a name that parses again.
pub fn agent_def(data: &[u8]) {
    let Some(text) = utf8(data) else { return };
    let source = DefSource { path: None, scope: Scope::Project };
    if let Ok((def, _warnings)) = parse(text, source) {
        assert_eq!(def.name.as_str().parse::<AgentName>().ok().as_ref(), Some(&def.name));
    }
}

/// Handoff YAML: never panics, and a handoff it accepts survives `to_yaml` and a second parse
/// with its goal and now intact.
pub fn handoff(data: &[u8]) {
    let Some(text) = utf8(data) else { return };
    if let Ok(first) = Handoff::parse(text) {
        let again = Handoff::parse(&first.to_yaml()).expect("a written handoff reads back");
        assert_eq!((again.goal(), again.now()), (first.goal(), first.now()));
    }
}

/// `Redactor`: the first byte says how much of the rest is a "credential" (made of hex, so it
/// cannot look like the redactor's own tags) and the remainder is the text around it. Neither
/// the registered credential nor its base64 form may survive, and neither may a token in the
/// shape of any built-in pattern that is built from the same bytes.
pub fn redactor(data: &[u8]) {
    static REDACTOR: OnceLock<Redactor> = OnceLock::new();
    let Some((&n, rest)) = data.split_first() else { return };
    let (secret_bytes, around) = rest.split_at(rest.len().min(usize::from(n % 33)));
    let around = String::from_utf8_lossy(around);
    let hex: String = secret_bytes.iter().map(|b| format!("{b:02x}")).collect();
    let secret = format!("FZ{hex}");

    let r = REDACTOR.get_or_init(Redactor::new);
    let account = AccountName("fuzz".into());
    // Shared across calls on purpose, so register and replace the one entry each time.
    r.register(&account, &secret);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&secret);
    let out = r.redact(&format!("{around} {secret} {around} {b64} {around}")).into_owned();
    r.unregister(&account);
    if secret.len() >= 6 {
        assert!(!out.contains(&secret), "the credential survived: {out}");
        assert!(!out.contains(&b64), "its base64 survived: {out}");
    }

    // Built-in shapes made of the same bytes: all of these are keys and none may survive.
    let pad: String = hex.chars().chain("0123456789abcdef0123456789abcdef0123456789".chars()).collect();
    let upper = pad.to_ascii_uppercase();
    let tokens = [
        format!("sk-ant-{pad}"),
        format!("sk-{}", &pad[..24]),
        format!("sk-proj-{pad}"),
        format!("ghp_{}", &pad[..36]),
        format!("AKIA{}", &upper[..16]),
        format!("Bearer {pad}"),
        format!("AIza{}", &pad[..35]),
        format!("eyJ{pad}.eyJ{pad}.{pad}"),
    ];
    // The text around can hold a copy of a token glued to a word character, which the patterns
    // leave alone on purpose (`\b`), so the check is that the copy added here is gone: there is
    // one occurrence fewer afterwards.
    for token in tokens {
        let text = format!("{around} {token} {around}");
        let out = r.redact(&text).into_owned();
        assert!(out.matches(&token).count() < text.matches(&token).count(), "{token} survived in {out}");
    }
}

/// The tool-pattern matcher: `ToolPattern` text round-trips, the specifier glob and the command
/// splitter never panic, and what the splitter returns is already normalised.
pub fn tool_pattern(data: &[u8]) {
    static DENY: OnceLock<ShellDeny> = OnceLock::new();
    let Some(text) = utf8(data) else { return };
    if let Ok(p) = text.parse::<ToolPattern>() {
        let shown = p.to_string();
        assert_eq!(shown.parse::<ToolPattern>().as_ref(), Ok(&p), "{shown}");
        if let Some(spec) = &p.spec {
            let _ = spec_matches(spec, text);
            let _ = spec_matches(text, spec);
        }
    }
    let norm = normalise_command(text);
    assert_eq!(normalise_command(&norm), norm, "normalising twice changed {norm:?}");
    if let Ok(parts) = split(text) {
        for part in parts {
            assert_eq!(normalise_command(&part), part);
            let _ = spec_matches("git *", &part);
        }
    }
    let deny = DENY.get_or_init(|| ShellDeny::new(&[]).expect("the built-in list compiles"));
    let _ = deny.hit(text);
}
