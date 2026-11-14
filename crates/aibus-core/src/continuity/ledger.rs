//! The ledger (01 section 8.1): `thoughts/ledgers/CONTINUITY_<topic>.md`, Markdown with the
//! sections `Goal`, `Completed`, `In Progress`, `Blockers` and `Decisions`, and an `Updated:`
//! line. The lead keeps it with `ledger_update`; people edit it by hand just as well, so a
//! section the parser does not know is kept as it is.

use std::fs;
use std::io;
use std::path::Path;

use super::{files_with, Layout};
use crate::persist::fs::{read_locked, update_locked};

pub const SECTIONS: [&str; 5] = ["Goal", "Completed", "In Progress", "Blockers", "Decisions"];

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ledger {
    pub topic: String,
    /// In file order. The five known sections are always there.
    pub sections: Vec<(String, String)>,
    pub updated: String,
}

/// The section a name means: case is ignored and `_`, `-` and a space are the same, so
/// `in_progress` works as well as `In Progress`.
pub fn canonical(name: &str) -> Result<&'static str, String> {
    let flat = name.trim().to_ascii_lowercase().replace(['_', '-'], " ");
    SECTIONS
        .iter()
        .find(|s| s.to_ascii_lowercase() == flat)
        .copied()
        .ok_or_else(|| format!("section {name:?}: want one of {}", SECTIONS.join(", ")))
}

/// A list line without its marker: `- [ ] write the test` is `write the test`.
fn item(line: &str) -> &str {
    let t = line.trim();
    let t = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .unwrap_or(t);
    let t = t
        .strip_prefix("[ ] ")
        .or_else(|| t.strip_prefix("[x] "))
        .unwrap_or(t);
    t.trim()
}

impl Ledger {
    pub fn new(topic: &str, date: &str) -> Self {
        Self {
            topic: topic.to_owned(),
            sections: SECTIONS
                .iter()
                .map(|s| ((*s).to_owned(), String::new()))
                .collect(),
            updated: date.to_owned(),
        }
    }

    /// Reads a ledger back. Text before the first heading is ignored (the title); a missing
    /// known section comes back empty.
    pub fn parse(topic: &str, text: &str) -> Self {
        fn close(cur: &mut Option<(String, Vec<&str>)>, out: &mut Ledger) {
            if let Some((name, lines)) = cur.take() {
                out.sections
                    .push((name, lines.join("\n").trim().to_owned()));
            }
        }
        let mut out = Self::new(topic, "");
        out.sections.clear();
        let mut current: Option<(String, Vec<&str>)> = None;
        for line in text.lines() {
            if let Some(name) = line.strip_prefix("## ") {
                close(&mut current, &mut out);
                current = Some((name.trim().to_owned(), Vec::new()));
            } else if let Some(date) = line.strip_prefix("Updated:") {
                close(&mut current, &mut out);
                out.updated = date.trim().to_owned();
            } else if let Some((_, lines)) = current.as_mut() {
                lines.push(line);
            }
        }
        close(&mut current, &mut out);
        for known in SECTIONS {
            if !out.sections.iter().any(|(n, _)| n == known) {
                out.sections.push((known.to_owned(), String::new()));
            }
        }
        out
    }

    pub fn render(&self) -> String {
        let mut out = format!("# Continuity: {}\n", self.topic);
        for (name, body) in &self.sections {
            out.push_str(&format!("\n## {name}\n"));
            if !body.is_empty() {
                out.push_str(body);
                out.push('\n');
            }
        }
        out.push_str(&format!("\nUpdated: {}\n", self.updated));
        out
    }

    pub fn section(&self, name: &str) -> &str {
        self.sections
            .iter()
            .find(|(n, _)| n == name)
            .map_or("", |(_, b)| b.as_str())
    }

    pub fn goal(&self) -> &str {
        self.section("Goal")
    }

    /// The items of a list section, markers removed.
    pub fn items(&self, name: &str) -> Vec<&str> {
        self.section(name)
            .lines()
            .map(item)
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// What the status bar shows next to the goal.
    pub fn in_progress(&self) -> Option<&str> {
        self.items("In Progress").into_iter().next()
    }

    /// Replaces a section's text, or adds to it with `append`. In every section but `Goal` each
    /// line becomes a list item, so "the first item" always means something.
    pub fn update(
        &mut self,
        section: &str,
        text: &str,
        append: bool,
        date: &str,
    ) -> Result<(), String> {
        let name = canonical(section)?;
        let body = if name == "Goal" {
            text.trim().to_owned()
        } else {
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| {
                    if l.starts_with("- ") || l.starts_with("* ") {
                        l.to_owned()
                    } else {
                        format!("- {l}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let slot = self
            .sections
            .iter_mut()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b)
            .ok_or("the section is missing")?;
        *slot = match (append, slot.is_empty()) {
            (true, false) if !body.is_empty() => format!("{slot}\n{body}"),
            _ => body,
        };
        self.updated = date.to_owned();
        Ok(())
    }
}

/// The topic of the most recently modified `CONTINUITY_<topic>.md` under `dir/ledgers`.
pub fn newest_topic(dir: &Path) -> Option<String> {
    files_with(&dir.join("ledgers"), "md")
        .into_iter()
        .filter_map(|p| {
            let name = p
                .file_stem()?
                .to_str()?
                .strip_prefix("CONTINUITY_")?
                .to_owned();
            Some((fs::metadata(&p).ok()?.modified().ok()?, name))
        })
        .max()
        .map(|(_, topic)| topic)
}

pub fn read(layout: &Layout) -> io::Result<Option<Ledger>> {
    Ok(read_locked(&layout.ledger())?.map(|t| Ledger::parse(&layout.topic, &t)))
}

/// Starts the topic's ledger. An existing one is an error, not overwritten.
pub fn create(layout: &Layout, date: &str) -> io::Result<Ledger> {
    let path = layout.ledger();
    fs::create_dir_all(layout.ledgers())?;
    let ledger = Ledger::new(&layout.topic, date);
    let text = ledger.render();
    let mut made = Ok(());
    update_locked(&path, |old| {
        if old.is_empty() {
            Ok(text.clone())
        } else {
            made = Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", path.display()),
            ));
            Ok(old.to_owned())
        }
    })?;
    made.map(|()| ledger)
}

/// `ledger_update`: changes one section under the lock, creating the ledger when the topic has
/// none yet, and returns the result.
pub fn update(
    layout: &Layout,
    section: &str,
    text: &str,
    append: bool,
    date: &str,
) -> io::Result<Ledger> {
    let path = layout.ledger();
    fs::create_dir_all(layout.ledgers())?;
    let mut result = None;
    update_locked(&path, |old| {
        let mut ledger = if old.trim().is_empty() {
            Ledger::new(&layout.topic, date)
        } else {
            Ledger::parse(&layout.topic, old)
        };
        ledger
            .update(section, text, append, date)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let rendered = ledger.render();
        result = Some(ledger);
        Ok(rendered)
    })?;
    result.ok_or_else(|| io::Error::other("the ledger was not updated"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn layout(dir: &Path) -> Layout {
        Layout::new(dir, &Config::default(), dir, Some("auth")).unwrap()
    }

    #[test]
    fn a_new_ledger_has_the_five_sections_and_the_date() {
        let l = Ledger::new("auth", "2026-11-08");
        let text = l.render();
        assert!(text.starts_with("# Continuity: auth\n"));
        for s in SECTIONS {
            assert!(text.contains(&format!("\n## {s}\n")), "{s}");
        }
        assert!(text.ends_with("\nUpdated: 2026-11-08\n"));
        assert_eq!(Ledger::parse("auth", &text), l);
    }

    #[test]
    fn section_names_are_forgiving() {
        assert_eq!(canonical("in_progress").unwrap(), "In Progress");
        assert_eq!(canonical("IN-PROGRESS").unwrap(), "In Progress");
        assert_eq!(canonical(" goal ").unwrap(), "Goal");
        assert!(canonical("notes").unwrap_err().contains("Goal, Completed"));
    }

    #[test]
    fn update_replaces_or_appends_and_lists_get_markers() {
        let mut l = Ledger::new("auth", "2026-11-07");
        l.update("goal", "Move auth to middleware", false, "2026-11-08")
            .unwrap();
        l.update(
            "in_progress",
            "write the test\n- run the migration",
            false,
            "2026-11-08",
        )
        .unwrap();
        l.update("In Progress", "ship it", true, "2026-11-09")
            .unwrap();
        assert_eq!(l.goal(), "Move auth to middleware");
        assert_eq!(
            l.section("In Progress"),
            "- write the test\n- run the migration\n- ship it"
        );
        assert_eq!(l.in_progress(), Some("write the test"));
        assert_eq!(l.updated, "2026-11-09");
        l.update("in progress", "only this", false, "2026-11-09")
            .unwrap();
        assert_eq!(l.items("In Progress"), ["only this"]);
    }

    #[test]
    fn a_section_a_person_added_survives_a_round_trip() {
        let text = "# Continuity: auth\n\n## Goal\nG\n\n## Notes\nkeep me\n\n## In Progress\n- [ ] one\n\nUpdated: 2026-11-01\n";
        let l = Ledger::parse("auth", text);
        assert_eq!(l.section("Notes"), "keep me");
        assert_eq!(l.in_progress(), Some("one"));
        assert_eq!(l.updated, "2026-11-01");
        let again = Ledger::parse("auth", &l.render());
        assert_eq!(again.section("Notes"), "keep me");
        assert_eq!(again.section("Completed"), "");
    }

    #[test]
    fn files_are_created_and_updated_under_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let l = layout(dir.path());
        assert!(read(&l).unwrap().is_none());
        update(&l, "goal", "G", false, "2026-11-08").unwrap();
        let got = update(&l, "blockers", "waiting on review", true, "2026-11-08").unwrap();
        assert_eq!(got.items("Blockers"), ["waiting on review"]);
        assert_eq!(read(&l).unwrap().unwrap(), got);
        assert_eq!(
            create(&l, "2026-11-08").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(update(&l, "nope", "x", false, "d").is_err());
        // The failed update left the file as it was.
        assert_eq!(read(&l).unwrap().unwrap(), got);
    }

    #[test]
    fn two_threads_updating_do_not_lose_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let l = layout(dir.path());
        let handles: Vec<_> = (0..2)
            .map(|t| {
                let l = l.clone();
                std::thread::spawn(move || {
                    for i in 0..10 {
                        update(&l, "completed", &format!("t{t} item {i}"), true, "d").unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(read(&l).unwrap().unwrap().items("Completed").len(), 20);
    }
}
