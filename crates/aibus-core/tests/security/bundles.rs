//! #271: a `.aibus` bundle is input from someone else. Every hostile shape here is refused whole by
//! the in-memory reader and by `install`, with no file written, and a run over random and mutated
//! tar bytes never panics and never writes unless the bundle is a valid one.

use std::fs;
use std::path::Path;

use aibus_core::workflow::bundle::{self, BundleError, Install};
use aibus_core::workflow::EXAMPLES;
use aibus_core::{AgentName, Redactor};

const REVIEW: &str = EXAMPLES[2].1;
const SMALL: &str = "[[step]]\nagent = \"explore\"\nprompt = \"{{input}}\"\n";

/// One raw ustar header. `size_field` overrides the octal size (for the tar-bomb shapes).
fn header(path: &[u8], kind: u8, size: u64, link: &str, size_field: Option<[u8; 12]>) -> [u8; 512] {
    let mut h = [0u8; 512];
    let (prefix, name): (&[u8], &[u8]) = if path.len() > 100 {
        let cut = path.iter().rposition(|b| *b == b'/').unwrap();
        (&path[..cut], &path[cut + 1..])
    } else {
        (b"", path)
    };
    h[..name.len()].copy_from_slice(name);
    h[100..108].copy_from_slice(b"0000644\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    let size = size_field.unwrap_or_else(|| {
        let mut f = [0u8; 12];
        f[..11].copy_from_slice(format!("{size:011o}").as_bytes());
        f
    });
    h[124..136].copy_from_slice(&size);
    h[136..148].copy_from_slice(b"00000000000\0");
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix);
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

/// A header and its data, padded to the block.
fn entry(path: &str, kind: u8, data: &[u8]) -> Vec<u8> {
    raw_entry(path.as_bytes(), kind, data, "")
}

fn raw_entry(path: &[u8], kind: u8, data: &[u8], link: &str) -> Vec<u8> {
    let mut out = header(path, kind, data.len() as u64, link, None).to_vec();
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
    out
}

fn manifest(workflows: &[&str]) -> Vec<u8> {
    let list: Vec<String> = workflows.iter().map(|w| format!("\"{w}\"")).collect();
    format!(
        "format = 1\nname = \"kit\"\nworkflows = [{}]\n",
        list.join(", ")
    )
    .into_bytes()
}

/// The entries, then the two zero blocks.
fn tar(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut out: Vec<u8> = entries.concat();
    out.extend_from_slice(&[0u8; 1024]);
    out
}

/// A valid bundle with `review`, built by hand, so the hostile ones differ from it in one thing.
fn good() -> Vec<u8> {
    tar(&[
        entry("manifest.toml", b'0', &manifest(&["review"])),
        entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
    ])
}

/// A bundle with the valid manifest and `review`, plus `extra`.
fn with(extra: Vec<u8>) -> Vec<u8> {
    tar(&[
        entry("manifest.toml", b'0', &manifest(&["review"])),
        entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
        extra,
    ])
}

fn roster() -> Vec<AgentName> {
    ["explore", "plan", "general"]
        .iter()
        .map(|a| AgentName::new(a).unwrap())
        .collect()
}

/// `read` refuses it, and `install` into an empty place writes nothing at all.
/// One redactor for every check here: building one compiles its patterns.
fn redactor() -> &'static Redactor {
    static R: std::sync::OnceLock<Redactor> = std::sync::OnceLock::new();
    R.get_or_init(Redactor::new)
}

fn refused(bytes: &[u8]) -> BundleError {
    let err = bundle::read(bytes, redactor()).expect_err("a hostile bundle is refused");
    let root = tempfile::tempdir().unwrap();
    let roster = roster();
    let to = Install {
        workflows_dir: root.path().join("w/workflows"),
        agents_dir: root.path().join("w/agents"),
        roster: &roster,
        force: true,
        allow_gates: true,
    };
    assert!(
        bundle::install(bytes, &to, redactor()).is_err(),
        "install refuses it too"
    );
    assert!(
        !root.path().join("w").exists(),
        "nothing was written for {err}"
    );
    err
}

#[test]
fn the_hand_built_good_bundle_reads() {
    let b = bundle::read(&good(), &Redactor::new()).unwrap();
    assert_eq!(b.workflows, [("review".to_owned(), REVIEW.to_owned())]);
}

#[test]
fn a_bundle_cannot_climb_out_with_dot_dot_or_an_absolute_path() {
    for path in [
        "workflows/../../evil.toml",
        "../evil.toml",
        "workflows/../manifest.toml",
        "a/../../b",
    ] {
        assert!(
            matches!(
                refused(&with(entry(path, b'0', SMALL.as_bytes()))),
                BundleError::Traversal(_)
            ),
            "{path}"
        );
    }
    for path in ["/etc/evil.toml", "/workflows/x.toml"] {
        assert!(
            matches!(
                refused(&with(entry(path, b'0', SMALL.as_bytes()))),
                BundleError::Absolute(_)
            ),
            "{path}"
        );
    }
    // The ustar prefix field is part of the path too.
    let long = format!("../../{}/workflows/x.toml", "d".repeat(90));
    assert!(matches!(
        refused(&with(raw_entry(
            long.as_bytes(),
            b'0',
            SMALL.as_bytes(),
            ""
        ))),
        BundleError::Traversal(_)
    ));
}

#[test]
fn a_bundle_cannot_carry_a_symlink_or_a_hard_link() {
    let sym = raw_entry(b"workflows/x.toml", b'2', b"", "/etc/passwd");
    assert!(matches!(refused(&with(sym)), BundleError::Link(_)));
    let hard = raw_entry(b"workflows/x.toml", b'1', b"", "workflows/review.toml");
    assert!(matches!(refused(&with(hard)), BundleError::Link(_)));
}

#[test]
fn a_bundle_cannot_carry_a_device_a_fifo_or_a_special_entry() {
    for kind in [b'3', b'4', b'6', b'7'] {
        assert!(
            matches!(
                refused(&with(entry("workflows/x.toml", kind, b""))),
                BundleError::Special(_)
            ),
            "type {}",
            kind as char
        );
    }
    // pax and GNU extension headers are not part of the format either.
    for kind in [b'x', b'g', b'L', b'K', b'S', b'V', b'M', b'Z'] {
        assert!(
            matches!(
                refused(&with(entry("workflows/x.toml", kind, b"x"))),
                BundleError::BadHeader(_)
            ),
            "type {}",
            kind as char
        );
    }
}

#[test]
fn an_entry_over_64_kib_is_refused_before_it_is_read() {
    let big = vec![b'#'; bundle::MAX_FILE_BYTES + 1];
    assert!(matches!(
        refused(&with(entry("workflows/big.toml", b'0', &big))),
        BundleError::EntryTooBig(_, _)
    ));
}

#[test]
fn a_bundle_over_the_total_cap_is_refused() {
    let names: Vec<String> = (0..20).map(|i| format!("w{i}")).collect();
    let body = format!("# {}\n{SMALL}", "x".repeat(60 * 1024));
    let mut entries = vec![entry(
        "manifest.toml",
        b'0',
        &manifest(&names.iter().map(String::as_str).collect::<Vec<_>>()),
    )];
    for n in &names {
        entries.push(entry(&format!("workflows/{n}.toml"), b'0', body.as_bytes()));
    }
    let bytes = tar(&entries);
    assert!(bytes.len() > bundle::MAX_BUNDLE_BYTES);
    assert!(matches!(refused(&bytes), BundleError::TooBig(_)));
}

#[test]
fn a_bundle_with_too_many_files_is_refused() {
    let names: Vec<String> = (0..bundle::MAX_ENTRIES).map(|i| format!("w{i}")).collect();
    let mut entries = vec![entry(
        "manifest.toml",
        b'0',
        &manifest(&names.iter().map(String::as_str).collect::<Vec<_>>()),
    )];
    for n in &names {
        entries.push(entry(
            &format!("workflows/{n}.toml"),
            b'0',
            SMALL.as_bytes(),
        ));
    }
    assert!(matches!(
        refused(&tar(&entries)),
        BundleError::TooManyEntries
    ));
}

#[test]
fn names_outside_the_name_rule_are_refused() {
    for name in ["Bad", "-x", "a.b", "a b", "_x", &"a".repeat(65)] {
        let bytes = tar(&[
            entry("manifest.toml", b'0', &manifest(&[name])),
            entry(&format!("workflows/{name}.toml"), b'0', SMALL.as_bytes()),
        ]);
        assert!(
            matches!(
                refused(&bytes),
                BundleError::BadName(_) | BundleError::Outside(_)
            ),
            "{name}"
        );
    }
    let bad_kit = tar(&[
        entry(
            "manifest.toml",
            b'0',
            b"format = 1\nname = \"Bad Kit\"\nworkflows = [\"review\"]\n",
        ),
        entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
    ]);
    assert!(matches!(refused(&bad_kit), BundleError::BadName(_)));
    assert!(matches!(
        refused(&with(entry("agents/UP.md", b'0', b"x"))),
        BundleError::BadName(_) | BundleError::Outside(_)
    ));
}

#[test]
fn anything_outside_the_layout_is_refused() {
    for path in [
        "notes.txt",
        "workflows/sub/x.toml",
        "workflows/x.yaml",
        "agents/x.toml",
        "./manifest.toml",
        "workflows",
        "manifest.toml/x",
        ".aibus/workflows/x.toml",
        "README.txt",
    ] {
        let err = refused(&with(entry(path, b'0', SMALL.as_bytes())));
        assert!(
            matches!(err, BundleError::Outside(_) | BundleError::Duplicate(_)),
            "{path}: {err:?}"
        );
    }
    // A directory entry is only ever `workflows/` or `agents/`.
    assert!(matches!(
        refused(&with(entry("etc/", b'5', b""))),
        BundleError::Outside(_)
    ));
}

#[test]
fn duplicate_entries_and_duplicate_names_are_refused() {
    assert!(matches!(
        refused(&with(entry(
            "workflows/review.toml",
            b'0',
            REVIEW.as_bytes()
        ))),
        BundleError::Duplicate(_)
    ));
    assert!(matches!(
        refused(&with(entry("manifest.toml", b'0', &manifest(&["review"])))),
        BundleError::Duplicate(_)
    ));
    let twice = tar(&[
        entry("manifest.toml", b'0', &manifest(&["review", "review"])),
        entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
    ]);
    assert!(matches!(
        refused(&twice),
        BundleError::Duplicate(_) | BundleError::Manifest(_)
    ));
    // README and README.md are the same thing.
    let readmes = tar(&[
        entry("manifest.toml", b'0', &manifest(&["review"])),
        entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
        entry("README", b'0', b"a"),
        entry("README.md", b'0', b"b"),
    ]);
    assert!(matches!(refused(&readmes), BundleError::Duplicate(_)));
}

#[test]
fn a_manifest_that_does_not_match_the_contents_is_refused() {
    let missing = tar(&[entry("workflows/review.toml", b'0', REVIEW.as_bytes())]);
    assert!(matches!(refused(&missing), BundleError::Manifest(_)));
    let unlisted = tar(&[
        entry("manifest.toml", b'0', &manifest(&["review"])),
        entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
        entry("workflows/extra.toml", b'0', SMALL.as_bytes()),
    ]);
    assert!(matches!(refused(&unlisted), BundleError::Manifest(_)));
    let absent = tar(&[
        entry("manifest.toml", b'0', &manifest(&["review", "gone"])),
        entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
    ]);
    assert!(matches!(refused(&absent), BundleError::Manifest(_)));
    for m in [
        "format = 2\nname = \"kit\"\nworkflows = [\"review\"]\n",
        "name = \"kit\"\nworkflows = [\"review\"]\n",
        "format = 1\nname = \"kit\"\nworkflows = [\"review\"]\nrun = \"rm -rf ~\"\n",
        "not toml = =",
    ] {
        let bytes = tar(&[
            entry("manifest.toml", b'0', m.as_bytes()),
            entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
        ]);
        assert!(matches!(refused(&bytes), BundleError::Manifest(_)), "{m}");
    }
}

#[test]
fn a_truncated_bundle_is_refused_at_every_length() {
    let good = good();
    for len in (0..good.len() - 1024).chain([good.len() - 1024, good.len() - 513, good.len() - 512])
    {
        let _ = refused(&good[..len]);
    }
    // Bytes after the end of the archive have to be zero padding.
    let mut tail = good.clone();
    tail.extend_from_slice(&[0u8; 512 * 18]);
    assert!(
        bundle::read(&tail, &Redactor::new()).is_ok(),
        "the zero padding of a 10 KiB record is fine"
    );
    tail.push(b'x');
    let _ = refused(&tail);
    let mut junk = good;
    junk.extend_from_slice(&entry("workflows/late.toml", b'0', SMALL.as_bytes()));
    let _ = refused(&junk);
}

#[test]
fn a_tar_bomb_of_huge_or_sparse_declared_sizes_is_refused_without_reading() {
    // 8 GiB declared in octal, no data behind it.
    let mut field = [0u8; 12];
    field[..11].copy_from_slice(b"77777777777");
    let bomb = header(b"workflows/x.toml", b'0', 0, "", Some(field)).to_vec();
    assert!(matches!(
        refused(&with(bomb)),
        BundleError::EntryTooBig(_, _)
    ));
    // The base-256 form of a size, which only a huge file needs.
    let mut b256 = [0xffu8; 12];
    b256[0] = 0x80;
    assert!(matches!(
        refused(&with(
            header(b"workflows/x.toml", b'0', 0, "", Some(b256)).to_vec()
        )),
        BundleError::BadHeader(_)
    ));
    // A size that is not octal at all.
    let mut junk = [b' '; 12];
    junk[..3].copy_from_slice(b"9z9");
    assert!(matches!(
        refused(&with(
            header(b"workflows/x.toml", b'0', 0, "", Some(junk)).to_vec()
        )),
        BundleError::BadHeader(_)
    ));
    // A GNU sparse entry that claims an 8 GiB size.
    assert!(matches!(
        refused(&with(
            header(b"workflows/x.toml", b'S', (1 << 33) - 1, "", None).to_vec()
        )),
        BundleError::BadHeader(_) | BundleError::EntryTooBig(_, _)
    ));
    // A directory entry that claims data.
    assert!(matches!(
        refused(&with(entry("agents/", b'5', b"xx"))),
        BundleError::BadHeader(_)
    ));
}

#[test]
fn a_header_with_a_bad_checksum_or_no_ustar_magic_is_refused() {
    let mut bytes = good();
    bytes[0] = b'n';
    assert!(matches!(refused(&bytes), BundleError::BadHeader(_)));
    let mut bytes = good();
    bytes[257..263].copy_from_slice(b"nope\0\0");
    // Fix the checksum so only the magic is wrong.
    bytes[148..156].copy_from_slice(b"        ");
    let sum: u32 = bytes[..512].iter().map(|b| u32::from(*b)).sum();
    bytes[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    assert!(matches!(refused(&bytes), BundleError::BadHeader(_)));
}

#[test]
fn non_utf8_names_and_text_are_refused() {
    let name = raw_entry(b"workflows/\xff\xfe.toml", b'0', SMALL.as_bytes(), "");
    assert!(matches!(
        refused(&with(name)),
        BundleError::NotUtf8(_) | BundleError::Outside(_)
    ));
    let mut body = SMALL.as_bytes().to_vec();
    body.extend_from_slice(b"# \xff\xfe\n");
    let bytes = tar(&[
        entry("manifest.toml", b'0', &manifest(&["x"])),
        entry("workflows/x.toml", b'0', &body),
    ]);
    assert!(matches!(refused(&bytes), BundleError::NotUtf8(_)));
    assert!(matches!(
        refused(&with(entry("README", b'0', b"\xc3\x28"))),
        BundleError::NotUtf8(_)
    ));
    // A NUL inside a name is not a shorter name.
    let nul = raw_entry(b"workflows/x.toml\0../../evil", b'0', SMALL.as_bytes(), "");
    let _ = refused(&with(nul));
}

#[test]
fn text_a_redactor_would_change_is_refused_and_never_echoed() {
    let key = format!("sk-ant-{}", "Zq9".repeat(10));
    let flow = format!("[[step]]\nagent = \"explore\"\nprompt = \"use {key} on {{{{input}}}}\"\n");
    let bytes = tar(&[
        entry("manifest.toml", b'0', &manifest(&["x"])),
        entry("workflows/x.toml", b'0', flow.as_bytes()),
    ]);
    let err = refused(&bytes);
    assert!(matches!(err, BundleError::Credential(_)));
    assert!(!err.to_string().contains(&key));
    // In the README, the description and an agent file too.
    let _ = refused(&with(entry(
        "README.md",
        b'0',
        format!("token {key}").as_bytes(),
    )));
    let m =
        format!("format = 1\nname = \"kit\"\ndescription = \"{key}\"\nworkflows = [\"review\"]\n");
    assert!(matches!(
        refused(&tar(&[
            entry("manifest.toml", b'0', m.as_bytes()),
            entry("workflows/review.toml", b'0', REVIEW.as_bytes())
        ])),
        BundleError::Credential(_)
    ));
    // A key the session registered counts as well.
    let r = Redactor::new();
    r.register(
        &aibus_core::AccountName("work".into()),
        "live-secret-0f9e8d7c",
    );
    let reg = tar(&[
        entry("manifest.toml", b'0', &manifest(&["x"])),
        entry(
            "workflows/x.toml",
            b'0',
            format!("# live-secret-0f9e8d7c\n{SMALL}").as_bytes(),
        ),
    ]);
    assert!(matches!(
        bundle::read(&reg, &r),
        Err(BundleError::Credential(_))
    ));
}

#[test]
fn a_workflow_over_32_steps_or_that_does_not_parse_is_refused() {
    let step = "[[step]]\nagent = \"explore\"\nprompt = \"{{input}}\"\n";
    let long = step.repeat(33);
    let bytes = tar(&[
        entry("manifest.toml", b'0', &manifest(&["x"])),
        entry("workflows/x.toml", b'0', long.as_bytes()),
    ]);
    assert!(
        matches!(refused(&bytes), BundleError::Workflow(n, why) if n == "x" && why.contains("33 steps"))
    );
    let bytes = tar(&[
        entry("manifest.toml", b'0', &manifest(&["x"])),
        entry(
            "workflows/x.toml",
            b'0',
            b"[[step]]\nagent = \"a\"\nprompt = \"{{nope}}\"\n",
        ),
    ]);
    assert!(matches!(refused(&bytes), BundleError::Workflow(_, _)));
    // A `name` inside the file that is not its file name would list under one name and run as another.
    let bytes = tar(&[
        entry("manifest.toml", b'0', &manifest(&["x"])),
        entry(
            "workflows/x.toml",
            b'0',
            format!("name = \"rm-all\"\n{SMALL}").as_bytes(),
        ),
    ]);
    assert!(matches!(refused(&bytes), BundleError::Workflow(_, _)));
}

#[test]
fn an_agent_file_that_does_not_parse_or_names_another_agent_is_refused() {
    let m = b"format = 1\nname = \"kit\"\nworkflows = [\"review\"]\nagents = [\"helper\"]\n";
    let bad = tar(&[
        entry("manifest.toml", b'0', m),
        entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
        entry("agents/helper.md", b'0', b"no front matter"),
    ]);
    assert!(matches!(refused(&bad), BundleError::Agent(_, _)));
    let other = tar(&[
        entry("manifest.toml", b'0', m),
        entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
        entry(
            "agents/helper.md",
            b'0',
            b"---\nname: explore\ndescription: shadows a built-in\n---\nx\n",
        ),
    ]);
    assert!(matches!(refused(&other), BundleError::Agent(_, _)));
}

/// A small, fixed PRNG, so a failure names the seed and replays.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn written(dir: &Path) -> bool {
    dir.exists()
        && fs::read_dir(dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true)
}

#[test]
fn random_and_mutated_tar_bytes_never_panic_and_never_write() {
    const ITERATIONS: usize = 32_000;
    let r = Redactor::new();
    let roster = roster();
    let root = tempfile::tempdir().unwrap();
    let to = Install {
        workflows_dir: root.path().join("w/workflows"),
        agents_dir: root.path().join("w/agents"),
        roster: &roster,
        force: false,
        allow_gates: false,
    };
    let seeds: Vec<Vec<u8>> = vec![
        good(),
        bundle::pack(
            "kit",
            "d",
            &[("review".into(), REVIEW.into()), ("x".into(), SMALL.into())],
            &[],
            Some(("README", "hi")),
            &r,
        )
        .unwrap(),
        with(entry("agents/", b'5', b"")),
    ];
    let mut rng = Rng(0x2710_0271_dead_beef);
    let (mut ok, mut installed) = (0usize, 0usize);
    for i in 0..ITERATIONS {
        let bytes: Vec<u8> = match i % 4 {
            // Random bytes, sometimes block-sized, sometimes with the ustar magic in place.
            0 => {
                let len = if rng.below(2) == 0 {
                    rng.below(4096)
                } else {
                    512 * (1 + rng.below(8))
                };
                let mut b: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                if b.len() >= 512 && rng.below(2) == 0 {
                    b[257..263].copy_from_slice(b"ustar\0");
                }
                b
            }
            // A valid bundle with a few bytes flipped.
            1 | 2 => {
                let mut b = seeds[rng.below(seeds.len())].clone();
                for _ in 0..1 + rng.below(8) {
                    let at = rng.below(b.len());
                    b[at] = rng.next() as u8;
                }
                b
            }
            // A valid bundle cut, or with a piece of itself spliced in.
            _ => {
                let mut b = seeds[rng.below(seeds.len())].clone();
                if rng.below(2) == 0 {
                    b.truncate(rng.below(b.len()));
                } else {
                    let from = rng.below(b.len());
                    let piece = b[from..(from + rng.below(1024)).min(b.len())].to_vec();
                    let at = rng.below(b.len());
                    b.splice(at..at, piece);
                }
                b
            }
        };
        let read = bundle::read(&bytes, &r);
        ok += usize::from(read.is_ok());
        match bundle::install(&bytes, &to, &r) {
            Ok(paths) => {
                assert!(
                    read.is_ok(),
                    "iteration {i}: install wrote what read refused"
                );
                installed += 1;
                for p in paths {
                    fs::remove_file(p).unwrap();
                }
            }
            Err(_) => assert!(
                !written(&to.workflows_dir) && !written(&to.agents_dir),
                "iteration {i}: a refused install wrote a file"
            ),
        }
    }
    // The run did reach past the first checks: some mutations only touch padding or text.
    assert!(ok > 0 && ok < ITERATIONS, "{ok} of {ITERATIONS} read");
    assert!(installed <= ok);
}

// --- Review of #277: install, listing and terminal output. ---

fn install_into(
    root: &Path,
    bytes: &[u8],
    force: bool,
    gates: bool,
) -> Result<Vec<std::path::PathBuf>, bundle::InstallError> {
    let roster = roster();
    let to = Install {
        workflows_dir: root.join(".aibus/workflows"),
        agents_dir: root.join(".aibus/agents"),
        roster: &roster,
        force,
        allow_gates: gates,
    };
    bundle::install(bytes, &to, redactor())
}

fn with_agent(name: &str, def: &str) -> Vec<u8> {
    let m = format!("format = 1\nname = \"kit\"\nworkflows = [\"x\"]\nagents = [\"{name}\"]\n");
    let flow = format!("[[step]]\nagent = \"{name}\"\nprompt = \"{{{{input}}}}\"\n");
    tar(&[
        entry("manifest.toml", b'0', m.as_bytes()),
        entry("workflows/x.toml", b'0', flow.as_bytes()),
        entry(&format!("agents/{name}.md"), b'0', def.as_bytes()),
    ])
}

#[cfg(unix)]
#[test]
fn install_never_writes_through_a_planted_temp_file_symlink() {
    let root = tempfile::tempdir().unwrap();
    let victim = root.path().join("victim.txt");
    fs::write(&victim, "precious\n").unwrap();
    let wd = root.path().join(".aibus/workflows");
    fs::create_dir_all(&wd).unwrap();
    // The name the old atomic write used for its temp file, pointing out of the directory.
    std::os::unix::fs::symlink(&victim, wd.join(".review.toml.tmp")).unwrap();
    install_into(root.path(), &good(), false, false).unwrap();
    assert_eq!(fs::read_to_string(&victim).unwrap(), "precious\n");
    let md = fs::symlink_metadata(wd.join("review.toml")).unwrap();
    assert!(
        md.is_file() && !md.file_type().is_symlink(),
        "review.toml is a plain file"
    );
    assert_eq!(fs::read_to_string(wd.join("review.toml")).unwrap(), REVIEW);
}

#[test]
fn a_bundle_cannot_shadow_a_built_in_agent_without_force() {
    let builtins = aibus_core::agent::builtin::names();
    assert!(
        builtins.iter().any(|n| n.as_str() == "lead"),
        "{builtins:?}"
    );
    for name in &builtins {
        let def = format!("---\nname: {name}\ndescription: replaced\n---\nDo something else.\n");
        let root = tempfile::tempdir().unwrap();
        let err =
            install_into(root.path(), &with_agent(name.as_str(), &def), false, true).unwrap_err();
        assert!(
            matches!(&err, bundle::InstallError::ShadowsBuiltin(n) if n == name.as_str()),
            "{name}: {err}"
        );
        assert!(err.to_string().contains("--force"), "{err}");
        assert!(
            !root.path().join(".aibus").exists(),
            "{name}: nothing written"
        );
    }
    let root = tempfile::tempdir().unwrap();
    let def = "---\nname: lead\ndescription: replaced\n---\nDo something else.\n";
    install_into(root.path(), &with_agent("lead", def), true, true).unwrap();
    assert!(root.path().join(".aibus/agents/lead.md").is_file());
}

#[test]
fn a_bundled_agent_with_an_account_or_an_initial_prompt_needs_the_gates_flag() {
    for extra in ["account: work", "initial_prompt: run the deploy script"] {
        let def = format!("---\nname: helper\ndescription: helps\n{extra}\n---\nHelp.\n");
        let root = tempfile::tempdir().unwrap();
        let err = install_into(root.path(), &with_agent("helper", &def), false, false).unwrap_err();
        assert!(
            matches!(&err, bundle::InstallError::AgentPowers(n) if n == "helper"),
            "{extra}: {err}"
        );
        assert!(!root.path().join(".aibus").exists());
        install_into(root.path(), &with_agent("helper", &def), false, true).unwrap();
    }
}

#[test]
fn control_characters_in_bundled_text_are_refused_and_never_echoed() {
    // TOML escapes put an OSC title and a clear-screen into the parsed strings.
    for flow in [
        "description = \"ok\\u001b]0;pwned\\u0007\"\n[[step]]\nagent = \"explore\"\nprompt = \"{{input}}\"\n",
        "[[step]]\nagent = \"explore\"\nprompt = \"{{input}}\\u001b[2J\"\n",
        "[[step]]\nagent = \"explore\"\nprompt = \"{{input}}\"\ngate = \"true\\u001b[2J\"\n",
    ] {
        let bytes = tar(&[entry("manifest.toml", b'0', &manifest(&["x"])), entry("workflows/x.toml", b'0', flow.as_bytes())]);
        let err = refused(&bytes);
        assert!(matches!(err, BundleError::Control(_)), "{flow}: {err:?}");
    }
    // Raw bytes too, in a README and the manifest's description.
    let _ = refused(&with(entry("README", b'0', b"hi \x1b[2J")));
    let m = b"format = 1\nname = \"kit\"\ndescription = \"a\\u001b]0;t\\u0007\"\nworkflows = [\"review\"]\n";
    assert!(matches!(
        refused(&tar(&[
            entry("manifest.toml", b'0', m),
            entry("workflows/review.toml", b'0', REVIEW.as_bytes())
        ])),
        BundleError::Control(_)
    ));
    // An entry name with an escape in it reaches the error text escaped.
    let err = refused(&with(entry(
        "workflows/a\x1b]0;x\x07.toml",
        b'0',
        SMALL.as_bytes(),
    )));
    assert!(
        !err.to_string().contains('\u{1b}') && !err.to_string().contains('\u{7}'),
        "{err}"
    );
    let err = refused(&with(entry("notes\x1b[2J.txt", b'0', b"x")));
    assert!(!err.to_string().contains('\u{1b}'), "{err}");
}

#[cfg(unix)]
#[test]
fn a_fifo_a_device_link_or_a_huge_file_never_hangs_or_is_read() {
    use std::sync::mpsc;
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let d = root.path().to_path_buf();
    assert!(std::process::Command::new("mkfifo")
        .arg(d.join("kit.aibus"))
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("mkfifo")
        .arg(d.join("loose.toml"))
        .status()
        .unwrap()
        .success());
    std::os::unix::fs::symlink("/dev/zero", d.join("zero.aibus")).unwrap();
    std::os::unix::fs::symlink("/dev/zero", d.join("zero.toml")).unwrap();
    let huge = fs::File::create(d.join("huge.toml")).unwrap();
    huge.set_len(10 << 30).unwrap();
    let (tx, rx) = mpsc::channel();
    let d2 = d.clone();
    std::thread::spawn(move || {
        let _ = tx.send(aibus_core::workflow::list_in(&d2, "project"));
    });
    let found = match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(f) => f,
        Err(_) => {
            // Unblock whatever is stuck before failing: opening the write end of a fifo waits for
            // a reader too, so each open gets its own thread and nothing here waits on it.
            for f in ["kit.aibus", "loose.toml"] {
                let p = d.join(f);
                std::thread::spawn(move || {
                    let _ = fs::OpenOptions::new().write(true).open(p);
                });
            }
            panic!("listing blocked on a fifo");
        }
    };
    let why = |n: &str| {
        found
            .iter()
            .find(|f| f.name == n)
            .and_then(|f| f.parsed.as_ref().err())
            .map(ToString::to_string)
            .unwrap_or_else(|| panic!("{n} is a refused row in {found:?}"))
    };
    for n in ["kit.aibus", "zero.aibus", "loose", "zero"] {
        assert!(why(n).contains("not a regular file"), "{n}: {}", why(n));
    }
    assert!(why("huge").contains("bytes"), "{}", why("huge"));
}

#[cfg(unix)]
#[test]
fn read_file_checks_the_type_and_the_size_before_reading() {
    let root = tempfile::tempdir().unwrap();
    let big = root.path().join("big.aibus");
    fs::File::create(&big).unwrap().set_len(10 << 30).unwrap();
    // The size from the file, not from what was read: nothing past the cap is read.
    assert_eq!(
        bundle::read_file(&big, redactor()),
        Err(BundleError::TooBig(10 << 30))
    );
    std::os::unix::fs::symlink("/dev/zero", root.path().join("zero.aibus")).unwrap();
    let err = bundle::read_file(&root.path().join("zero.aibus"), redactor()).unwrap_err();
    assert!(
        matches!(&err, BundleError::Io(_, why) if why == "not a regular file"),
        "{err:?}"
    );
    let err = bundle::read_file(root.path(), redactor()).unwrap_err();
    assert!(
        matches!(&err, BundleError::Io(_, why) if why == "not a regular file"),
        "{err:?}"
    );
}

#[cfg(unix)]
#[test]
fn install_refuses_a_symlinked_aibus_or_workflows_directory() {
    for link in [".aibus", ".aibus/workflows"] {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = root.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::create_dir_all(root.path().join(link).parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.path().join(link)).unwrap();
        let err = install_into(root.path(), &good(), true, true).unwrap_err();
        assert!(
            matches!(err, bundle::InstallError::NotPlain(_)),
            "{link}: {err:?}"
        );
        assert!(
            fs::read_dir(&elsewhere).unwrap().next().is_none(),
            "{link}: nothing written through it"
        );
    }
}

#[test]
fn install_refuses_a_workflows_dir_or_target_that_is_not_plain() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join(".aibus")).unwrap();
    fs::write(root.path().join(".aibus/workflows"), "a file").unwrap();
    let err = install_into(root.path(), &good(), true, true).unwrap_err();
    assert_eq!(
        err,
        bundle::InstallError::NotPlain(root.path().join(".aibus/workflows"))
    );

    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join(".aibus/workflows/review.toml")).unwrap();
    let err = install_into(root.path(), &good(), true, true).unwrap_err();
    assert_eq!(
        err,
        bundle::InstallError::NotPlain(root.path().join(".aibus/workflows/review.toml"))
    );
}

/// Run as a user who cannot write a 0500 directory (root can, so this is skipped there; the gate
/// runs this test binary once more as `nobody`).
#[cfg(unix)]
#[test]
fn a_failed_install_leaves_nothing_behind() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let ad = root.path().join(".aibus/agents");
    fs::create_dir_all(&ad).unwrap();
    fs::set_permissions(&ad, fs::Permissions::from_mode(0o500)).unwrap();
    if fs::write(ad.join("probe"), "x").is_ok() {
        eprintln!("this user can write a 0500 directory (root?), skipping");
        return;
    }
    let def = "---\nname: helper\ndescription: helps\n---\nHelp.\n";
    let err = install_into(root.path(), &with_agent("helper", def), false, false);
    fs::set_permissions(&ad, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(err.is_err());
    let wd = root.path().join(".aibus/workflows");
    let left: Vec<_> = fs::read_dir(&wd)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "left behind: {left:?}");
}

#[test]
fn the_manifest_agents_list_has_to_match_the_agent_files() {
    let def = "---\nname: helper\ndescription: helps\n---\nHelp.\n";
    let unlisted = tar(&[
        entry("manifest.toml", b'0', &manifest(&["review"])),
        entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
        entry("agents/helper.md", b'0', def.as_bytes()),
    ]);
    assert!(matches!(refused(&unlisted), BundleError::Manifest(_)));
    let m = b"format = 1\nname = \"kit\"\nworkflows = [\"review\"]\nagents = [\"helper\"]\n";
    let absent = tar(&[
        entry("manifest.toml", b'0', m),
        entry("workflows/review.toml", b'0', REVIEW.as_bytes()),
    ]);
    assert!(matches!(refused(&absent), BundleError::Manifest(_)));
}

#[test]
fn a_directory_entry_whose_data_is_a_hidden_header_is_refused() {
    // The data block of `agents/` is itself a valid header: skipping it as padding would read it
    // as an entry.
    let hidden = entry("workflows/evil.toml", b'0', SMALL.as_bytes());
    let mut dir = header(b"agents/", b'5', 512, "", None).to_vec();
    dir.extend_from_slice(&hidden[..512]);
    let bytes = with(dir);
    assert!(matches!(refused(&bytes), BundleError::BadHeader(_)));
}

#[test]
fn bytes_after_a_nul_in_a_name_and_a_stray_byte_after_a_size_are_bad_headers() {
    let nul = raw_entry(
        b"workflows/review.toml\0../evil",
        b'0',
        REVIEW.as_bytes(),
        "",
    );
    let bytes = tar(&[entry("manifest.toml", b'0', &manifest(&["review"])), nul]);
    assert!(matches!(refused(&bytes), BundleError::BadHeader(_)));
    let mut size = [0u8; 12];
    size[..11].copy_from_slice(format!("{:011o}", REVIEW.len()).as_bytes());
    size[11] = 0x01;
    let mut review = header(b"workflows/review.toml", b'0', 0, "", Some(size)).to_vec();
    review.extend_from_slice(REVIEW.as_bytes());
    review.resize(review.len().div_ceil(512) * 512, 0);
    let bytes = tar(&[entry("manifest.toml", b'0', &manifest(&["review"])), review]);
    assert!(matches!(refused(&bytes), BundleError::BadHeader(_)));
}

#[test]
fn only_so_many_bundles_in_one_directory_are_read() {
    let root = tempfile::tempdir().unwrap();
    for i in 0..bundle::MAX_BUNDLES_PER_DIR + 3 {
        fs::write(root.path().join(format!("b{i:03}.aibus")), b"not a tar").unwrap();
    }
    let found = aibus_core::workflow::list_in(root.path(), "project");
    let read = found
        .iter()
        .filter(|f| {
            f.parsed
                .as_ref()
                .is_err_and(|e| e.to_string().contains("cut short"))
        })
        .count();
    assert_eq!(read, bundle::MAX_BUNDLES_PER_DIR);
    assert!(
        found.iter().any(|f| f
            .parsed
            .as_ref()
            .is_err_and(|e| e.to_string().contains("more than"))),
        "{found:?}"
    );
}
