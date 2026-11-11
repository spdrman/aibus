//! rataflow keeps id lookups in `HashMap`s and renders edges through one; this checks that the
//! order of a map never reaches the buffer. Run with `--features flow-diagram`.
#![cfg(feature = "flow-diagram")]

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use aibus_testkit::tui::{render, replay_app};

#[test]
fn two_hundred_renders_of_one_state_hash_to_one_value() {
    for size in [(98, 46), (80, 24), (120, 50)] {
        let mut hashes = std::collections::BTreeSet::new();
        for _ in 0..200 {
            // A fresh app each time: fresh maps, fresh hasher seeds inside rataflow.
            let mut app = replay_app();
            let f = render(&mut app, size, 2);
            let mut h = DefaultHasher::new();
            (&f.text, &f.styles).hash(&mut h);
            hashes.insert(h.finish());
        }
        assert_eq!(hashes.len(), 1, "{size:?} rendered {} different frames", hashes.len());
    }
}
