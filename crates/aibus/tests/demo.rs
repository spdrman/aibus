//! The README demo (`aibus demo`, hidden): two plays draw the same frames, nothing personal
//! shows in them, and the committed GIF is about ten seconds long.

mod common;

use aibus_testkit::xdg_temp;
use common::{aibus, stderr};

fn frames(home: &std::path::Path) -> String {
    let out = aibus(home, &["demo", "--text"]);
    assert!(out.status.success(), "{}", stderr(&out));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn two_plays_give_the_same_text_frames() {
    let (a, b) = (xdg_temp(), xdg_temp());
    let (fa, fb) = (frames(a.path()), frames(b.path()));
    assert_eq!(fa.matches("\n=== t=").count() + 1, 100);
    // Different homes, same frames: the demo reads nothing from the machine.
    assert_eq!(fa, fb);
}

#[test]
fn the_ansi_stream_marks_every_frame_with_its_virtual_time() {
    let home = xdg_temp();
    let out = aibus(home.path(), &["demo"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let s = String::from_utf8(out.stdout).unwrap();
    assert_eq!(s.matches("\x1b]7777;").count(), 100);
    assert!(s.contains("\x1b]7777;9900\x07"));
}

#[test]
fn the_demo_output_holds_nothing_personal() {
    let home = xdg_temp();
    let mut all = frames(home.path());
    all.push_str(&String::from_utf8(aibus(home.path(), &["demo"]).stdout).unwrap());
    // The recording and its script are scanned too: they are what the frames come from.
    for f in ["msgs.jsonl", "script.ron"] {
        all.push_str(
            &std::fs::read_to_string(format!(
                "{}/../../docs/media/demo/{f}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap(),
        );
    }
    let home_dir = home.path().to_string_lossy().into_owned();
    let mut banned = vec![home_dir.as_str(), "/home/", "/Users/", "/root", "C:\\", "@"];
    let host = std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    if let Some(h) = host.as_deref().filter(|h| h.len() > 3) {
        banned.push(h);
    }
    if let Ok(u) = std::env::var("USER") {
        if u.len() > 3 {
            banned.push(Box::leak(u.into_boxed_str()));
        }
    }
    for b in banned {
        assert!(!all.contains(b), "the demo output contains {b:?}");
    }
    let keys = regex::Regex::new(
        r"sk-[A-Za-z0-9_-]{10,}|AKIA[0-9A-Z]{12,}|gh[pous]_[A-Za-z0-9]{20,}|[A-Za-z0-9+/_-]{40,}",
    )
    .unwrap();
    assert!(
        keys.find(&all).is_none(),
        "key-shaped string: {:?}",
        keys.find(&all)
    );
    // The only account in the demo is the built-in fake; no real provider is named.
    for real in ["anthropic", "openai", "kimi", "moonshot", "claude", "gpt-"] {
        assert!(!all.to_lowercase().contains(real), "{real}");
    }
}

/// Hundredths of a second per frame, read by walking the GIF blocks.
fn gif_delays(b: &[u8]) -> Vec<u32> {
    assert!(b.starts_with(b"GIF8"));
    let mut i = 13;
    if b[10] & 0x80 != 0 {
        i += 3 * (2usize << (b[10] & 7));
    }
    let (mut out, mut pending) = (Vec::new(), 0);
    while b[i] != 0x3B {
        match b[i] {
            0x21 => {
                if b[i + 1] == 0xF9 {
                    pending = u32::from(b[i + 4]) | u32::from(b[i + 5]) << 8;
                }
                i += 2;
                while b[i] != 0 {
                    i += usize::from(b[i]) + 1;
                }
                i += 1;
            }
            0x2C => {
                let flags = b[i + 9];
                i += 10;
                if flags & 0x80 != 0 {
                    i += 3 * (2usize << (flags & 7));
                }
                i += 1;
                while b[i] != 0 {
                    i += usize::from(b[i]) + 1;
                }
                i += 1;
                out.push(std::mem::take(&mut pending));
            }
            t => panic!("bad GIF block {t:#x}"),
        }
    }
    out
}

#[test]
fn the_committed_gif_is_ten_seconds_and_small_and_the_readme_embeds_it() {
    let root = format!("{}/../..", env!("CARGO_MANIFEST_DIR"));
    let gif = std::fs::read(format!("{root}/docs/media/aibus-ui.gif")).unwrap();
    let total = gif_delays(&gif).iter().sum::<u32>();
    assert!((980..=1020).contains(&total), "{total} cs");
    assert!(gif.len() < 5_000_000, "{} bytes", gif.len());
    let readme = std::fs::read_to_string(format!("{root}/README.md")).unwrap();
    assert!(readme.contains("![") && readme.contains("docs/media/aibus-ui.gif"));
}
