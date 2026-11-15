//! Reading a package that somebody else made (#286): names, links, special files, sizes, depth,
//! case collisions and the content hash. The hostile cases mirror what the `.aibus` bundle reader
//! refuses (#271) for a directory instead of a tar.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;

use aibus_extension::package::{self, Package, PackageError};
use common::{package_with, template_manifest};

fn base() -> (tempfile::TempDir, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let p = package_with(t.path(), "pkg", &template_manifest());
    (t, p)
}

#[test]
fn a_good_package_reads_and_its_hash_is_stable() {
    let (_t, p) = base();
    let a = package::read(&p).unwrap();
    let b = package::read(&p).unwrap();
    assert_eq!(a.hash, b.hash);
    assert!(a.hash.starts_with("sha256-tree-v1:") && a.hash.len() == "sha256-tree-v1:".len() + 64);
    assert!(
        a.files.contains_key("aibus-extension.toml")
            && a.files.contains_key("fixtures/basic.jsonl")
    );
}

#[test]
fn one_changed_byte_one_added_file_or_one_rename_changes_the_hash() {
    let (_t, p) = base();
    let before = package::read(&p).unwrap().hash;
    fs::write(
        p.join("fixtures/basic.jsonl"),
        b"{\"type\":\"text\",\"text\":\"hellp\"}\n",
    )
    .unwrap();
    let changed = package::read(&p).unwrap().hash;
    assert_ne!(before, changed);
    fs::write(p.join("extra.txt"), b"x").unwrap();
    let added = package::read(&p).unwrap().hash;
    assert_ne!(changed, added);
    fs::rename(p.join("extra.txt"), p.join("extra2.txt")).unwrap();
    assert_ne!(added, package::read(&p).unwrap().hash);
}

#[test]
fn the_hash_does_not_depend_on_directory_order_or_file_times() {
    let (_t, p) = base();
    let a = package::read(&p).unwrap().hash;
    let f = p.join("README.md");
    let bytes = fs::read(&f).unwrap();
    fs::remove_file(&f).unwrap();
    fs::write(&f, bytes).unwrap();
    assert_eq!(a, package::read(&p).unwrap().hash);
}

#[test]
fn a_symlinked_file_or_directory_inside_the_package_is_refused() {
    let (t, p) = base();
    fs::write(t.path().join("secret"), b"s").unwrap();
    symlink(t.path().join("secret"), p.join("fixtures/link.jsonl")).unwrap();
    assert!(matches!(package::read(&p), Err(PackageError::Link(_))));
    fs::remove_file(p.join("fixtures/link.jsonl")).unwrap();
    symlink(t.path(), p.join("dirlink")).unwrap();
    assert!(matches!(package::read(&p), Err(PackageError::Link(_))));
}

#[test]
fn a_symlinked_package_directory_is_refused() {
    let (t, p) = base();
    let l = t.path().join("alias");
    symlink(&p, &l).unwrap();
    assert!(matches!(package::read(&l), Err(PackageError::Link(_))));
}

#[test]
fn a_symlinked_manifest_is_refused() {
    let (t, p) = base();
    fs::rename(p.join("aibus-extension.toml"), t.path().join("real.toml")).unwrap();
    symlink(t.path().join("real.toml"), p.join("aibus-extension.toml")).unwrap();
    assert!(matches!(package::read(&p), Err(PackageError::Link(_))));
}

#[test]
fn a_fifo_is_refused_without_hanging() {
    let (_t, p) = base();
    let status = std::process::Command::new("mkfifo")
        .arg(p.join("fixtures/pipe.jsonl"))
        .status()
        .unwrap();
    assert!(status.success());
    assert!(matches!(package::read(&p), Err(PackageError::Special(_))));
}

#[test]
fn a_file_over_the_per_file_cap_is_refused() {
    let (_t, p) = base();
    fs::write(
        p.join("big.bin"),
        vec![0u8; (package::MAX_FILE_BYTES + 1) as usize],
    )
    .unwrap();
    assert!(matches!(
        package::read(&p),
        Err(PackageError::FileTooBig(_))
    ));
}

#[test]
fn a_package_over_the_total_cap_is_refused() {
    let (_t, p) = base();
    for i in 0..5 {
        fs::write(
            p.join(format!("part{i}.bin")),
            vec![1u8; package::MAX_FILE_BYTES as usize],
        )
        .unwrap();
    }
    assert!(matches!(package::read(&p), Err(PackageError::TooBig)));
}

#[test]
fn too_many_files_are_refused() {
    let (_t, p) = base();
    for i in 0..package::MAX_FILES {
        fs::write(p.join(format!("f{i}")), b"x").unwrap();
    }
    assert!(matches!(package::read(&p), Err(PackageError::TooManyFiles)));
}

#[test]
fn a_directory_nested_too_deep_is_refused() {
    let (_t, p) = base();
    let mut d = p.clone();
    for i in 0..=package::MAX_DEPTH {
        d = d.join(format!("d{i}"));
    }
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("x"), b"x").unwrap();
    assert!(matches!(package::read(&p), Err(PackageError::TooDeep(_))));
}

#[test]
fn names_that_could_escape_or_confuse_are_refused() {
    for bad in [
        "../x",
        "/abs",
        "a//b",
        "a/./b",
        ".hidden",
        "a/.git/x",
        "with\nnewline",
        "back\\slash",
        "nul\0byte",
        "",
        "a/",
        &"n".repeat(300),
    ] {
        let mut files = BTreeMap::new();
        files.insert(
            "aibus-extension.toml".to_owned(),
            template_manifest().into_bytes(),
        );
        files.insert(bad.to_owned(), b"x".to_vec());
        assert!(
            matches!(Package::from_files(files), Err(PackageError::BadName(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn a_non_utf8_name_on_disk_is_refused() {
    let (_t, p) = base();
    let name = std::ffi::OsStr::from_bytes(b"bad\xff.txt");
    // APFS refuses such a name outright; there is nothing to read then.
    if fs::write(p.join(name), b"x").is_err() {
        return;
    }
    assert!(matches!(package::read(&p), Err(PackageError::BadName(_))));
}

#[test]
fn two_names_that_differ_only_by_case_are_refused() {
    let mut files = BTreeMap::new();
    files.insert(
        "aibus-extension.toml".to_owned(),
        template_manifest().into_bytes(),
    );
    files.insert("README.md".to_owned(), b"a".to_vec());
    files.insert("readme.md".to_owned(), b"b".to_vec());
    assert!(matches!(
        Package::from_files(files),
        Err(PackageError::CaseCollision(..))
    ));
}

#[test]
fn a_package_without_a_manifest_or_with_a_bad_one_is_refused() {
    let (_t, p) = base();
    fs::write(p.join("aibus-extension.toml"), b"not = toml = at all").unwrap();
    assert!(matches!(package::read(&p), Err(PackageError::Manifest(_))));
    fs::remove_file(p.join("aibus-extension.toml")).unwrap();
    assert!(matches!(package::read(&p), Err(PackageError::NoManifest)));
}

#[test]
fn a_fixture_the_manifest_names_has_to_be_in_the_package() {
    let (_t, p) = base();
    fs::remove_file(p.join("fixtures/failed-tool.jsonl")).unwrap();
    assert!(matches!(package::read(&p), Err(PackageError::Manifest(_))));
}

#[test]
fn a_path_that_is_not_a_directory_is_refused() {
    let (_t, p) = base();
    assert!(matches!(
        package::read(&p.join("README.md")),
        Err(PackageError::NotADirectory(_))
    ));
    assert!(matches!(
        package::read(&p.join("missing")),
        Err(PackageError::NotADirectory(_) | PackageError::Io(_))
    ));
}

#[test]
fn write_then_read_gives_the_same_package_and_private_modes() {
    use std::os::unix::fs::PermissionsExt;
    let (t, p) = base();
    let pkg = package::read(&p).unwrap();
    let out = t.path().join("out");
    pkg.write(&out).unwrap();
    assert_eq!(package::read(&out).unwrap(), pkg);
    assert_eq!(fs::metadata(&out).unwrap().permissions().mode() & 0o077, 0);
    assert_eq!(
        fs::metadata(out.join("README.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
    assert!(
        pkg.write(&out).is_err(),
        "an existing directory is never written into"
    );
}

// ---- #332 review, item 7: hard links, empty directories, fd-relative walking ----

#[test]
fn a_hard_linked_file_is_refused() {
    let (t, p) = base();
    fs::write(t.path().join("outside.txt"), b"x").unwrap();
    fs::hard_link(t.path().join("outside.txt"), p.join("fixtures/hard.jsonl")).unwrap();
    assert!(matches!(package::read(&p), Err(PackageError::Link(_))));
}

#[test]
fn empty_directories_count_toward_the_file_cap() {
    let (_t, p) = base();
    for i in 0..package::MAX_FILES + 1 {
        fs::create_dir(p.join(format!("empty{i}"))).unwrap();
    }
    assert!(matches!(package::read(&p), Err(PackageError::TooManyFiles)));
}

#[test]
fn a_directory_that_is_a_link_is_refused_at_any_depth() {
    let (t, p) = base();
    fs::create_dir_all(p.join("a/b")).unwrap();
    symlink(t.path(), p.join("a/b/c")).unwrap();
    assert!(matches!(package::read(&p), Err(PackageError::Link(_))));
}
