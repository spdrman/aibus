//! Versions and ranges (#286): exact `x.y.z` and comparator ranges.

use aibus_extension::version::{exact, Range, Version};

#[test]
fn an_exact_version_is_three_plain_numbers() {
    assert_eq!(exact("1.2.3"), Some(Version(1, 2, 3)));
    for bad in [
        "1.2",
        "1.2.3.4",
        "1.2.x",
        "01.2.3",
        "1.2.3-rc1",
        "1.2.3+b",
        " 1.2.3",
        "1.2.3\n",
        "",
        "-1.0.0",
        "99999999999.0.0",
    ] {
        assert_eq!(exact(bad), None, "{bad:?}");
    }
}

#[test]
fn a_range_is_comparators_that_must_all_hold() {
    let r = Range::parse(">=1.0.0, <2.0.0").unwrap();
    assert!(r.contains(&Version(1, 0, 0)) && r.contains(&Version(1, 9, 9)));
    assert!(!r.contains(&Version(0, 9, 9)) && !r.contains(&Version(2, 0, 0)));
    assert!(Range::parse("=1.2.3").unwrap().contains(&Version(1, 2, 3)));
    assert!(!Range::parse("=1.2.3").unwrap().contains(&Version(1, 2, 4)));
    assert!(Range::parse(">1.2.3, <=1.3.0")
        .unwrap()
        .contains(&Version(1, 3, 0)));
}

#[test]
fn a_malformed_range_is_an_error() {
    for bad in [
        "",
        "^1.0.0",
        "~1",
        "latest",
        ">=",
        ">=1.0",
        "1.0.0",
        ">=1.0.0 <2.0.0",
        ">=1.0.0,",
        "*",
    ] {
        assert!(Range::parse(bad).is_err(), "{bad:?}");
    }
}
