//! Ratchet behavior against owned temporary Git repositories.

use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A temporary `git init` checkout, removed when dropped.
struct Repository(PathBuf);

impl Drop for Repository {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Repository {
    fn new() -> Self {
        let repository = Self(std::env::temp_dir().join(format!(
            "simplestchat-python-lines-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        fs::create_dir(&repository.0).unwrap();
        // The allowance lives beside the crate, as in the real checkout.
        fs::create_dir(repository.0.join("xtask")).unwrap();
        repository.git(&["init", "--quiet"]);
        repository
    }

    fn git(&self, arguments: &[&str]) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(&self.0)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// Write a file and stage it.
    fn track(&self, path: &str, contents: &str) {
        self.write(path, contents);
        self.git(&["add", "--", path]);
    }

    /// Delete a file from the index and the working tree.
    fn remove(&self, path: &str) {
        self.git(&["rm", "--quiet", "--force", "--", path]);
    }

    fn allowance(&self) -> String {
        fs::read_to_string(self.0.join(ALLOWANCE_FILE)).unwrap()
    }

    /// The ratchet's result and the offending paths it reported.
    fn check(&self, arguments: &[&str]) -> (Result<()>, String) {
        let arguments: Vec<String> = arguments.iter().map(|&argument| argument.into()).collect();
        let mut diagnostics = Vec::new();
        let result = ratchet(&self.0, &arguments, &mut diagnostics);
        (result, String::from_utf8(diagnostics).unwrap())
    }
}

#[test]
fn passes_when_every_tracked_python_file_is_listed_and_the_total_is_within_the_allowance() {
    let repository = Repository::new();
    repository.track("build/tool.py", "import os\nprint(os.name)\n");
    repository.track("ops/files/helper.py", "VALUE = 1\n");
    repository.track("typings/stub.pyi", "def name() -> str: ...\n");
    repository.write(
        ALLOWANCE_FILE,
        "total 3\n2\tbuild/tool.py\n1\tops/files/helper.py\n",
    );
    let (result, diagnostics) = repository.check(&[]);
    result.unwrap();
    assert_eq!(diagnostics, "");
    // Rows and a total above the current counts pass: Python may shrink.
    let looser = "total 5\n3\tbuild/tool.py\n2\tops/files/helper.py\n";
    repository.write(ALLOWANCE_FILE, looser);
    let (result, diagnostics) = repository.check(&[]);
    result.unwrap();
    assert_eq!(diagnostics, "");
    assert_eq!(repository.allowance(), looser);
}

#[test]
fn fails_naming_a_tracked_python_file_absent_from_the_allowance() {
    let repository = Repository::new();
    repository.track("build/tool.py", "VALUE = 1\n");
    repository.track("a/b/c.py", "OTHER = 2\n");
    repository.write(ALLOWANCE_FILE, "total 10\n1\tbuild/tool.py\n");
    let (result, diagnostics) = repository.check(&[]);
    let error = result.unwrap_err().to_string();
    assert_eq!(diagnostics, "a/b/c.py: 1 lines, not in the allowance\n");
    assert!(
        error.starts_with(
            "2 lines in 2 tracked Python files, 1 offending paths; the allowance is 10."
        ),
        "{error}"
    );
    assert!(
        error.ends_with("`cargo run -q -p xtask -- python-lines --update`"),
        "{error}"
    );
}

#[test]
fn fails_when_a_file_grows_even_if_the_total_is_within_the_allowance() {
    let repository = Repository::new();
    repository.track("build/grown.py", "A = 1\nB = 2\nC = 3\n");
    repository.track("build/shrunk.py", "D = 4\n");
    repository.write(
        ALLOWANCE_FILE,
        "total 4\n2\tbuild/grown.py\n2\tbuild/shrunk.py\n",
    );
    let (result, diagnostics) = repository.check(&[]);
    let error = result.unwrap_err().to_string();
    assert_eq!(diagnostics, "build/grown.py: 3 lines, 2 recorded\n");
    assert!(
        error.starts_with(
            "4 lines in 2 tracked Python files, 1 offending paths; the allowance is 4."
        ),
        "{error}"
    );
}

#[test]
fn fails_when_the_total_grows_even_if_every_file_is_listed() {
    let repository = Repository::new();
    repository.track("build/first.py", "A = 1\nB = 2\nC = 3\n");
    repository.track("build/second.py", "D = 4\n");
    // Every row holds, but the recorded total is below their sum.
    repository.write(
        ALLOWANCE_FILE,
        "total 3\n3\tbuild/first.py\n1\tbuild/second.py\n",
    );
    let (result, diagnostics) = repository.check(&[]);
    let error = result.unwrap_err().to_string();
    assert_eq!(diagnostics, "");
    assert!(
        error.starts_with(
            "4 lines in 2 tracked Python files, 0 offending paths; the allowance is 3."
        ),
        "{error}"
    );
}

#[test]
fn fails_when_a_listed_file_is_no_longer_tracked() {
    let repository = Repository::new();
    repository.track("build/removed.py", "A = 1\nB = 2\n");
    repository.track("build/tool.py", "VALUE = 1\n");
    repository.write(
        ALLOWANCE_FILE,
        "total 3\n2\tbuild/removed.py\n1\tbuild/tool.py\n",
    );
    repository.check(&[]).0.unwrap();
    repository.remove("build/removed.py");
    let (result, diagnostics) = repository.check(&[]);
    let error = result.unwrap_err().to_string();
    assert_eq!(diagnostics, "build/removed.py: listed but not tracked\n");
    assert!(
        error.starts_with(
            "1 lines in 1 tracked Python files, 1 offending paths; the allowance is 3."
        ),
        "{error}"
    );
    // `--update` removes the stale row, and the check passes again.
    repository.check(&["--update"]).0.unwrap();
    assert_eq!(repository.allowance(), "total 1\n1\tbuild/tool.py\n");
    repository.check(&[]).0.unwrap();
}

#[test]
fn ignores_vendor_and_untracked_python() {
    let repository = Repository::new();
    repository.track("build/tool.py", "VALUE = 1\n");
    repository.track("vendor/upstream/tasks.py", "A = 1\nB = 2\n");
    repository.write("build/untracked.py", "C = 3\n");
    repository.write(ALLOWANCE_FILE, "total 1\n1\tbuild/tool.py\n");
    let (result, diagnostics) = repository.check(&[]);
    result.unwrap();
    assert_eq!(diagnostics, "");
    repository.check(&["--update"]).0.unwrap();
    assert_eq!(repository.allowance(), "total 1\n1\tbuild/tool.py\n");
}

#[test]
fn update_lowers_the_allowance_and_refuses_to_raise_it() {
    let repository = Repository::new();
    repository.track("ops/second.py", "A = 1\nB = 2\nC = 3\n");
    repository.track("build/first.py", "D = 4\nE = 5\n");
    repository.write(
        ALLOWANCE_FILE,
        "total 6\n3\tbuild/first.py\n3\tops/second.py\n",
    );
    // A shrunk file lowers its row and the total.
    repository.check(&["--update"]).0.unwrap();
    assert_eq!(
        repository.allowance(),
        "total 5\n2\tbuild/first.py\n3\tops/second.py\n"
    );
    // A deleted file loses its row.
    repository.remove("build/first.py");
    repository.check(&["--update"]).0.unwrap();
    assert_eq!(repository.allowance(), "total 3\n3\tops/second.py\n");
    // Growth is refused and reported, and the allowance stays as it was.
    repository.write("ops/second.py", "A = 1\nB = 2\nC = 3\nD = 4\n");
    let (result, diagnostics) = repository.check(&["--update"]);
    let error = result.unwrap_err().to_string();
    assert_eq!(diagnostics, "ops/second.py: 4 lines, 3 recorded\n");
    assert!(
        error.contains("`--update` only removes rows and lowers counts"),
        "{error}"
    );
    assert_eq!(repository.allowance(), "total 3\n3\tops/second.py\n");
    // A recorded total below the rows' sum is not raised either.
    repository.write("ops/second.py", "A = 1\nB = 2\nC = 3\n");
    repository.write(ALLOWANCE_FILE, "total 2\n3\tops/second.py\n");
    let (result, diagnostics) = repository.check(&["--update"]);
    let error = result.unwrap_err().to_string();
    assert_eq!(diagnostics, "");
    assert!(
        error.starts_with(
            "3 lines in 1 tracked Python files, 0 offending paths; the allowance is 2,"
        ),
        "{error}"
    );
    assert_eq!(repository.allowance(), "total 2\n3\tops/second.py\n");
}

#[test]
fn update_refuses_an_unlisted_path_and_a_grown_file_even_when_the_total_falls() {
    let repository = Repository::new();
    repository.track("build/large.py", &"A = 1\n".repeat(10));
    repository.track("build/small.py", "B = 2\nC = 3\n");
    let recorded = "total 12\n10\tbuild/large.py\n2\tbuild/small.py\n";
    repository.write(ALLOWANCE_FILE, recorded);
    // Deleting ten lines does not admit a new one-line file.
    repository.remove("build/large.py");
    repository.track("build/added.py", "D = 4\n");
    let (result, diagnostics) = repository.check(&["--update"]);
    let error = result.unwrap_err().to_string();
    assert_eq!(
        diagnostics,
        "build/added.py: 1 lines, not in the allowance\n"
    );
    assert!(
        error.starts_with(
            "3 lines in 2 tracked Python files, 1 offending paths; the allowance is 12,"
        ),
        "{error}"
    );
    assert_eq!(repository.allowance(), recorded);
    // Nor does it admit a listed file that grew by one line.
    repository.remove("build/added.py");
    repository.write("build/small.py", "B = 2\nC = 3\nE = 5\n");
    let (result, diagnostics) = repository.check(&["--update"]);
    let error = result.unwrap_err().to_string();
    assert_eq!(diagnostics, "build/small.py: 3 lines, 2 recorded\n");
    assert!(
        error.starts_with(
            "3 lines in 1 tracked Python files, 1 offending paths; the allowance is 12,"
        ),
        "{error}"
    );
    assert_eq!(repository.allowance(), recorded);
}

#[test]
fn missing_allowance_is_an_error_for_check_and_update() {
    let repository = Repository::new();
    repository.track("build/tool.py", "VALUE = 1\n");
    let modes: [&[&str]; 2] = [&[], &["--update"]];
    for arguments in modes {
        let (result, diagnostics) = repository.check(arguments);
        let error = result.unwrap_err().to_string();
        assert!(error.contains(ALLOWANCE_FILE), "{arguments:?}: {error}");
        assert_eq!(diagnostics, "");
        assert!(!repository.0.join(ALLOWANCE_FILE).exists());
    }
}

#[test]
fn counts_an_unterminated_last_line_and_crlf_files_by_lines() {
    let repository = Repository::new();
    repository.track("build/blank.py", "\n\n");
    repository.track("build/carriage.py", "A = 1\rB = 2\r");
    repository.track("build/crlf.py", "A = 1\r\nB = 2\r\nC = 3\r\n");
    repository.track("build/empty.py", "");
    repository.track("build/unterminated.py", "A = 1\nB = 2");
    // `--update` lowers each generous row to the file's exact count.
    repository.write(
        ALLOWANCE_FILE,
        "total 45\n9\tbuild/blank.py\n9\tbuild/carriage.py\n9\tbuild/crlf.py\n\
         9\tbuild/empty.py\n9\tbuild/unterminated.py\n",
    );
    repository.check(&["--update"]).0.unwrap();
    assert_eq!(
        repository.allowance(),
        "total 8\n2\tbuild/blank.py\n1\tbuild/carriage.py\n3\tbuild/crlf.py\n\
         0\tbuild/empty.py\n2\tbuild/unterminated.py\n"
    );
}
