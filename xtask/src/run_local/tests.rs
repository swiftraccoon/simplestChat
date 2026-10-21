//! Actual local-launch preparation with inert tools in owned fake checkouts.

use super::*;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const KEYS: &[&str] = &[
    "RUSTC",
    "RUSTDOC",
    "OPENSSL_DIR",
    "OPENSSL_STATIC",
    "PKG_CONFIG_PATH",
    "AWS_LC_SYS_SYSTEM_DIR",
    "AWS_LC_SYS_STATIC",
    "AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK",
    "PIP_CONSTRAINT",
    "BIND_ADDR",
    "PORT",
    "ANNOUNCE_IP",
    "MEDIA_WORKERS",
    "WEBRTC_SERVER_PORT_BASE",
    "ALLOW_AD_HOC_ROOMS",
    "PATH",
    "DATABASE_URL",
];
const DISPATCH: &str = r#"
record() {
    printf '%s\000' "$command" "$PWD" "$#" "$@"
    printf '%s\000' "${RUSTC-}" "${RUSTDOC-}" "${OPENSSL_DIR-}" "${OPENSSL_STATIC-}" \
      "${PKG_CONFIG_PATH-}" "${AWS_LC_SYS_SYSTEM_DIR-}" "${AWS_LC_SYS_STATIC-}" \
      "${AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK-}" "${PIP_CONSTRAINT-}" "${BIND_ADDR-}" \
      "${PORT-}" "${ANNOUNCE_IP-}" "${MEDIA_WORKERS-}" "${WEBRTC_SERVER_PORT_BASE-}" \
      "${ALLOW_AD_HOC_ROOMS-}" "$PATH" "${DATABASE_URL-}"
}
record "$@" >> "$FIXTURE_LOG"
case "$command" in
    rustup) if [ "$1" = which ]; then phase=rustup-which; else phase=cargo; fi ;;
    node) phase=npm-install ;;
    npm) if [ "$3" = ci ]; then phase=npm-ci; else phase=npm-build; fi ;;
    install-openssl) phase=openssl-install ;;
    install-aws-lc) phase=aws-lc-install ;;
    route|ipconfig) phase="$command" ;;
    *) exit 94 ;;
esac
if [ "${FIXTURE_FAIL-}" = "$phase" ]; then exit "${FIXTURE_FAILURE_CODE-37}"; fi
case "$command" in
    rustup)
        if [ "$1" = which ]; then
            printf '%s/toolchain/bin/%s\n' "$FIXTURE_ROOT" "$4"
        else
            command=server
            record "$@" >> "$FIXTURE_LOG"
            if [ "${FIXTURE_FAIL-}" = server ]; then exit "${FIXTURE_FAILURE_CODE-37}"; fi
            if [ "${FIXTURE_SIGNAL-}" = server ]; then kill -TERM $$; fi
        fi ;;
    install-openssl)
        if [ "${FIXTURE_EMPTY_INSTALL-}" = 1 ]; then exit 0; fi
        /bin/mkdir -p "$1/lib"
        : > "$1/lib/libssl.a"
        : > "$1/lib/libcrypto.a" ;;
    install-aws-lc)
        if [ "${FIXTURE_EMPTY_INSTALL-}" = 1 ]; then exit 0; fi
        if [ -d "$1" ]; then
            echo 'AWS-LC destination already exists; retain it or select a new prefix.' >&2
            exit 2
        fi
        /bin/mkdir -p "$1/lib" "$1/share/rust"
        : > "$1/lib/libcrypto-awslc.a"
        : > "$1/share/rust/aws_lc_bindings.rs" ;;
    route)
        if [ "${FIXTURE_ROUTE_FAIL-}" = 1 ]; then exit 1; fi
        printf '   route to: default\n  interface: en7\n' ;;
    ipconfig)
        if [ "$2" = en7 ]; then
            if [ "${FIXTURE_ROUTE_ADDRESS_EMPTY-}" != 1 ]; then printf '192.0.2.7\n'; fi
        elif [ "$2" = en0 ] && [ "${FIXTURE_FALLBACK_FAIL-}" != 1 ]; then
            printf '192.0.2.20\n'
        else exit 1; fi ;;
esac
"#;

#[derive(Debug)]
struct Event {
    command: String,
    cwd: String,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
}

struct Fixture {
    temporary: PathBuf,
    root: PathBuf,
    bin: PathBuf,
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
            "simplestchat-run-rust-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&temporary).unwrap();
        let temporary = temporary.canonicalize().unwrap();
        let root = temporary.join("checkout with spaces");
        let bin = temporary.join("fake commands");
        for directory in [
            &root,
            &bin,
            &root.join("toolchain/bin"),
            &root.join("build"),
            &root.join("web/dist"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        let log = temporary.join("commands.nul");
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let media = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let environment = [
            (
                "PATH",
                std::env::join_paths([
                    bin.clone(),
                    PathBuf::from("/usr/bin"),
                    PathBuf::from("/bin"),
                ])
                .unwrap(),
            ),
            ("FIXTURE_LOG", log.as_os_str().to_owned()),
            ("FIXTURE_ROOT", root.as_os_str().to_owned()),
            ("ANNOUNCE_IP", "127.0.0.1".into()),
            ("PORT", port.to_string().into()),
            ("WEBRTC_SERVER_PORT_BASE", media.to_string().into()),
            ("MEDIA_WORKERS", "1".into()),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect();
        let fixture = Self {
            temporary,
            root,
            bin,
            log,
            environment,
        };
        fixture.write("rust-toolchain.toml", "[toolchain]\nchannel = \"9.88.7\"\n");
        fixture.write("build/pip-constraints.txt", "# fixture\n");
        fixture.write(
            "web/dist/index.html",
            "<!doctype html><title>Owned fixture</title>\n",
        );
        fixture.install_native();
        for name in ["rustup", "node", "npm", "route", "ipconfig"] {
            fixture.executable(&fixture.bin.join(name), name);
        }
        fixture.executable(
            &fixture.root.join("build/install-openssl.sh"),
            "install-openssl",
        );
        fixture.executable(
            &fixture.root.join("build/install-aws-lc.sh"),
            "install-aws-lc",
        );
        for name in ["rustc", "rustdoc"] {
            let path = fixture.root.join("toolchain/bin").join(name);
            fs::write(&path, "#!/bin/sh\nexit 94\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fixture
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn executable(&self, path: &Path, name: &str) {
        fs::write(path, format!("#!/bin/sh\ncommand='{name}'\n{DISPATCH}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn install_native(&self) {
        for file in [
            "target/openssl-4.0.3/lib/libssl.a",
            "target/openssl-4.0.3/lib/libcrypto.a",
            "target/aws-lc-5.11.0/lib/libcrypto-awslc.a",
            "target/aws-lc-5.11.0/share/rust/aws_lc_bindings.rs",
        ] {
            self.write(file, "fixture archive\n");
        }
    }

    fn prepare(
        &self,
        args: &[&str],
        changes: &[(&str, &str)],
        platform: Platform,
    ) -> tooling::Result<Option<Command>> {
        let mut environment = self.environment.clone();
        environment.extend(
            changes
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into())),
        );
        prepare(
            &self.root,
            &args
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
            &environment,
            platform,
        )
    }

    fn events(&self) -> Vec<Event> {
        let data = fs::read(&self.log).unwrap_or_default();
        let mut words = data.split(|byte| *byte == 0).peekable();
        let mut result = Vec::new();
        while let Some(command) = words.next() {
            if words.peek().is_none() && command.is_empty() {
                break;
            }
            let text = |word: &[u8]| String::from_utf8(word.to_vec()).unwrap();
            let cwd = text(words.next().unwrap());
            let count: usize = text(words.next().unwrap()).parse().unwrap();
            let arguments = words.by_ref().take(count).map(text).collect();
            let environment = KEYS
                .iter()
                .map(|key| ((*key).to_owned(), text(words.next().unwrap())))
                .collect();
            result.push(Event {
                command: text(command),
                cwd,
                arguments,
                environment,
            });
        }
        result
    }
}

fn child_env(command: &Command, key: &str) -> String {
    command
        .get_envs()
        .find(|(name, _)| *name == key)
        .and_then(|(_, value)| value)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn help_and_invalid_arguments_have_no_setup_side_effects() {
    let missing = Path::new("/does-not-exist");
    assert!(
        prepare(
            missing,
            &["--help".into()],
            &Environment::new(),
            Platform::Other
        )
        .unwrap()
        .is_none()
    );
    for args in [
        vec!["--unknown".into()],
        vec!["unexpected".into()],
        vec!["--skip-web".into(), "unexpected".into()],
    ] {
        let error = prepare(missing, &args, &Environment::new(), Platform::Other).unwrap_err();
        assert_eq!(error.code, 2);
        assert!(error.message.contains("Usage:"));
    }
}

#[test]
fn pins_the_toolchain_and_native_environment_and_refreshes_locked_web_dependencies() {
    let f = Fixture::new();
    let original = f.environment.clone();
    let command = f
        .prepare(
            &[],
            &[
                ("RUSTC", "/wrong/homebrew/rustc"),
                ("RUSTDOC", "/wrong/homebrew/rustdoc"),
                ("RUSTUP_TOOLCHAIN", "stable"),
                ("OPENSSL_DIR", "/wrong/openssl"),
                ("OPENSSL_STATIC", "0"),
                ("AWS_LC_SYS_SYSTEM_DIR", "/wrong/aws-lc"),
                ("AWS_LC_SYS_STATIC", "0"),
                ("AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK", "1"),
                ("PIP_CONSTRAINT", "/wrong/pip-constraints"),
                ("BIND_ADDR", "0.0.0.0"),
            ],
            Platform::Other,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        f.environment, original,
        "the caller's environment is immutable"
    );
    assert_eq!(command.get_program(), "rustup");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            "run",
            "9.88.7",
            "cargo",
            "run",
            "--locked",
            "--bin",
            "simplestChat"
        ]
    );
    assert_eq!(command.get_current_dir(), Some(f.root.as_path()));
    for (key, relative) in [
        ("RUSTC", "toolchain/bin/rustc"),
        ("RUSTDOC", "toolchain/bin/rustdoc"),
        ("OPENSSL_DIR", "target/openssl-4.0.3"),
        ("AWS_LC_SYS_SYSTEM_DIR", "target/aws-lc-5.11.0"),
        ("PIP_CONSTRAINT", "build/pip-constraints.txt"),
        ("PKG_CONFIG_PATH", "target/openssl-4.0.3/lib/pkgconfig"),
    ] {
        assert_eq!(
            child_env(&command, key),
            f.root.join(relative).to_string_lossy()
        );
    }
    for (key, value) in [
        ("OPENSSL_STATIC", "1"),
        ("AWS_LC_SYS_STATIC", "1"),
        ("AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK", "0"),
        ("BIND_ADDR", "127.0.0.1"),
        ("MEDIA_WORKERS", "1"),
        ("ALLOW_AD_HOC_ROOMS", "true"),
    ] {
        assert_eq!(child_env(&command, key), value);
    }
    let paths: Vec<_> = std::env::split_paths(&child_env(&command, "PATH")).collect();
    assert_eq!(paths[0], f.root.join("target/npm-12.2.0/bin"));
    assert_eq!(paths[1], f.root.join("toolchain/bin"));
    let events = f.events();
    assert_eq!(
        events
            .iter()
            .map(|event| event.command.as_str())
            .collect::<Vec<_>>(),
        ["rustup", "rustup", "node", "npm", "npm"]
    );
    assert!(
        events
            .iter()
            .all(|event| event.cwd == f.root.to_string_lossy())
    );
    assert_eq!(
        events[2].arguments,
        [
            "build/install-npm.mjs",
            f.root.join("target/npm-12.2.0").to_str().unwrap()
        ]
    );
    assert_eq!(
        events[3].arguments,
        ["--prefix", "web", "ci", "--ignore-scripts"]
    );
    assert_eq!(events[4].arguments, ["--prefix", "web", "run", "build"]);
    assert_eq!(
        events[3].environment["RUSTC"],
        f.root.join("toolchain/bin/rustc").to_string_lossy()
    );
}

#[test]
fn defaults_and_explicit_network_settings_keep_http_on_loopback() {
    let f = Fixture::new();
    let mut defaults = f.environment.clone();
    for key in ["PORT", "MEDIA_WORKERS", "WEBRTC_SERVER_PORT_BASE"] {
        defaults.remove(OsStr::new(key));
    }
    local_defaults(&mut defaults);
    for (key, expected) in [
        ("PORT", "3000"),
        ("MEDIA_WORKERS", "1"),
        ("WEBRTC_SERVER_PORT_BASE", "40000"),
        ("ALLOW_AD_HOC_ROOMS", "true"),
        ("BIND_ADDR", "127.0.0.1"),
    ] {
        assert_eq!(defaults[OsStr::new(key)], expected);
    }
    // Defaults are tested before binding to avoid assuming standard ports are free.
    defaults.insert("ANNOUNCE_IP".into(), "invalid".into());
    assert!(
        prepare(&f.root, &[], &defaults, Platform::Other)
            .unwrap_err()
            .message
            .contains("ANNOUNCE_IP")
    );
    let command = f
        .prepare(
            &["--skip-web"],
            &[
                ("ANNOUNCE_IP", "192.0.2.33"),
                ("MEDIA_WORKERS", "2"),
                ("ALLOW_AD_HOC_ROOMS", "false"),
                ("BIND_ADDR", "0.0.0.0"),
            ],
            Platform::Other,
        )
        .unwrap()
        .unwrap();
    assert_eq!(child_env(&command, "ANNOUNCE_IP"), "192.0.2.33");
    assert_eq!(child_env(&command, "MEDIA_WORKERS"), "2");
    assert_eq!(child_env(&command, "ALLOW_AD_HOC_ROOMS"), "false");
    assert_eq!(child_env(&command, "BIND_ADDR"), "127.0.0.1");
    assert!(
        !f.events()
            .iter()
            .any(|event| ["route", "ipconfig"].contains(&event.command.as_str()))
    );
}

#[test]
fn macos_discovers_the_default_interface_and_falls_back_to_en0() {
    for changes in [
        vec![("ANNOUNCE_IP", "")],
        vec![("ANNOUNCE_IP", ""), ("FIXTURE_ROUTE_FAIL", "1")],
        vec![("ANNOUNCE_IP", ""), ("FIXTURE_ROUTE_ADDRESS_EMPTY", "1")],
    ] {
        let f = Fixture::new();
        let command = f
            .prepare(&["--skip-web"], &changes, Platform::Darwin)
            .unwrap()
            .unwrap();
        let expected = if changes.len() == 1 {
            "192.0.2.7"
        } else {
            "192.0.2.20"
        };
        assert_eq!(child_env(&command, "ANNOUNCE_IP"), expected);
        assert!(f.events().iter().any(|event| event.command == "route"));
    }
    let f = Fixture::new();
    for (platform, changes) in [
        (Platform::Other, vec![("ANNOUNCE_IP", "")]),
        (
            Platform::Darwin,
            vec![
                ("ANNOUNCE_IP", ""),
                ("FIXTURE_ROUTE_FAIL", "1"),
                ("FIXTURE_FALLBACK_FAIL", "1"),
            ],
        ),
    ] {
        assert!(
            f.prepare(&["--skip-web"], &changes, platform)
                .unwrap_err()
                .message
                .contains("ANNOUNCE_IP")
        );
    }
}

#[test]
fn installs_only_missing_native_prerequisites_and_skip_web_omits_all_npm_work() {
    let f = Fixture::new();
    fs::remove_dir_all(f.root.join("target")).unwrap();
    f.prepare(&[], &[], Platform::Other).unwrap();
    assert_eq!(
        f.events()
            .iter()
            .filter(|event| event.command == "install-openssl")
            .count(),
        1
    );
    assert_eq!(
        f.events()
            .iter()
            .filter(|event| event.command == "install-aws-lc")
            .count(),
        1
    );
    let installer = f
        .events()
        .into_iter()
        .find(|event| event.command == "install-openssl")
        .unwrap();
    assert_eq!(
        installer.arguments,
        [f.root.join("target/openssl-4.0.3").to_str().unwrap()]
    );
    let f = Fixture::new();
    f.prepare(&["--skip-web"], &[], Platform::Other).unwrap();
    assert!(
        !f.events()
            .iter()
            .any(|event| ["node", "npm"].contains(&event.command.as_str()))
    );
    fs::remove_file(f.root.join("web/dist/index.html")).unwrap();
    let failure = f
        .prepare(&["--skip-web"], &[], Platform::Other)
        .unwrap_err();
    assert_eq!(failure.code, 2);
    assert!(failure.message.contains("web/dist"));
}

#[test]
fn missing_tools_constraints_and_selected_compilers_fail_before_installs() {
    for dependency in ["rustup", "node"] {
        let f = Fixture::new();
        fs::remove_file(f.bin.join(dependency)).unwrap();
        // Keep PATH fixture-only so a host installation cannot satisfy the check.
        let failure = f
            .prepare(
                &["--skip-web"],
                &[("PATH", f.bin.to_str().unwrap())],
                Platform::Other,
            )
            .unwrap_err();
        assert_eq!(failure.code, 2);
        assert!(failure.message.contains(dependency));
        assert!(f.events().is_empty());
    }
    let f = Fixture::new();
    fs::remove_file(f.root.join("build/pip-constraints.txt")).unwrap();
    assert!(
        f.prepare(&["--skip-web"], &[], Platform::Other)
            .unwrap_err()
            .message
            .contains("pip-constraints")
    );
    assert!(f.events().is_empty());
    for compiler in ["rustc", "rustdoc"] {
        let f = Fixture::new();
        fs::set_permissions(
            f.root.join("toolchain/bin").join(compiler),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let failure = f
            .prepare(&["--skip-web"], &[], Platform::Other)
            .unwrap_err();
        assert_eq!(failure.code, 2);
        assert!(failure.message.contains("Rust 9.88.7 is unavailable"));
        assert!(
            !f.events()
                .iter()
                .any(|event| event.command.starts_with("install-"))
        );
    }
}

#[test]
fn both_openssl_archives_are_required_and_partial_aws_lc_is_refused() {
    let f = Fixture::new();
    fs::remove_file(f.root.join("target/openssl-4.0.3/lib/libcrypto.a")).unwrap();
    f.prepare(&["--skip-web"], &[], Platform::Other).unwrap();
    assert_eq!(
        f.events()
            .iter()
            .filter(|event| event.command == "install-openssl")
            .count(),
        1
    );
    let f = Fixture::new();
    fs::remove_file(
        f.root
            .join("target/aws-lc-5.11.0/share/rust/aws_lc_bindings.rs"),
    )
    .unwrap();
    let failure = f
        .prepare(&["--skip-web"], &[], Platform::Other)
        .unwrap_err();
    assert_eq!(failure.code, 2);
    assert!(!f.events().iter().any(|event| event.command == "server"));
}

#[test]
fn every_setup_failure_stops_and_retains_its_status_before_the_server() {
    for phase in [
        "rustup-which",
        "openssl-install",
        "aws-lc-install",
        "npm-install",
        "npm-ci",
        "npm-build",
    ] {
        let f = Fixture::new();
        fs::remove_dir_all(f.root.join("target")).unwrap();
        let failure = f
            .prepare(
                &[],
                &[("FIXTURE_FAIL", phase), ("FIXTURE_FAILURE_CODE", "37")],
                Platform::Other,
            )
            .unwrap_err();
        assert_eq!(failure.code, 37, "{phase}: {failure}");
        assert!(!f.events().iter().any(|event| event.command == "server"));
    }
    for phase in ["cargo", "server"] {
        let f = Fixture::new();
        let mut command = f
            .prepare(
                &["--skip-web"],
                &[("FIXTURE_FAIL", phase), ("FIXTURE_FAILURE_CODE", "37")],
                Platform::Other,
            )
            .unwrap()
            .unwrap();
        let status = command.status().unwrap();
        assert_eq!(tooling::shell_status(status), 37);
        assert_eq!(
            f.events().iter().any(|event| event.command == "server"),
            phase == "server"
        );
    }
    let f = Fixture::new();
    let status = f
        .prepare(
            &["--skip-web"],
            &[("FIXTURE_SIGNAL", "server")],
            Platform::Other,
        )
        .unwrap()
        .unwrap()
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(15));
    assert_eq!(tooling::shell_status(status), 143);
}

#[test]
fn invalid_configuration_fails_before_tool_queries_or_installation() {
    let f = Fixture::new();
    for changes in [
        vec![("ANNOUNCE_IP", "::1")],
        vec![("ANNOUNCE_IP", "not-an-ip")],
        vec![("ANNOUNCE_IP", "127.000.0.1")],
        vec![("PORT", "0")],
        vec![("PORT", "65536")],
        vec![("PORT", "03")],
        vec![("MEDIA_WORKERS", "0")],
        vec![("MEDIA_WORKERS", "65")],
        vec![("WEBRTC_SERVER_PORT_BASE", "65535"), ("MEDIA_WORKERS", "2")],
    ] {
        let failure = f.prepare(&[], &changes, Platform::Other).unwrap_err();
        assert_eq!(failure.code, 2);
        assert!(failure.message.contains("must be") || failure.message.contains("range exceeds"));
        assert!(f.events().is_empty());
    }
}

#[test]
fn database_refusals_never_expose_credentials_or_connect_to_the_database() {
    let f = Fixture::new();
    for value in [
        "",
        "not-a-url",
        "postgres://user:private-password@example.test/chat_dev",
        "postgres://user:private-password@127.0.0.1/chat_dev?hostaddr=192.0.2.20",
        "postgres://127.0.0.1/chat_dev?options=unexpected",
        "postgres://127.0.0.1/",
        "postgres://127.0.0.1/chat_dev#fragment",
        "https://127.0.0.1/chat_dev",
    ] {
        let failure = f
            .prepare(&[], &[("DATABASE_URL", value)], Platform::Other)
            .unwrap_err();
        assert_eq!(failure.code, 2);
        assert!(failure.message.contains("DATABASE_URL"));
        assert!(!failure.message.contains("private-password"));
        assert!(f.events().is_empty());
    }
    for value in [
        "postgres://user:private-password@127.0.0.1:1/chat_dev?sslmode=disable",
        "postgresql://localhost:1/chat_dev",
        "postgres://[::1]:1/chat_dev?sslmode=disable",
    ] {
        let command = f
            .prepare(&["--skip-web"], &[("DATABASE_URL", value)], Platform::Other)
            .unwrap()
            .unwrap();
        assert_eq!(child_env(&command, "DATABASE_URL"), value);
    }
}

#[test]
fn occupied_http_and_media_sockets_remain_owned_and_preflight_releases_its_own_probes() {
    let f = Fixture::new();
    let tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = tcp.local_addr().unwrap().port().to_string();
    let failure = f
        .prepare(&[], &[("PORT", &port)], Platform::Other)
        .unwrap_err();
    assert!(failure.message.contains("EADDRINUSE"));
    assert_eq!(tcp.local_addr().unwrap().port().to_string(), port);
    assert!(f.events().is_empty());
    let udp = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let media = udp.local_addr().unwrap().port().to_string();
    let failure = f
        .prepare(&[], &[("WEBRTC_SERVER_PORT_BASE", &media)], Platform::Other)
        .unwrap_err();
    assert!(failure.message.contains("EADDRINUSE"));
    assert_eq!(udp.local_addr().unwrap().port().to_string(), media);
    assert!(f.events().is_empty());
    for _ in 0..2 {
        f.prepare(&["--skip-web"], &[], Platform::Other).unwrap();
    }
}

#[test]
fn every_media_worker_is_probed_before_installation() {
    let f = Fixture::new();
    let occupied = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let port = occupied.local_addr().unwrap().port();
    let previous = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port - 1)).unwrap();
    drop(previous);
    let failure = f
        .prepare(
            &[],
            &[
                ("WEBRTC_SERVER_PORT_BASE", &(port - 1).to_string()),
                ("MEDIA_WORKERS", "2"),
            ],
            Platform::Other,
        )
        .unwrap_err();
    assert!(failure.message.contains("EADDRINUSE"));
    assert!(failure.message.contains(&port.to_string()));
    assert_eq!(occupied.local_addr().unwrap().port(), port);
    assert!(f.events().is_empty());
}

#[test]
fn a_successful_installer_must_supply_the_required_archives_and_bindings() {
    let f = Fixture::new();
    fs::remove_dir_all(f.root.join("target")).unwrap();
    let failure = f
        .prepare(
            &["--skip-web"],
            &[("FIXTURE_EMPTY_INSTALL", "1")],
            Platform::Other,
        )
        .unwrap_err();
    assert_eq!(failure.code, 2);
    assert!(failure.message.contains("Static OpenSSL is missing"));
    assert!(
        !f.events()
            .iter()
            .any(|event| ["node", "npm", "server"].contains(&event.command.as_str()))
    );
}
