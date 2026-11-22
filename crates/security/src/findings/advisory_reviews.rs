//! The cargo-audit reviews of the committed policy, held to the lockfile.
//!
//! A cargo-audit review is bound to one advisory and to one `name@version`.
//! The gate compares a review with what the scanner reports; this test
//! compares it with `Cargo.lock` itself, so that a review of a package that
//! is no longer locked is found although the scanner reports nothing for it.

use crate::policy::{self, Date};
use std::{collections::BTreeSet, path::Path};

/// Every `name@version` that the lockfile `text` holds.
fn locked(text: &str) -> BTreeSet<String> {
    let lock: toml::Value = toml::from_str(text).expect("a lockfile");
    let packages = lock["package"].as_array().expect("its packages");
    packages
        .iter()
        .map(|package| {
            let name = package["name"].as_str().expect("a package name");
            let version = package["version"].as_str().expect("a package version");
            format!("{name}@{version}")
        })
        .collect()
}

#[test]
fn every_committed_cargo_audit_review_names_a_locked_package() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join("Cargo.lock")).expect("the lockfile");
    let locked = locked(&text);
    // Read as of the first day: whether a review has expired is the gate's
    // question on the day it runs.
    let reviews: Vec<_> = policy::read(&root.join("security/exceptions.json"), Date::MIN, false)
        .expect("the policy")
        .into_iter()
        .filter(|review| review.scanner == "cargo-audit")
        .collect();
    assert!(!reviews.is_empty());
    for review in reviews {
        assert!(locked.contains(&review.scope), "{}", review.scope);
    }
}

#[test]
fn a_lockfile_names_each_version_of_a_package() {
    let text = concat!(
        "version = 4\n\n",
        "[[package]]\nname = \"owned\"\nversion = \"1.0.0\"\n\n",
        "[[package]]\nname = \"owned\"\nversion = \"2.0.0\"\n",
    );
    let expected = ["owned@1.0.0", "owned@2.0.0"].map(str::to_owned);
    assert_eq!(locked(text), expected.into());
}
