//! The recorded answers of retired inline programs, test by test. Each case
//! of a test holds the SHA-256 of the canonical JSON description the
//! program was given (`input`) and what it answered (`answer`); a replay
//! builds its own descriptions and must ask for every recorded case in
//! order.

use super::ansible::{Verdict, first_failures};
use serde_json::Value;

/// The canonical bytes of a case description: compact JSON with sorted
/// keys, as the recorder bound them.
pub(super) fn canonical(case: &Value) -> Vec<u8> {
    serde_json::to_vec(case).unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One test's recorded cases, consumed in order.
pub(super) struct Recorded {
    test: String,
    cases: Vec<Value>,
    next: usize,
}

impl Recorded {
    /// The cases recorded for `test` in `text`, which must hold the
    /// recordings of exactly `tests`.
    pub(super) fn new(text: &str, tests: &[&str], test: &str) -> Self {
        let value: Value = serde_json::from_str(text).unwrap();
        let mut recorded: Vec<&str> = value["tests"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut expected = tests.to_vec();
        recorded.sort_unstable();
        expected.sort_unstable();
        assert_eq!(
            recorded, expected,
            "the recorded tests are not the replayed ones"
        );
        let cases = value["tests"][test]["cases"].as_array().unwrap().clone();
        Self {
            test: test.to_owned(),
            cases,
            next: 0,
        }
    }

    /// What the program answered the next case, once `case` is shown to be
    /// the description that was recorded.
    pub(super) fn answer(&mut self, case: &Value) -> Value {
        let number = self.next;
        let Some(recorded) = self.cases.get(number) else {
            panic!("{}: case {number} has no recording", self.test);
        };
        assert_eq!(
            recorded["input"].as_str(),
            Some(sha256_hex(&canonical(case)).as_str()),
            "{}: case {number} is not the description that was recorded",
            self.test
        );
        self.next += 1;
        recorded["answer"].clone()
    }

    /// The number of cases replayed, once every recorded case was asked for.
    pub(super) fn finish(self) -> usize {
        assert_eq!(
            self.next,
            self.cases.len(),
            "{}: recorded cases were never asked for",
            self.test
        );
        self.next
    }
}

/// How a program ended, as a replay compares it: accepted, refused with
/// its own fixed words, or refused by a traceback, whose words are no
/// contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Ending {
    Accepted,
    Refused(String),
    Thrown,
}

/// The ending a recorded answer describes.
pub(super) fn ending(answer: &Value) -> Ending {
    if answer.get("traceback").is_some() {
        assert_eq!(answer["exit"], 1);
        return Ending::Thrown;
    }
    if answer["exit"] == 0 {
        return Ending::Accepted;
    }
    assert_eq!(answer["exit"], 1);
    let words = answer["stderr"].as_str().unwrap();
    Ending::Refused(words.strip_suffix('\n').unwrap_or(words).to_owned())
}

/// Whether Ansible decided a case as the program did: an accepted case
/// passes, a refusal in fixed words fails with those words, and a case the
/// program threw on fails, or the module refuses it before any assertion.
pub(super) fn agrees(ending: &Ending, decided: Option<&Verdict>) -> bool {
    match (ending, decided) {
        (Ending::Accepted, Some(Verdict::Passed)) => true,
        (Ending::Refused(words), Some(Verdict::Failed(_, said))) => said == words,
        (Ending::Thrown, Some(Verdict::Failed(..)) | None) => true,
        _ => false,
    }
}

/// Replay `cases` through `tasks`: `results` builds each case's registered
/// variables, or nothing where a module refuses it. Each of `stricter` is
/// a case the program accepted and the tasks deliberately refuse: with the
/// given words, or in any way, the module's included, where none are
/// given. Returns how many cases were replayed.
pub(super) fn replay(
    mut recorded: Recorded,
    test: &str,
    tasks: &[Value],
    cases: &[Value],
    results: impl Fn(&Value) -> Option<Value>,
    stricter: &[(usize, Option<&str>)],
) -> usize {
    let variables: Vec<Option<Value>> = cases.iter().map(&results).collect();
    let evaluated: Vec<Value> = variables.iter().flatten().cloned().collect();
    let mut verdicts = first_failures(tasks, &evaluated).into_iter();
    for (index, (case, variables)) in cases.iter().zip(&variables).enumerate() {
        let ending = ending(&recorded.answer(case));
        let decided = variables.as_ref().map(|_| verdicts.next().unwrap());
        if let Some((_, words)) = stricter.iter().find(|(number, _)| *number == index) {
            assert_eq!(ending, Ending::Accepted, "{test} {index}");
            let refused = match (words, &decided) {
                (Some(words), Some(Verdict::Failed(_, said))) => said == words,
                (None, decided) => !matches!(decided, Some(Verdict::Passed)),
                _ => false,
            };
            assert!(refused, "{test} {index}: {decided:?}");
            continue;
        }
        assert!(
            agrees(&ending, decided.as_ref()),
            "{test} {index}: {ending:?} {decided:?}"
        );
    }
    recorded.finish()
}
