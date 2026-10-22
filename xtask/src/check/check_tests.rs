//! Behavioral gates run against owned checkouts and inert executable tools.

use super::*;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn bootstrap_linux_graph_excludes_native_tls_until_the_client_feature_is_selected() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let graph = |features: &[&str]| {
        let output = std::process::Command::new(env!("CARGO"))
            .args([
                "tree",
                "--locked",
                "--offline",
                "-p",
                "xtask",
                "--target",
                "x86_64-unknown-linux-gnu",
                "--edges",
                "normal,build",
                "--prefix",
                "none",
            ])
            .args(features)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let bootstrap = graph(&[]);
    for dependency in ["native-tls v", "openssl v", "openssl-sys v", "aws-lc-sys v"] {
        assert!(
            !bootstrap.lines().any(|line| line.starts_with(dependency)),
            "bootstrap unexpectedly requires {dependency}"
        );
    }
    let client = graph(&["--features", "public-smoke"]);
    assert!(client.lines().any(|line| line.starts_with("openssl-sys v")));
}

const ENVIRONMENT_KEYS: &[&str] = &[
    "PATH",
    "RUSTC",
    "RUSTDOC",
    "OPENSSL_DIR",
    "OPENSSL_STATIC",
    "PKG_CONFIG_PATH",
    "PIP_CONSTRAINT",
    "RUSTDOCFLAGS",
    "PYTHON_CHECK_ENV",
    "AWS_LC_SYS_SYSTEM_DIR",
    "AWS_LC_SYS_STATIC",
    "AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK",
];
const SOURCES: &[&str] = &[
    "build/tracked.py",
    "build/new helper.py",
    "build/nested/untracked.py",
    "ops/ansible/files/host.py",
    "ops/ansible/callback_plugins/observer.py",
    "ops/ansible/filter_plugins/filter.py",
    "ops/ansible/tests/test_new\ncase.py",
    "typings/fixture/nested.pyi",
];
const DISPATCH: &str = r#"
{
    printf '%s\000' "$command" "$PWD" "$#" "$@"
    printf '%s\000' "$PATH" "${RUSTC-}" "${RUSTDOC-}" "${OPENSSL_DIR-}" \
        "${OPENSSL_STATIC-}" "${PKG_CONFIG_PATH-}" "${PIP_CONSTRAINT-}" \
        "${RUSTDOCFLAGS-}" "${PYTHON_CHECK_ENV-}" "${AWS_LC_SYS_SYSTEM_DIR-}" \
        "${AWS_LC_SYS_STATIC-}" "${AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK-}"
} >> "$FIXTURE_LOG"
case "$command" in
    npm) if [ "$3" = run ]; then phase="npm-$4"; else phase="npm-$3"; fi ;;
    rustup)
        if [ "$1" = which ]; then
            phase="rustup-which-$4"
        elif [ "$4" = run ]; then
            phase="cargo-${10}"
        else phase="cargo-$4"; fi ;;
    sh|bash) phase="$command:$2" ;;
    node)
        if [ "${2-}" = --test-concurrency=1 ]; then phase=serial-helper-tests
        else phase=helper-tests; fi ;;
    shellcheck) phase=shellcheck ;;
    ruff) if [ "$1" = check ]; then phase=python-lint; else phase=python-format; fi ;;
    basedpyright) phase=python-types ;;
    *) exit 99 ;;
esac
if [ "${FIXTURE_FAIL-}" = "$phase" ]; then exit "${FIXTURE_FAILURE_CODE-43}"; fi
if [ "${FIXTURE_SIGNAL-}" = "$phase" ]; then kill -TERM $$; fi
if [ "$command" = rustup ] && [ "$1" = which ]; then
    printf '%s/%s\n' "$FIXTURE_TOOLCHAIN_BIN" "$4"
fi
"#;

#[derive(Debug)]
struct Event {
    command: String,
    cwd: String,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
}

use std::collections::BTreeMap;

struct Fixture {
    temporary: PathBuf,
    root: PathBuf,
    bin: PathBuf,
    toolchain: PathBuf,
    log: PathBuf,
    environment: Environment,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temporary);
    }
}

impl Fixture {
    fn new() -> Self {
        let temporary = std::env::temp_dir().join(format!(
            "simplestchat-check-rust-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&temporary).unwrap();
        let temporary = fs::canonicalize(temporary).unwrap();
        let root = temporary.join("checkout with spaces");
        let bin = temporary.join("fake commands");
        let toolchain = root.join("pinned toolchain/bin");
        for directory in [
            &bin,
            &toolchain,
            &root.join("web/node_modules"),
            &root.join("build/tests"),
            &root.join("load_tests"),
            &root.join(".githooks"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        let log = temporary.join("commands.nul");
        let environment = [
            ("PATH", bin.as_os_str()),
            ("FIXTURE_LOG", log.as_os_str()),
            ("FIXTURE_TOOLCHAIN_BIN", toolchain.as_os_str()),
            ("LC_ALL", OsStr::new("C")),
            ("GIT_CONFIG_NOSYSTEM", OsStr::new("1")),
            ("GIT_CONFIG_GLOBAL", OsStr::new("/dev/null")),
            ("GIT_TERMINAL_PROMPT", OsStr::new("0")),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.to_owned()))
        .collect();
        let fixture = Self {
            temporary,
            root,
            bin,
            toolchain,
            log,
            environment,
        };
        fixture.write("rust-toolchain.toml", "[toolchain]\nchannel = \"9.88.7\"\n");
        fixture.write("build/pip-constraints.txt", "# fixture\n");
        for name in [
            "build/helper with spaces.sh",
            ".githooks/commit-msg",
            "build/tests/first.test.mjs",
            "build/tests/second.test.mjs",
            "load_tests/benchmark-local.test.mjs",
            "load_tests/report-command.test.mjs",
        ] {
            fixture.write(name, "never execute this fixture\n");
        }
        fixture.install_native(&fixture.root.join("target/openssl-4.0.3"), false);
        fixture.install_native(&fixture.root.join("target/aws-lc-5.11.0"), true);
        for name in ["npm", "rustup", "sh", "bash", "shellcheck", "node"] {
            fixture.executable(&fixture.bin.join(name), name);
        }
        for name in ["python", "ruff", "basedpyright"] {
            fixture.executable(&fixture.root.join("ops/ansible/.venv/bin").join(name), name);
        }
        symlink("/usr/bin/git", fixture.bin.join("git")).unwrap();
        fixture.write(".gitignore", "private/\nresults/\nops/ansible/.venv/\n");
        for name in SOURCES.iter().copied().chain([
            "private/secret.py",
            "results/generated.py",
            "vendor/upstream.py",
            "reference/example.py",
        ]) {
            fixture.write(name, "# inert source\n");
        }
        tooling::run(
            &fixture.root,
            &fixture.environment,
            "git",
            ["init", "--quiet"],
        )
        .unwrap();
        tooling::run(
            &fixture.root,
            &fixture.environment,
            "git",
            [
                "add",
                "--",
                "build/tracked.py",
                "vendor/upstream.py",
                "reference/example.py",
            ],
        )
        .unwrap();
        fixture
    }

    fn write(&self, name: &str, contents: &str) {
        let path = self.root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn executable(&self, path: &Path, name: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("#!/bin/sh\ncommand='{name}'\n{DISPATCH}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn install_native(&self, prefix: &Path, aws: bool) {
        let names = if aws {
            ["lib/libcrypto-awslc.a", "share/rust/aws_lc_bindings.rs"]
        } else {
            ["lib/libssl.a", "lib/libcrypto.a"]
        };
        for name in names {
            let file = prefix.join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "fixture\n").unwrap();
        }
    }

    fn run(&self, arguments: &[&str], overrides: &[(&str, OsString)]) -> tooling::Result<()> {
        fs::write(&self.log, []).unwrap();
        let mut environment = self.environment.clone();
        environment.extend(
            overrides
                .iter()
                .map(|(name, value)| ((*name).into(), value.clone())),
        );
        run_with_env(
            &self.root,
            &arguments
                .iter()
                .map(|argument| (*argument).into())
                .collect::<Vec<_>>(),
            environment,
        )
    }

    fn events(&self) -> Vec<Event> {
        let data = fs::read(&self.log).unwrap();
        // Empty environment values are meaningful; only remove the final NUL.
        let mut fields = data
            .strip_suffix(&[0])
            .unwrap_or(&data)
            .split(|byte| *byte == 0);
        let mut events = Vec::new();
        while let Some(command) = fields.next().filter(|field| !field.is_empty()) {
            let text = |field: &[u8]| String::from_utf8(field.to_vec()).unwrap();
            let cwd = text(fields.next().unwrap());
            let count = text(fields.next().unwrap()).parse::<usize>().unwrap();
            let arguments = fields.by_ref().take(count).map(text).collect();
            let environment = ENVIRONMENT_KEYS
                .iter()
                .map(|key| ((*key).to_owned(), text(fields.next().unwrap())))
                .collect();
            events.push(Event {
                command: text(command),
                cwd,
                arguments,
                environment,
            });
        }
        events
    }
}

fn phases(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event.command.as_str() {
            "npm" => format!(
                "npm-{}",
                event.arguments[if event.arguments[2] == "run" { 3 } else { 2 }]
            ),
            "rustup" if event.arguments[0] == "which" => {
                format!("rustup-which-{}", event.arguments[3])
            }
            "rustup" if event.arguments[3] == "run" => format!("cargo-{}", event.arguments[9]),
            "rustup" => format!("cargo-{}", event.arguments[3]),
            "sh" | "bash" => format!("{}:{}", event.command, event.arguments[1]),
            "ruff" => format!(
                "python-{}",
                if event.arguments[0] == "check" {
                    "lint"
                } else {
                    "format"
                }
            ),
            "basedpyright" => "python-types".to_owned(),
            "node" => "helper-tests".to_owned(),
            _ => event.command.clone(),
        })
        .collect()
}

#[test]
fn help_and_invalid_arguments_execute_no_tools() {
    let fixture = Fixture::new();
    assert!(USAGE.contains("--web | --rust | --python | --helpers"));
    assert!(USAGE.contains("Default: run all four groups"));
    for argument in ["--help", "-h"] {
        fixture.run(&[argument], &[]).unwrap();
        assert!(fixture.events().is_empty());
    }
    for arguments in [
        vec!["--unknown"],
        vec!["web"],
        vec!["--web", "--rust"],
        vec!["--python", "--helpers"],
        vec!["--help", "extra"],
    ] {
        let error = fixture.run(&arguments, &[]).unwrap_err();
        assert_eq!(error.code, 2);
        assert!(error.message.contains("Usage:"));
        assert!(fixture.events().is_empty());
    }
}

#[test]
fn web_runs_only_lint_format_tests_and_build_from_checkout() {
    let fixture = Fixture::new();
    fixture.run(&["--web"], &[]).unwrap();
    let events = fixture.events();
    assert_eq!(
        phases(&events),
        ["npm-lint", "npm-format:check", "npm-test", "npm-build"]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.arguments.clone())
            .collect::<Vec<_>>(),
        [
            vec!["--prefix", "web", "run", "lint"],
            vec!["--prefix", "web", "run", "format:check"],
            vec!["--prefix", "web", "test"],
            vec!["--prefix", "web", "run", "build"],
        ]
    );
    assert!(
        events
            .iter()
            .all(|event| Path::new(&event.cwd) == fixture.root)
    );
}

#[test]
fn tool_environment_pwd_matches_actual_checkout_from_other_working_directories() {
    let fixture = Fixture::new();
    let environment = [(
        OsString::from("PWD"),
        OsString::from("/wrong/working/directory"),
    )]
    .into_iter()
    .collect();
    let output =
        tooling::output(&fixture.root, &environment, "/usr/bin/env", [] as [&str; 0]).unwrap();
    assert_eq!(
        output,
        format!("PWD={}\n", fixture.root.display()).as_bytes()
    );
}

#[test]
fn rust_pins_native_inputs_all_target_clippy_and_warning_free_private_docs() {
    let fixture = Fixture::new();
    fixture
        .run(
            &["--rust"],
            &[
                ("RUSTC", "/wrong/rustc".into()),
                ("RUSTDOC", "/wrong/rustdoc".into()),
                ("RUSTUP_TOOLCHAIN", "stable".into()),
                ("PKG_CONFIG_PATH", "/wrong/pkgconfig".into()),
                ("OPENSSL_STATIC", "0".into()),
                ("PIP_CONSTRAINT", "/wrong/pip".into()),
                ("RUSTDOCFLAGS", "--cfg quality_fixture".into()),
                ("AWS_LC_SYS_STATIC", "0".into()),
                ("AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK", "1".into()),
            ],
        )
        .unwrap();
    let events = fixture.events();
    assert_eq!(
        phases(&events),
        [
            "rustup-which-rustc",
            "rustup-which-rustdoc",
            "cargo-fmt",
            "cargo-clippy",
            "cargo-line-length",
            "cargo-protocol-ts",
            "cargo-http-policy",
            "cargo-doc"
        ]
    );
    assert_eq!(
        events[0].arguments,
        ["which", "--toolchain", "9.88.7", "rustc"]
    );
    assert_eq!(
        events[1].arguments,
        ["which", "--toolchain", "9.88.7", "rustdoc"]
    );
    assert_eq!(
        events[2].arguments,
        ["run", "9.88.7", "cargo", "fmt", "--all", "--", "--check"]
    );
    assert_eq!(
        events[3].arguments,
        [
            "run",
            "9.88.7",
            "cargo",
            "clippy",
            "--locked",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--no-deps",
            "--",
            "-D",
            "warnings"
        ]
    );
    for (event, checker) in events[4..7]
        .iter()
        .zip(["line-length", "protocol-ts", "http-policy"])
    {
        let mut expected = vec![
            "run", "9.88.7", "cargo", "run", "--locked", "-q", "-p", "xtask", "--", checker,
        ];
        if checker != "line-length" {
            expected.push("--check");
        }
        assert_eq!(event.arguments, expected);
    }
    assert!(
        events
            .iter()
            .all(|event| { event.command == "rustup" && Path::new(&event.cwd) == fixture.root })
    );
    assert_eq!(
        events[7].arguments,
        [
            "run",
            "9.88.7",
            "cargo",
            "doc",
            "--locked",
            "--workspace",
            "--all-features",
            "--no-deps",
            "--document-private-items"
        ]
    );
    for event in &events[2..] {
        let environment = &event.environment;
        assert_eq!(
            environment["RUSTC"],
            fixture.toolchain.join("rustc").to_str().unwrap()
        );
        assert_eq!(
            environment["RUSTDOC"],
            fixture.toolchain.join("rustdoc").to_str().unwrap()
        );
        assert_eq!(
            environment["PATH"],
            format!("{}:{}", fixture.toolchain.display(), fixture.bin.display())
        );
        assert_eq!(
            environment["OPENSSL_DIR"],
            fixture.root.join("target/openssl-4.0.3").to_str().unwrap()
        );
        assert_eq!(
            environment["PKG_CONFIG_PATH"],
            fixture
                .root
                .join("target/openssl-4.0.3/lib/pkgconfig")
                .to_str()
                .unwrap()
        );
        assert_eq!(
            environment["PIP_CONSTRAINT"],
            fixture
                .root
                .join("build/pip-constraints.txt")
                .to_str()
                .unwrap()
        );
        assert_eq!(
            environment["AWS_LC_SYS_SYSTEM_DIR"],
            fixture.root.join("target/aws-lc-5.11.0").to_str().unwrap()
        );
        assert_eq!(environment["OPENSSL_STATIC"], "1");
        assert_eq!(environment["AWS_LC_SYS_STATIC"], "1");
        assert_eq!(environment["AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK"], "0");
    }
    assert_eq!(
        events[7].environment["RUSTDOCFLAGS"],
        "--cfg quality_fixture -D warnings"
    );
}

#[test]
fn native_prefix_overrides_with_spaces_are_respected() {
    let fixture = Fixture::new();
    let openssl = fixture.root.join("custom static OpenSSL");
    let aws = fixture.root.join("custom AWS-LC");
    fixture.install_native(&openssl, false);
    fixture.install_native(&aws, true);
    fixture
        .run(
            &["--rust"],
            &[
                ("OPENSSL_DIR", openssl.clone().into_os_string()),
                ("AWS_LC_SYS_SYSTEM_DIR", aws.clone().into_os_string()),
            ],
        )
        .unwrap();
    for event in &fixture.events()[2..] {
        assert_eq!(event.environment["OPENSSL_DIR"], openssl.to_str().unwrap());
        assert_eq!(
            event.environment["PKG_CONFIG_PATH"],
            openssl.join("lib/pkgconfig").to_str().unwrap()
        );
        assert_eq!(
            event.environment["AWS_LC_SYS_SYSTEM_DIR"],
            aws.to_str().unwrap()
        );
    }
}

#[test]
fn relative_native_prefixes_are_resolved_from_the_owned_checkout() {
    let fixture = Fixture::new();
    fixture
        .run(
            &["--rust"],
            &[
                ("OPENSSL_DIR", "target/openssl-4.0.3".into()),
                ("AWS_LC_SYS_SYSTEM_DIR", "target/aws-lc-5.11.0".into()),
            ],
        )
        .unwrap();
    assert_eq!(
        fixture.events()[2].environment["OPENSSL_DIR"],
        "target/openssl-4.0.3"
    );
}

#[test]
fn helper_checks_each_script_individually_and_expands_test_paths() {
    let fixture = Fixture::new();
    fixture.run(&["--helpers"], &[]).unwrap();
    let events = fixture.events();
    assert_eq!(
        phases(&events),
        [
            "sh:.githooks/commit-msg",
            "bash:build/helper with spaces.sh",
            "shellcheck",
            "helper-tests"
        ]
    );
    assert!(
        events[..2]
            .iter()
            .all(|event| event.arguments.len() == 2 && event.arguments[0] == "-n")
    );
    assert_eq!(
        events[2].arguments,
        ["build/helper with spaces.sh", ".githooks/commit-msg"]
    );
    assert_eq!(
        events[3].arguments,
        [
            "--test",
            "build/tests/first.test.mjs",
            "build/tests/second.test.mjs",
            "load_tests/benchmark-local.test.mjs",
            "load_tests/report-command.test.mjs"
        ]
    );
    assert!(
        events
            .iter()
            .all(|event| Path::new(&event.cwd) == fixture.root)
    );
}

#[test]
fn helper_test_filenames_with_spaces_stay_one_argument() {
    let fixture = Fixture::new();
    fixture.write(
        "build/tests/test with spaces.test.mjs",
        "never execute this fixture\n",
    );
    fixture.run(&["--helpers"], &[]).unwrap();
    assert_eq!(
        fixture.events().last().unwrap().arguments,
        [
            "--test",
            "build/tests/first.test.mjs",
            "build/tests/second.test.mjs",
            "build/tests/test with spaces.test.mjs",
            "load_tests/benchmark-local.test.mjs",
            "load_tests/report-command.test.mjs"
        ]
    );
}

#[test]
fn shared_xtask_inventory_tracks_the_actual_cargo_consumers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    // This is a conservative maintained-source policy, not a JavaScript parser.
    // Inspect literal Cargo commands independently of their imported API name,
    // quoting or line layout. Computed executable names require policy review.
    let cargo = regex::Regex::new(r#"['"`]cargo(?:['"`]|\s)"#).unwrap();
    let mut consumers = fs::read_dir(root.join("build/tests"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".test.mjs"))
        .chain([
            root.join("load_tests/benchmark-local.test.mjs"),
            root.join("load_tests/report-command.test.mjs"),
        ])
        .filter(|path| {
            let source = fs::read_to_string(path).unwrap();
            cargo.is_match(&source) && source.contains("xtask")
        })
        .map(|path| {
            path.strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    consumers.sort();
    assert_eq!(consumers, SHARED_XTASK_TESTS);
}

#[test]
fn helper_pools_preserve_each_test_once_and_stop_at_the_first_failed_pool() {
    let fixture = Fixture::new();
    for path in SHARED_XTASK_TESTS {
        fixture.write(path, "never execute this fixture\n");
    }
    fixture.run(&["--helpers"], &[]).unwrap();
    let events = fixture.events();
    let nodes = events
        .iter()
        .filter(|event| event.command == "node")
        .collect::<Vec<_>>();
    assert_eq!(nodes.len(), 2);
    assert_eq!(
        nodes[0].arguments,
        [
            "--test",
            "build/tests/first.test.mjs",
            "build/tests/second.test.mjs",
            "load_tests/benchmark-local.test.mjs",
            "load_tests/report-command.test.mjs",
        ]
    );
    assert_eq!(
        nodes[1].arguments,
        ["--test", "--test-concurrency=1"]
            .into_iter()
            .chain(SHARED_XTASK_TESTS.iter().copied())
            .collect::<Vec<_>>()
    );
    for (phase, count, code) in [("helper-tests", 1, 37), ("serial-helper-tests", 2, 41)] {
        let error = fixture
            .run(
                &["--helpers"],
                &[
                    ("FIXTURE_FAIL", phase.into()),
                    ("FIXTURE_FAILURE_CODE", code.to_string().into()),
                ],
            )
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(
            fixture
                .events()
                .iter()
                .filter(|event| event.command == "node")
                .count(),
            count
        );
    }
}

fn installed_node() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|component| component.join("node"))
        .find(|path| {
            path.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
        .expect("Installed Node is required by the helper scheduling fixture")
        .canonicalize()
        .unwrap()
}

fn literal_shell(value: &Path) -> String {
    format!("'{}'", value.to_str().unwrap().replace('\'', "'\\''"))
}

#[test]
fn actual_node_keeps_the_shared_cli_lease_exclusive_and_other_files_parallel() {
    let fixture = Fixture::new();
    let node = installed_node();
    let wrapper = fixture.root.join("node-wrapper.cjs");
    let actual_node = serde_json::to_string(node.to_str().unwrap()).unwrap();
    fixture.write(
        "node-wrapper.cjs",
        &format!(
            r#"
const assert = require('node:assert/strict');
const {{spawnSync}} = require('node:child_process');
const args = process.argv.slice(2);
assert.equal(args[0], '--test');
const serial = args[1] === '--test-concurrency=1';
if (!serial) {{
  assert(!args.some(value => value.startsWith('--test-concurrency')));
  // Give this private barrier two workers even on a one-CPU test host. The
  // runner must admit the parallel pool without a global serialization flag.
  args.splice(1, 0, '--test-concurrency=2');
}}
const result = spawnSync({actual_node}, args, {{stdio:'inherit', env:process.env}});
assert.ifError(result.error);
assert.equal(result.signal, null);
process.exitCode = result.status;
"#
        ),
    );
    fs::write(
        fixture.bin.join("node"),
        format!(
            "#!/bin/sh\nexec {} {} \"$@\"\n",
            literal_shell(&node),
            literal_shell(&wrapper)
        ),
    )
    .unwrap();
    fs::set_permissions(fixture.bin.join("node"), fs::Permissions::from_mode(0o755)).unwrap();

    let cargo = fixture.root.join("fake-cargo.cjs");
    fixture.write(
        "fake-cargo.cjs",
        r#"
const assert = require('node:assert/strict');
const fs = require('node:fs');
assert.deepEqual(process.argv.slice(2), ['build','--locked','-q','-p','xtask']);
assert(fs.existsSync('.shared-cli-lease'));
fs.mkdirSync('target/debug', {recursive:true});
fs.rmSync('target/debug/xtask', {force:true});
fs.writeFileSync('target/debug/xtask', '#!/bin/sh\nprintf "fixture-cli\\n"\n', {mode:0o700});
"#,
    );
    fs::write(
        fixture.bin.join("cargo"),
        format!(
            "#!/bin/sh\nexec {} {} \"$@\"\n",
            literal_shell(&node),
            literal_shell(&cargo)
        ),
    )
    .unwrap();
    fs::set_permissions(fixture.bin.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
    for path in SHARED_XTASK_TESTS {
        fixture.write(
            path,
            r#"
import assert from 'node:assert/strict';
import {execFileSync, spawnSync} from 'node:child_process';
import {mkdirSync, rmSync, writeFileSync} from 'node:fs';
import {readFile, writeFile} from 'node:fs/promises';
import {basename, join} from 'node:path';
import test from 'node:test';
test('one file owns fake Cargo plus every use of its executable', async () => {
  // Failing admission cannot remove a different worker's lease.
  mkdirSync('.shared-cli-lease');
  try {
    await writeFile('.shared-cli-lease/owner', basename(import.meta.url));
    execFileSync('cargo', ['build','--locked','-q','-p','xtask']);
    assert.equal((await readFile('.shared-cli-lease/owner','utf8')), basename(import.meta.url));
    const child = spawnSync(join(process.cwd(),'target/debug/xtask'), [], {encoding:'utf8'});
    assert.ifError(child.error); assert.equal(child.signal, null);
    assert.equal(child.status, 0); assert.equal(child.stdout, 'fixture-cli\n');
    writeFileSync(`ran-${basename(import.meta.url)}`, 'joined');
  } finally { rmSync('.shared-cli-lease', {recursive:true}); }
});
"#,
        );
    }
    for (name, peer) in [("first", "second"), ("second", "first")] {
        fixture.write(
            &format!("build/tests/{name}.test.mjs"),
            &format!(
                r#"
import assert from 'node:assert/strict';
import {{existsSync, writeFileSync}} from 'node:fs';
import {{setTimeout as wait}} from 'node:timers/promises';
import test from 'node:test';
test('the independent helper pool admits both barrier workers', async () => {{
  writeFileSync('pure-{name}', 'ready');
  for (let count=0; !existsSync('pure-{peer}') && count<2000; count++) await wait(5);
  assert(existsSync('pure-{peer}'), 'Other parallel file was not admitted');
}});
"#
            ),
        );
    }
    for path in [
        "load_tests/benchmark-local.test.mjs",
        "load_tests/report-command.test.mjs",
    ] {
        fixture.write(
            path,
            "import test from 'node:test'; test('inert', () => {});\n",
        );
    }
    fixture.run(&["--helpers"], &[]).unwrap();
    assert!(!fixture.root.join(".shared-cli-lease").exists());
    for path in SHARED_XTASK_TESTS {
        assert_eq!(
            fs::read(fixture.root.join(format!(
                "ran-{}",
                Path::new(path).file_name().unwrap().to_str().unwrap()
            )))
            .unwrap(),
            b"joined"
        );
    }
    for name in ["first", "second"] {
        assert!(fixture.root.join(format!("pure-{name}")).is_file());
    }
}

#[test]
fn default_runs_all_groups_in_order() {
    let fixture = Fixture::new();
    fixture.run(&[], &[]).unwrap();
    assert_eq!(
        phases(&fixture.events()),
        [
            "npm-lint",
            "npm-format:check",
            "npm-test",
            "npm-build",
            "rustup-which-rustc",
            "rustup-which-rustdoc",
            "cargo-fmt",
            "cargo-clippy",
            "cargo-line-length",
            "cargo-protocol-ts",
            "cargo-http-policy",
            "cargo-doc",
            "python-lint",
            "python-format",
            "python-types",
            "sh:.githooks/commit-msg",
            "bash:build/helper with spaces.sh",
            "shellcheck",
            "helper-tests"
        ]
    );
}

#[test]
fn missing_web_dependencies_stop_without_installing_anything() {
    let fixture = Fixture::new();
    fs::remove_dir_all(fixture.root.join("web/node_modules")).unwrap();
    let error = fixture.run(&["--web"], &[]).unwrap_err();
    assert_eq!(error.code, 2);
    assert!(error.message.contains("Install web dependencies first"));
    assert!(fixture.events().is_empty());
}

#[test]
fn missing_toolchain_pin_stops_before_rustup() {
    let fixture = Fixture::new();
    fixture.write("rust-toolchain.toml", "[toolchain]\n");
    let error = fixture.run(&["--rust"], &[]).unwrap_err();
    assert_eq!(error.code, 2);
    assert!(
        error
            .message
            .contains("Cannot read the pinned Rust toolchain")
    );
    assert!(fixture.events().is_empty());
}

#[test]
fn missing_native_archive_or_bindings_stop_before_cargo() {
    for name in [
        "target/openssl-4.0.3/lib/libssl.a",
        "target/openssl-4.0.3/lib/libcrypto.a",
        "target/aws-lc-5.11.0/lib/libcrypto-awslc.a",
        "target/aws-lc-5.11.0/share/rust/aws_lc_bindings.rs",
    ] {
        let fixture = Fixture::new();
        fs::remove_file(fixture.root.join(name)).unwrap();
        let error = fixture.run(&["--rust"], &[]).unwrap_err();
        assert_eq!(error.code, 2);
        assert!(error.message.contains(if name.contains("openssl") {
            "Static OpenSSL"
        } else {
            "Static AWS-LC"
        }));
        assert_eq!(
            phases(&fixture.events()),
            ["rustup-which-rustc", "rustup-which-rustdoc"]
        );
    }
}

#[test]
fn missing_programs_preserve_status_127_without_host_fallback() {
    for (program, group) in [
        ("npm", "--web"),
        ("rustup", "--rust"),
        ("sh", "--helpers"),
        ("bash", "--helpers"),
        ("shellcheck", "--helpers"),
        ("node", "--helpers"),
        ("git", "--python"),
    ] {
        let fixture = Fixture::new();
        fs::remove_file(fixture.bin.join(program)).unwrap();
        let error = fixture.run(&[group], &[]).unwrap_err();
        assert_eq!(error.code, 127, "{program}: {error}");
        assert!(error.message.contains(program));
        assert!(
            !fixture
                .events()
                .iter()
                .any(|event| event.command == program)
        );
    }
}

#[test]
fn nonexecutable_program_preserves_shell_status_126() {
    let fixture = Fixture::new();
    fs::set_permissions(fixture.bin.join("npm"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(fixture.run(&["--web"], &[]).unwrap_err().code, 126);
    assert!(fixture.events().is_empty());
}

#[test]
fn each_failed_phase_preserves_status_and_prevents_every_later_phase() {
    for phase in [
        "npm-lint",
        "npm-format:check",
        "npm-test",
        "npm-build",
        "rustup-which-rustc",
        "rustup-which-rustdoc",
        "cargo-fmt",
        "cargo-clippy",
        "cargo-line-length",
        "cargo-protocol-ts",
        "cargo-http-policy",
        "cargo-doc",
        "python-lint",
        "python-format",
        "python-types",
        "sh:.githooks/commit-msg",
        "bash:build/helper with spaces.sh",
        "shellcheck",
        "helper-tests",
    ] {
        let fixture = Fixture::new();
        let error = fixture
            .run(
                &[],
                &[
                    ("FIXTURE_FAIL", phase.into()),
                    ("FIXTURE_FAILURE_CODE", "43".into()),
                ],
            )
            .unwrap_err();
        assert_eq!(error.code, 43, "{phase}: {error}");
        assert_eq!(
            phases(&fixture.events()).last().unwrap(),
            phase,
            "no command after {phase}"
        );
    }
}

#[test]
fn child_signal_status_is_preserved_without_running_next_phase() {
    let fixture = Fixture::new();
    let error = fixture
        .run(&["--web"], &[("FIXTURE_SIGNAL", "npm-lint".into())])
        .unwrap_err();
    assert_eq!(error.code, 143);
    assert_eq!(phases(&fixture.events()), ["npm-lint"]);
}

#[test]
fn python_discovers_tracked_untracked_nested_stubs_and_newline_paths() {
    let fixture = Fixture::new();
    fixture.run(&["--python"], &[]).unwrap();
    let events = fixture.events();
    assert_eq!(
        phases(&events),
        ["python-lint", "python-format", "python-types"]
    );
    for (event, offset) in events.iter().zip([2, 3, 2]) {
        let mut sources = event.arguments[offset..].to_vec();
        sources.sort();
        let mut expected = SOURCES
            .iter()
            .map(|source| (*source).to_owned())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(sources, expected);
        assert_eq!(Path::new(&event.cwd), fixture.root);
    }
    assert_eq!(&events[0].arguments[..2], ["check", "--"]);
    assert_eq!(&events[1].arguments[..3], ["format", "--check", "--"]);
    assert_eq!(
        &events[2].arguments[..2],
        [
            "--pythonpath",
            fixture
                .root
                .join("ops/ansible/.venv/bin/python")
                .to_str()
                .unwrap()
        ]
    );
}

#[test]
fn python_explicit_environment_with_spaces_never_uses_default_tools() {
    let fixture = Fixture::new();
    let selected = fixture.temporary.join("external controller environment");
    for name in ["python", "ruff", "basedpyright"] {
        fixture.executable(&selected.join("bin").join(name), name);
    }
    fs::remove_dir_all(fixture.root.join("ops/ansible/.venv")).unwrap();
    fixture
        .run(
            &["--python"],
            &[("PYTHON_CHECK_ENV", selected.clone().into_os_string())],
        )
        .unwrap();
    assert_eq!(
        fixture.events()[2].arguments[1],
        selected.join("bin/python").to_str().unwrap()
    );
}

#[test]
fn python_relative_environment_is_resolved_from_checkout() {
    let fixture = Fixture::new();
    fixture
        .run(
            &["--python"],
            &[("PYTHON_CHECK_ENV", "ops/ansible/.venv".into())],
        )
        .unwrap();
    assert_eq!(
        fixture.events()[2].arguments[1],
        "ops/ansible/.venv/bin/python"
    );
}

#[test]
fn python_git_discovery_failure_stops_before_all_checkers() {
    let fixture = Fixture::new();
    let error = fixture
        .run(
            &["--python"],
            &[(
                "GIT_DIR",
                fixture
                    .temporary
                    .join("absent git directory")
                    .into_os_string(),
            )],
        )
        .unwrap_err();
    assert_eq!(error.code, 128);
    assert!(fixture.events().is_empty());
}

#[test]
fn python_rejects_new_source_roots_before_any_checker() {
    for source in [
        "new-owned-root/module.py",
        "build/unapproved.pyi",
        "typings/unapproved.py",
    ] {
        let fixture = Fixture::new();
        fixture.write(source, "# policy must be explicit\n");
        let error = fixture.run(&["--python"], &[]).unwrap_err();
        assert_eq!(error.code, 1);
        assert!(
            error
                .message
                .contains("Python source outside the checked roots")
        );
        assert!(error.message.contains(source));
        assert!(fixture.events().is_empty());
    }
}

#[test]
fn python_checks_all_tools_are_executable_before_discovery() {
    for name in ["python", "ruff", "basedpyright"] {
        let fixture = Fixture::new();
        fs::set_permissions(
            fixture.root.join("ops/ansible/.venv/bin").join(name),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let error = fixture.run(&["--python"], &[]).unwrap_err();
        assert_eq!(error.code, 1);
        assert!(error.message.contains(&format!("Missing {name}")));
        assert!(fixture.events().is_empty());
    }
}

#[test]
fn python_invalid_explicit_environment_does_not_fall_back() {
    let fixture = Fixture::new();
    let error = fixture
        .run(
            &["--python"],
            &[(
                "PYTHON_CHECK_ENV",
                fixture
                    .temporary
                    .join("absent environment")
                    .into_os_string(),
            )],
        )
        .unwrap_err();
    assert_eq!(error.code, 1);
    assert!(error.message.contains("Missing python"));
    assert!(fixture.events().is_empty());
}

#[test]
fn python_empty_owned_source_set_is_not_success() {
    let fixture = Fixture::new();
    for source in SOURCES {
        fs::remove_file(fixture.root.join(source)).unwrap();
    }
    tooling::run(
        &fixture.root,
        &fixture.environment,
        "git",
        ["rm", "--cached", "--quiet", "build/tracked.py"],
    )
    .unwrap();
    let error = fixture.run(&["--python"], &[]).unwrap_err();
    assert_eq!(error.code, 1);
    assert!(error.message.contains("No maintained Python sources found"));
    assert!(fixture.events().is_empty());
}
