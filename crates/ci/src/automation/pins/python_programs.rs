//! No tracked file outside `vendor/` holds a Python program of this
//! repository. Python stays only where an approved exception needs it:
//! Ansible (its controller environment and the interpreter it runs on
//! managed hosts), the scanner environment the security gate installs, and
//! the vendored worker build. Recordings and frozen references may quote
//! Python as data; each one is listed below, proven to be data, and held to
//! its exact number of quoting lines.

use regex::bytes::Regex;
use sha2::{Digest, Sha256};
use simplestchat_toolkit::test_env::helper_with_signals;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// One way a file can hold or start a Python program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Form {
    /// A `.py` or `.pyi` file.
    Source,
    /// A `#!` line whose interpreter is Python.
    Shebang,
    /// An interpreter handed its program on the command line (`-c`) or on
    /// its standard input (`-`).
    Inline,
    /// A heredoc fed to an interpreter: a `PY…` delimiter, or an
    /// interpreter word before `<<`.
    Heredoc,
    /// A quoted interpreter name or path, as a `Command` or a child-process
    /// call starts it.
    Command,
}

/// Why an allowlisted file may match, and how the guard proves it.
#[derive(Clone, Copy, Debug)]
enum Kind {
    /// Code or configuration of an approved exception, or a test that
    /// names Python only to refuse it.
    Exception,
    /// A JSON recording: it must parse as JSON and must not be executable,
    /// so its Python can only be quoted text.
    Recording,
    /// A frozen reference read as bytes: it must not be executable and must
    /// keep the recorded SHA-256, so no line can be added to it.
    Frozen(&'static str),
}

/// A file that may match `form` on exactly `lines` lines, and why.
#[derive(Clone, Copy, Debug)]
struct Allowed {
    path: &'static str,
    form: Form,
    lines: usize,
    kind: Kind,
    reason: &'static str,
}

const ANSIBLE: &str = "Ansible's interpreter on managed hosts, as the plays and their pins name it";
const CONTROLLER: &str =
    "the Ansible controller environment, built from ops/ansible/requirements.txt";
const REFUSED: &str = "a test that names Python only as a refused or absent form";
const SCANNERS: &str = "the security gate's scanner environment and its scripted interpreter";
const VENDORED: &str = "the vendored worker build's interpreter, which runs its Meson and Ninja";
const RECORDED: &str = "a recording of answers that quotes Python commands as data";
const FROZEN: &str = "a frozen reference script, read only for its bytes and extracted filters";

/// Every file that may match, ordered by path, with its reason. Nothing
/// else may match, and every entry must still match exactly as often as
/// recorded.
const ALLOWED: &[Allowed] = &[
    Allowed {
        path: ".github/workflows/infrastructure-security.yml",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: CONTROLLER,
    },
    Allowed {
        path: ".github/workflows/maintenance-host-runtime.yml",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: CONTROLLER,
    },
    Allowed {
        path: ".github/workflows/maintenance-launcher-runtime.yml",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: CONTROLLER,
    },
    Allowed {
        path: ".github/workflows/release-deploy-runtime.yml",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: CONTROLLER,
    },
    Allowed {
        path: "crates/ci/src/automation/maintenance_caller_tests.rs",
        form: Form::Command,
        lines: 2,
        kind: Kind::Exception,
        reason: ANSIBLE,
    },
    Allowed {
        path: "crates/ci/src/automation/native_verification_caller_tests.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: REFUSED,
    },
    Allowed {
        path: "crates/ci/src/automation/pins/force_release.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: ANSIBLE,
    },
    Allowed {
        path: "crates/ci/src/automation/pins/security_ci/codeql.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: REFUSED,
    },
    Allowed {
        path: "crates/ci/src/automation/routine_caller_tests.rs",
        form: Form::Command,
        lines: 4,
        kind: Kind::Exception,
        reason: ANSIBLE,
    },
    Allowed {
        path: "crates/ci/src/command.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: VENDORED,
    },
    Allowed {
        path: "crates/ci/src/container_benchmark/reference/build/benchmark-container.sh",
        form: Form::Inline,
        lines: 1,
        kind: Kind::Frozen("a329d731b73630b5823e4a2d626178858b1adc305e1be2972b3d8b8b90edf5de"),
        reason: FROZEN,
    },
    Allowed {
        path: "crates/ci/src/container_benchmark/reference/build/benchmark-container.sh",
        form: Form::Heredoc,
        lines: 1,
        kind: Kind::Frozen("a329d731b73630b5823e4a2d626178858b1adc305e1be2972b3d8b8b90edf5de"),
        reason: FROZEN,
    },
    Allowed {
        path: "crates/ci/src/native_dtls/execute_tests.rs",
        form: Form::Command,
        lines: 4,
        kind: Kind::Exception,
        reason: VENDORED,
    },
    Allowed {
        path: "crates/ci/src/native_dtls/files_tests.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: VENDORED,
    },
    Allowed {
        path: "crates/ci/src/native_dtls/plan.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: VENDORED,
    },
    Allowed {
        path: "crates/ci/src/release_controller_plan.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: CONTROLLER,
    },
    Allowed {
        path: "crates/ci/src/release_controller_tests.rs",
        form: Form::Command,
        lines: 2,
        kind: Kind::Exception,
        reason: CONTROLLER,
    },
    Allowed {
        path: "crates/ci/src/release_controller_wait.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: CONTROLLER,
    },
    Allowed {
        path: "crates/release/src/ops_artifact/maintain_runtime/deploy_workflow_tests.rs",
        form: Form::Inline,
        lines: 2,
        kind: Kind::Exception,
        reason: REFUSED,
    },
    Allowed {
        path: "crates/release/src/ops_artifact/maintain_runtime/deploy_workflow_tests.rs",
        form: Form::Heredoc,
        lines: 1,
        kind: Kind::Exception,
        reason: REFUSED,
    },
    Allowed {
        path: "crates/release/src/ops_artifact/maintain_runtime/deploy_workflow_tests.rs",
        form: Form::Command,
        lines: 4,
        kind: Kind::Exception,
        reason: REFUSED,
    },
    Allowed {
        path: "crates/release/src/ops_artifact/maintain_runtime/execute.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: ANSIBLE,
    },
    Allowed {
        path: "crates/release/src/ops_artifact/maintain_runtime/infrastructure.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: ANSIBLE,
    },
    Allowed {
        path: "crates/release/src/ops_artifact/maintain_runtime/recorded_infrastructure.json",
        form: Form::Command,
        lines: 3,
        kind: Kind::Recording,
        reason: RECORDED,
    },
    Allowed {
        path: "crates/security/src/actions/tests.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: SCANNERS,
    },
    Allowed {
        path: "crates/security/src/codeql/recorded_local_main.json",
        form: Form::Command,
        lines: 58,
        kind: Kind::Recording,
        reason: RECORDED,
    },
    Allowed {
        path: "crates/security/src/environment.rs",
        form: Form::Command,
        lines: 2,
        kind: Kind::Exception,
        reason: SCANNERS,
    },
    Allowed {
        path: "crates/security/src/environment/recorded_environment.json",
        form: Form::Command,
        lines: 25,
        kind: Kind::Recording,
        reason: RECORDED,
    },
    Allowed {
        path: "crates/security/src/environment/tests.rs",
        form: Form::Command,
        lines: 4,
        kind: Kind::Exception,
        reason: SCANNERS,
    },
    Allowed {
        path: "crates/security/src/fast/tests.rs",
        form: Form::Command,
        lines: 2,
        kind: Kind::Exception,
        reason: SCANNERS,
    },
    Allowed {
        path: "crates/security/src/licenses/registry_tests.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: SCANNERS,
    },
    Allowed {
        path: "crates/security/src/licenses/review_tests.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: SCANNERS,
    },
    Allowed {
        path: "crates/security/src/native_suites/recorded_suites.json",
        form: Form::Command,
        lines: 2,
        kind: Kind::Recording,
        reason: RECORDED,
    },
    Allowed {
        path: "crates/security/src/native_suites/tests.rs",
        form: Form::Inline,
        lines: 1,
        kind: Kind::Exception,
        reason: REFUSED,
    },
    Allowed {
        path: "crates/security/src/native_suites/tests.rs",
        form: Form::Heredoc,
        lines: 1,
        kind: Kind::Exception,
        reason: REFUSED,
    },
    Allowed {
        path: "crates/security/src/openssl/tests.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: SCANNERS,
    },
    Allowed {
        path: "crates/security/src/tier.rs",
        form: Form::Command,
        lines: 1,
        kind: Kind::Exception,
        reason: SCANNERS,
    },
];

/// The pattern of one line-based form.
fn pattern(form: Form) -> Regex {
    // The interpreter's name, optionally versioned; the start of a command
    // word, optionally a path; and a launcher path before the name, through
    // `env` and its options.
    let name = r"python(?:[0-9]+(?:\.[0-9]+)?)?";
    let word = r#"(?:^|[^A-Za-z0-9_.-])(?:[^\s'"]*/)?"#;
    let launcher = r#"(?:/[^\s'"]*/)?(?:env[ \t]+(?:-[^\s'"]*[ \t]+)*)?"#;
    let text = match form {
        Form::Source => unreachable!("a source file is recognized by its name"),
        Form::Shebang => format!(r"#![ \t]*{launcher}{name}(?:[^A-Za-z0-9_.-]|$)"),
        Form::Inline => {
            format!(r#"{word}{name}(?:[ \t]+-[A-Za-z]+)*?[ \t]+-(?:[A-Za-z]*c)?(?:[ \t'"\\]|$)"#)
        }
        Form::Heredoc => {
            format!(r#"<<-?[ \t]*\\?['"]?PY[A-Za-z0-9_]*|{word}{name}(?:[^A-Za-z0-9_.-][^\n]*)?<<"#)
        }
        Form::Command => format!(
            concat!(
                r#"['"](?:[^'"\s]*/)?python[0-9]+(?:\.[0-9]+)?['"]"#,
                r#"|['"][^'"\s]*/python['"]"#,
                r#"|(?:Command|Process)::new\(\s*['"`]{name}['"`]"#,
                r#"|\b(?:spawn|spawnSync|execFile|execFileSync|execSync)\(\s*['"`]{name}['"`]"#
            ),
            name = name
        ),
    };
    Regex::new(&format!("(?m){text}")).unwrap()
}

/// Each line-based form with its pattern.
fn patterns() -> Vec<(Form, Regex)> {
    [Form::Shebang, Form::Inline, Form::Heredoc, Form::Command]
        .into_iter()
        .map(|form| (form, pattern(form)))
        .collect()
}

/// How many lines of `bytes` match each of `patterns`.
fn count(bytes: &[u8], patterns: &[(Form, Regex)]) -> BTreeMap<Form, usize> {
    let mut found = BTreeMap::new();
    for (form, pattern) in patterns {
        let lines = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| pattern.is_match(line))
            .count();
        if lines > 0 {
            found.insert(*form, lines);
        }
    }
    found
}

/// Whether `path` names a Python source file.
fn source(path: &str) -> bool {
    path.ends_with(".py") || path.ends_with(".pyi")
}

/// Every tracked file outside `vendor/`, as its index mode and path.
async fn tracked(root: &Path) -> Vec<(String, String)> {
    let mut git = std::process::Command::new("git");
    git.args(["ls-files", "--stage", "-z"])
        .current_dir(root)
        .stderr(std::process::Stdio::null());
    let (status, bytes) =
        helper_with_signals(git, Duration::from_secs(30), Some(16 * 1024 * 1024), None)
            .await
            .unwrap();
    assert_eq!(status, 0, "git ls-files must list the checkout");
    let mut files = Vec::new();
    for entry in bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry = std::str::from_utf8(entry).unwrap();
        let (meta, path) = entry.split_once('\t').unwrap();
        let mode = meta.split(' ').next().unwrap();
        if !path.starts_with("vendor/") {
            files.push((mode.to_owned(), path.to_owned()));
        }
    }
    files
}

/// Every form found in each tracked file, keyed by path and form, with the
/// matching line count and the file's index mode.
async fn findings(root: &Path) -> BTreeMap<(String, Form), (usize, String)> {
    let patterns = patterns();
    let mut found = BTreeMap::new();
    for (mode, path) in tracked(root).await {
        if source(&path) {
            found.insert((path.clone(), Form::Source), (1, mode.clone()));
        }
        let bytes = std::fs::read(root.join(&path)).unwrap();
        for (form, lines) in count(&bytes, &patterns) {
            found.insert((path.clone(), form), (lines, mode.clone()));
        }
    }
    found
}

/// Prove an allowlisted file is what its kind claims.
fn prove(root: &Path, allowed: &Allowed, mode: &str) {
    let path = allowed.path;
    match allowed.kind {
        Kind::Exception => {}
        Kind::Recording => {
            assert_eq!(mode, "100644", "{path} must not be executable");
            assert!(path.ends_with(".json"), "{path} must be a JSON recording");
            let bytes = std::fs::read(root.join(path)).unwrap();
            assert!(
                serde_json::from_slice::<serde_json::Value>(&bytes).is_ok(),
                "{path} must parse as JSON"
            );
        }
        Kind::Frozen(digest) => {
            assert_eq!(mode, "100644", "{path} must not be executable");
            let bytes = std::fs::read(root.join(path)).unwrap();
            let actual = Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(actual, digest, "{path} must keep its frozen bytes");
        }
    }
}

/// The guard: what the checkout holds is exactly what the allowlist admits,
/// and each recording and frozen reference is proven to be data.
#[tokio::test]
async fn no_tracked_file_holds_a_python_program_of_the_repository() {
    let root = super::ansible::root();
    let found = findings(&root).await;
    let mut refused = Vec::new();
    for ((path, form), (lines, mode)) in &found {
        let entry = ALLOWED
            .iter()
            .find(|allowed| allowed.path == path && allowed.form == *form);
        match entry {
            Some(allowed) if allowed.lines == *lines => prove(&root, allowed, mode),
            _ => refused.push(format!("{path}: {form:?} on {lines} line(s)")),
        }
    }
    let stale = ALLOWED
        .iter()
        .filter(|allowed| {
            found
                .get(&(allowed.path.to_owned(), allowed.form))
                .is_none_or(|(lines, _)| *lines != allowed.lines)
        })
        .map(|allowed| format!("{}: {:?}", allowed.path, allowed.form))
        .collect::<Vec<_>>();
    assert!(
        refused.is_empty() && stale.is_empty(),
        "Python programs outside the approved exceptions: {refused:#?}; \
         allowlist entries that no longer match exactly: {stale:#?}"
    );
}

/// Each allowlist entry names one file and form once, with a reason, in
/// path order, so a review sees every exception in one place.
#[test]
fn the_allowlist_is_ordered_unique_and_reasoned() {
    for pair in ALLOWED.windows(2) {
        assert!(
            (pair[0].path, pair[0].form) < (pair[1].path, pair[1].form),
            "{} {:?}",
            pair[1].path,
            pair[1].form
        );
    }
    for allowed in ALLOWED {
        assert!(
            allowed.lines > 0 && !allowed.reason.is_empty(),
            "{}",
            allowed.path
        );
        assert!(!allowed.path.starts_with("vendor/"), "{}", allowed.path);
    }
}

/// `text` with `{p}` spelled as the interpreter's name and `{d}` as the
/// heredoc delimiter prefix, so this file holds none of the forms itself.
fn spelled(text: &str) -> Vec<u8> {
    text.replace("{p}", "python")
        .replace("{d}", "PY")
        .into_bytes()
}

/// Every form finds each of its programs on its own line, and no form finds
/// the Python that stays: package names, module runs, versions, languages
/// and the Ansible interpreter setting.
#[test]
fn each_form_finds_its_programs_and_only_those() {
    let cases: [(Form, &[&str]); 4] = [
        (
            Form::Shebang,
            &[
                "#!/usr/bin/{p}3",
                "#!/usr/bin/env {p}3",
                "#!/usr/bin/env -S {p}3 -I",
                "#! /opt/tools/{p}",
                r##"fs::write(&path, "#!/usr/bin/{p}3\nimport os\n")"##,
            ],
        ),
        (
            Form::Inline,
            &[
                "{p}3 -c 'import os'",
                "{p} -c pass",
                "/usr/bin/{p}3 -I -c 'import os'",
                "{p}3.14 -Ic pass",
                "run: {p}3 -",
                r#"format!("{script}{p}3 -c 'import os'\n")"#,
            ],
        ),
        (
            Form::Heredoc,
            &[
                "cat <<'{d}'",
                "sh -s <<{d}_PROBE",
                r#"<<-\"{d}\""#,
                "{p}3 <<EOF",
            ],
        ),
        (
            Form::Command,
            &[
                r#"Command::new("{p}3")"#,
                r#"Command::new("/usr/bin/{p}3")"#,
                r#"Process::new("{p}")"#,
                "spawnSync('{p}3', ['-c', program])",
                r#"spawn("{p}", [])"#,
                r#"vec!["/usr/bin/{p}3".into(), "-c".into()]"#,
                r#"directory.join("{p}3")"#,
                r#"venv.join("bin/{p}")"#,
            ],
        ),
    ];
    let patterns = patterns();
    for (form, programs) in cases {
        for program in programs {
            let found = count(&spelled(program), &patterns);
            assert!(found.contains_key(&form), "{form:?}: {program}");
        }
    }
    for kept in [
        r#"{p}3 -m venv "$RUNNER_TEMP/ansible-controller""#,
        "{p}3 -m pip install --requirement constraints.txt",
        "{p}3 -m invoke --search-root vendor/worker setup",
        "{p}3 --version",
        "ansible_{p}_interpreter: /usr/bin/{p}3",
        "uses: actions/setup-{p}@v6",
        "sudo apt-get install -y {p}3-pip {p}3-venv",
        r#"for language in ["{p}", "rust"] {"#,
        "the `{p}3` package the host needs",
        "export PYTHONPATH=/opt/tools",
        "cat <<'EOF'",
    ] {
        assert!(count(&spelled(kept), &patterns).is_empty(), "{kept}");
    }
}

/// A source file is recognized by its name alone.
#[test]
fn source_files_are_recognized_by_their_names() {
    for path in ["build/tool.py", "ops/ansible/stubs/loader.pyi"] {
        assert!(source(path), "{path}");
    }
    for path in ["build/tool.pyc", "build/tool.py.txt", "docs/python.md"] {
        assert!(!source(path), "{path}");
    }
}
