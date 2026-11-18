//! Owned filesystem fixtures; every path is created under this process's unique
//! private directory. The path is canonical so platform /tmp links aren't read.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
pub(crate) struct Fixture(pub PathBuf);
impl Fixture {
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-ops-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// The lowercase hexadecimal SHA-256 by which a recording names the input of a case.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A retained reference's recorded answers, test by test under `tests`. Each case of a test
/// holds the SHA-256 of the exact input the reference was given (`input`) and what it
/// answered (`answer`); the recording's `comment` says what produced it.
pub(crate) struct Recording(serde_json::Value);
impl Recording {
    /// Parses `text`, which must hold the recordings of exactly `tests`.
    pub(crate) fn new(text: &str, tests: &[&str]) -> Self {
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        let mut recorded = value["tests"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut expected = tests.to_vec();
        recorded.sort_unstable();
        expected.sort_unstable();
        assert_eq!(
            recorded, expected,
            "the recorded tests are not the replayed ones"
        );
        Self(value)
    }
    /// Everything recorded for `test`.
    pub(crate) fn test(&self, test: &str) -> &serde_json::Value {
        let recorded = &self.0["tests"][test];
        assert!(recorded.is_object(), "{test} has no recording");
        recorded
    }
    /// The cases recorded for `test`, to be replayed in the order the test asked them.
    pub(crate) fn cases(&self, test: &str) -> Recorded {
        Recorded {
            test: test.into(),
            cases: self.test(test)["cases"].as_array().unwrap().clone(),
            next: 0,
        }
    }
}

/// One test's recorded cases, consumed in order. A case the test builds must be the input the
/// reference answered, and every recorded case must be asked for.
pub(crate) struct Recorded {
    test: String,
    cases: Vec<serde_json::Value>,
    next: usize,
}
impl Recorded {
    /// The whole next recorded case, once `input` is shown to be the bytes it was recorded for.
    pub(crate) fn case(&mut self, input: &[u8]) -> serde_json::Value {
        let number = self.next;
        let Some(case) = self.cases.get(number) else {
            panic!("{}: case {number} has no recording", self.test);
        };
        assert_eq!(
            case["input"].as_str(),
            Some(sha256_hex(input).as_str()),
            "{}: case {number} is not the input that was recorded",
            self.test
        );
        self.next += 1;
        case.clone()
    }
    /// What the reference answered the next case, which must be `input`.
    pub(crate) fn answer(&mut self, input: &[u8]) -> serde_json::Value {
        let mut case = self.case(input);
        case["answer"].take()
    }
    /// The number of cases replayed, once every recorded case has been asked for.
    pub(crate) fn finish(self) -> usize {
        assert_eq!(
            self.next,
            self.cases.len(),
            "{}: recorded cases were never asked for",
            self.test
        );
        self.next
    }
}

/// A recorder's printed tally as a map, to compare with the one a replay counts.
pub(crate) fn tally(counts: &[(&str, usize)]) -> std::collections::BTreeMap<String, usize> {
    counts
        .iter()
        .map(|(name, count)| ((*name).to_string(), *count))
        .collect()
}

/// Set to `1` in a child fixture's environment by the parent test that owns it.
pub(crate) const CHILD_FIXTURE: &str = "SIMPLESTCHAT_CHILD_FIXTURE";

/// Whether this test process is a child fixture launched by its owning parent.
/// A broad `--include-ignored` run reaches the fixture without the variable;
/// the fixture then returns at once instead of failing for want of its parent.
pub(crate) fn launched_by_parent() -> bool {
    std::env::var_os(CHILD_FIXTURE).is_some_and(|value| value == "1")
}
