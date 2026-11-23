//! The runtime selection checks of `tasks/docker.yml`, `runtime-binaries.yml`
//! and `runtime-verify.yml`, carried over from the Python runtime component
//! tests. Each check is a command with fixed arguments and an assertion that
//! Ansible evaluates; the assertions are run as written against every answer
//! the retired inline programs were recorded on, and must decide as they
//! did. Nothing contacts a host or Docker.

use super::ansible::play;
use super::ansible_modules::command_result;
use super::recorded::{Recorded, replay};
use super::{document, located};
use serde_json::{Value, json};

const RECORDED: &str = include_str!("runtime_components_recorded.json");
const UNITS: &str = "every_recorded_docker_unit_definition_is_answered_as_the_program_did";
const CONTAINERD: &str = "every_recorded_containerd_service_is_answered_as_the_program_did";
const VERSIONS: &str = "every_recorded_upstream_version_answer_is_judged_as_the_program_did";
const IDENTITIES: &str = "every_recorded_runtime_identity_answer_is_judged_as_the_program_did";
const TESTS: [&str; 4] = [UNITS, CONTAINERD, VERSIONS, IDENTITIES];

fn recorded(test: &str) -> Recorded {
    Recorded::new(RECORDED, &TESTS, test)
}

/// The task of `file` under `ops/ansible/tasks` named `name`.
fn task(file: &str, name: &str) -> Value {
    let tasks = document(&format!("ops/ansible/tasks/{file}"));
    let tasks = tasks.as_array().unwrap();
    located(tasks, |task| task["name"] == name).1.clone()
}

fn ok(stdout: &str) -> Value {
    json!({"status": 0, "stdout": stdout})
}

/// The `KEY=VALUE` lines `systemctl show` prints; a field without a value
/// is a line without `=`.
fn unit_text(fields: &[(&str, Option<&str>)]) -> String {
    let line = |(key, value): &(&str, Option<&str>)| match value {
        Some(value) => format!("{key}={value}\n"),
        None => format!("{key}\n"),
    };
    fields.iter().map(line).collect()
}

/// `fields` with `changes` applied in place, where a change to `None`
/// removes the field.
fn changed<'a>(
    fields: &[(&'a str, &'a str)],
    changes: &[(&str, Option<&'a str>)],
) -> Vec<(&'a str, Option<&'a str>)> {
    let mut changed = Vec::new();
    for (key, value) in fields {
        match changes.iter().find(|(name, _)| name == key) {
            Some((_, Some(value))) => changed.push((*key, Some(*value))),
            Some((_, None)) => {}
            None => changed.push((*key, Some(*value))),
        }
    }
    changed
}

const SHOW: &str = concat!(
    "--property=LoadState --property=FragmentPath --property=DropInPaths ",
    "--property=ExecStart --property=Listen"
);

fn show(unit: &str) -> String {
    format!("systemctl show {unit} {SHOW}")
}

const EXEC: &str = concat!(
    "{ path=/usr/bin/dockerd ; argv[]=/usr/bin/dockerd -H fd:// ",
    "--containerd=/run/containerd/containerd.sock ; ignore_errors=no ; start_time=[n/a] ; ",
    "stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
);

fn service_fields() -> Vec<(&'static str, &'static str)> {
    vec![
        ("LoadState", "loaded"),
        ("FragmentPath", "/usr/lib/systemd/system/docker.service"),
        ("DropInPaths", ""),
        ("ExecStart", EXEC),
        ("Listen", ""),
    ]
}

fn socket_fields() -> Vec<(&'static str, &'static str)> {
    vec![
        ("LoadState", "loaded"),
        ("FragmentPath", "/usr/lib/systemd/system/docker.socket"),
        ("DropInPaths", ""),
        ("ExecStart", ""),
        ("Listen", "/run/docker.sock (Stream)"),
    ]
}

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

/// The program's words for a service and for a socket it refused.
const SERVICE_REFUSED: &str =
    "Docker must use its packaged Unix-socket command without extra host options.";
const SOCKET_REFUSED: &str = "Docker socket must listen only on its local Unix socket.";

/// A case position and the refusal it must meet.
type Stricter = (usize, Option<&'static str>);

/// The unit definitions the program was given, in its order, and the
/// positions of those the rules refuse although the program accepted them.
/// Each of those prints, beside the local definition the program read, a
/// listener or command it never read (it kept a property's last line and
/// skipped lines without a value) or a host option after the packaged
/// command it looked for.
fn unit_cases() -> (Vec<Value>, Vec<Stricter>) {
    fn case(service: Value, socket: Value) -> Value {
        let (service_shown, socket_shown) = (show("docker.service"), show("docker.socket"));
        json!({"answers": {service_shown: service, socket_shown: socket}})
    }
    let text = |fields: &[(&str, &str)], changes: &[(&str, Option<&str>)]| {
        unit_text(&changed(fields, changes))
    };
    let (service, socket) = (service_fields(), socket_fields());
    let good_service = ok(&text(&service, &[]));
    let good_socket = ok(&text(&socket, &[]));
    let absent = |status: i32| {
        let fields = [
            ("LoadState", Some("not-found")),
            ("FragmentPath", Some("")),
            ("DropInPaths", Some("")),
            ("ExecStart", Some("")),
            ("Listen", Some("")),
        ];
        json!({"status": status, "stdout": unit_text(&fields)})
    };
    let mut cases = vec![case(good_service.clone(), good_socket.clone())];
    let mut stricter = Vec::new();
    cases.push(case(
        ok(&text(
            &service,
            &[("FragmentPath", Some("/lib/systemd/system/docker.service"))],
        )),
        ok(&text(
            &socket,
            &[
                ("FragmentPath", Some("/lib/systemd/system/docker.socket")),
                ("Listen", Some("/var/run/docker.sock (Stream)")),
            ],
        )),
    ));
    for status in [0, 1, 4] {
        cases.push(case(absent(status), absent(status)));
    }
    cases.push(case(absent(3), good_socket.clone()));
    cases.push(case(good_service.clone(), absent(3)));
    cases.push(case(absent(0), good_socket.clone()));
    cases.push(case(good_service.clone(), absent(1)));
    type Variant = fn(&[(&str, &str)]) -> Value;
    let variants: [Variant; 12] = [
        |fields| json!({"status": 1, "stdout": unit_text(&changed(fields, &[]))}),
        |fields| {
            ok(&unit_text(&changed(
                fields,
                &[("LoadState", Some("masked"))],
            )))
        },
        |fields| ok(&unit_text(&changed(fields, &[("LoadState", None)]))),
        |fields| {
            let changes = [("FragmentPath", Some("/etc/systemd/system/x"))];
            ok(&unit_text(&changed(fields, &changes)))
        },
        |fields| ok(&unit_text(&changed(fields, &[("FragmentPath", None)]))),
        |fields| {
            let changes = [("DropInPaths", Some("/etc/systemd/system/x.d/a.conf"))];
            ok(&unit_text(&changed(fields, &changes)))
        },
        |fields| ok(&unit_text(&changed(fields, &[("DropInPaths", None)]))),
        |fields| ok(&format!("{}\n", unit_text(&changed(fields, &[])))),
        |fields| ok(&unit_text(&changed(fields, &[])).replace('\n', "\r\n")),
        |fields| {
            let printed = format!("{}X=", unit_text(&changed(fields, &[])));
            json!({"status": 0, "stdoutHex": format!("{}ff0a", hex(&printed))})
        },
        |fields| {
            let printed = unit_text(&changed(fields, &[]));
            json!({"status": 0, "stdout": printed, "stderrHex": "ff"})
        },
        |fields| {
            let printed = unit_text(&changed(fields, &[]));
            json!({"status": 0, "stdout": printed, "sleep": true})
        },
    ];
    for variant in variants {
        cases.push(case(variant(&service), good_socket.clone()));
        cases.push(case(good_service.clone(), variant(&socket)));
    }
    // A line naming a listener without a value is refused where the program
    // skipped it; the service rules read no listener lines.
    let bare = |fields: &[(&str, &str)]| ok(&format!("{}Listen\n", text(fields, &[])));
    cases.push(case(bare(&service), good_socket.clone()));
    stricter.push((cases.len(), Some(SOCKET_REFUSED)));
    cases.push(case(good_service.clone(), bare(&socket)));
    let tcp = EXEC.replace(
        "containerd.sock ;",
        "containerd.sock -H tcp://0.0.0.0:2375 ;",
    );
    let first = EXEC.replace(
        "argv[]=/usr/bin/dockerd -H fd://",
        "argv[]=/usr/bin/dockerd -H tcp://0.0.0.0:2375 -H fd://",
    );
    let twice = format!("{EXEC} ; {{ path=/bin/true ; argv[]=/bin/true ; }}");
    let doubled = EXEC.replace("argv[]=", "argv[]=argv[]=");
    let unterminated = EXEC.replace(" ;", "");
    for command in [
        Some(tcp.as_str()),
        Some(first.as_str()),
        Some(twice.as_str()),
        Some(doubled.as_str()),
        Some(unterminated.as_str()),
        Some(""),
        None,
        Some("argv[]=/usr/bin/dockerd -H fd:// --containerd=/run/containerd/containerd.sock ;"),
        Some("xargv[]=/usr/bin/dockerd -H fd:// --containerd=/run/containerd/containerd.sock ; "),
    ] {
        let service = ok(&text(&service, &[("ExecStart", command)]));
        cases.push(case(service, good_socket.clone()));
    }
    for listen in [
        Some("0.0.0.0:2375 (Stream)"),
        Some("/run/docker.sock"),
        Some("/run/docker.sock (Stream) "),
        Some("/run/docker.sock (Stream) 0.0.0.0:2375 (Stream)"),
        Some(""),
        None,
    ] {
        let socket = ok(&text(&socket, &[("Listen", listen)]));
        cases.push(case(good_service.clone(), socket));
    }
    let extra = "Listen=0.0.0.0:2375 (Stream)\n";
    let socket_text = text(&socket, &[]);
    cases.push(case(
        good_service.clone(),
        ok(&format!("{socket_text}{extra}")),
    ));
    stricter.push((cases.len(), Some(SOCKET_REFUSED)));
    cases.push(case(
        good_service.clone(),
        ok(&format!("{extra}{socket_text}")),
    ));
    let listening = |lines: &[&str]| {
        let lines: String = lines
            .iter()
            .map(|line| format!("Listen={line}\n"))
            .collect();
        ok(&format!("{}{lines}", text(&socket, &[("Listen", None)])))
    };
    let both_remote = ["0.0.0.0:2375 (Stream)", "[::]:2376 (Stream)"];
    cases.push(case(good_service.clone(), listening(&both_remote)));
    for lines in [
        [
            "/run/docker.sock (Stream)",
            "0.0.0.0:2375 (Stream)",
            "/var/run/docker.sock (Stream)",
        ],
        [
            "/run/docker.sock (Stream)",
            "0.0.0.0:2375 (Datagram)",
            "/run/docker.sock (Stream)",
        ],
    ] {
        stricter.push((cases.len(), Some(SOCKET_REFUSED)));
        cases.push(case(good_service.clone(), listening(&lines)));
    }
    let local = ["/run/docker.sock (Stream)", "/var/run/docker.sock (Stream)"];
    cases.push(case(good_service.clone(), listening(&local)));
    // A host option after an argument `;` follows the packaged command the
    // program looked for.
    for smuggled in [
        "-H tcp://0.0.0.0:2375",
        "--host=tcp://0.0.0.0:2375",
        "--host tcp://0.0.0.0:2375",
        "-Htcp://0.0.0.0:2375",
        "-DH tcp://0.0.0.0:2375",
        "-H fd://",
    ] {
        let after = format!("containerd.sock ; {smuggled} ;");
        let command = EXEC.replace("containerd.sock ;", &after);
        let service = ok(&text(&service, &[("ExecStart", Some(&command))]));
        stricter.push((cases.len(), Some(SERVICE_REFUSED)));
        cases.push(case(service, good_socket.clone()));
    }
    let service_text = text(&service, &[]);
    // A second command line, which the program read in place of the first.
    let remote_command = EXEC.replace("-H fd://", "-H tcp://0.0.0.0:2375");
    let remote_line = format!("ExecStart={remote_command}\n");
    stricter.push((cases.len(), Some(SERVICE_REFUSED)));
    cases.push(case(
        ok(&format!("{remote_line}{service_text}")),
        good_socket.clone(),
    ));
    cases.push(case(
        ok(&format!("{service_text}{remote_line}")),
        good_socket.clone(),
    ));
    // A command line without a value is refused, where the program skipped it.
    stricter.push((cases.len(), Some(SERVICE_REFUSED)));
    cases.push(case(
        ok(&format!("{service_text}ExecStart\n")),
        good_socket.clone(),
    ));
    // What Debian 13 prints for the units of the pinned docker-ce, in the
    // order systemd answers: before the daemon first starts, and while it
    // runs.
    let running = EXEC
        .replace(
            "start_time=[n/a]",
            "start_time=[Mon 2026-11-23 09:00:01 UTC]",
        )
        .replace("pid=0", "pid=812");
    for command in [EXEC, running.as_str()] {
        let printed_service = [
            ("ExecStart", Some(command)),
            ("LoadState", Some("loaded")),
            (
                "FragmentPath",
                Some("/usr/lib/systemd/system/docker.service"),
            ),
            ("DropInPaths", Some("")),
        ];
        let printed_socket = [
            ("Listen", Some("/run/docker.sock (Stream)")),
            ("LoadState", Some("loaded")),
            (
                "FragmentPath",
                Some("/usr/lib/systemd/system/docker.socket"),
            ),
            ("DropInPaths", Some("")),
        ];
        cases.push(case(
            ok(&unit_text(&printed_service)),
            ok(&unit_text(&printed_socket)),
        ));
    }
    cases.push(case(
        ok(&format!("{service_text}LoadState=masked\n")),
        good_socket.clone(),
    ));
    let etc = [("FragmentPath", Some("/etc/systemd/system/docker.service"))];
    cases.push(case(
        ok(&text(&service, &etc)),
        ok(&text(&socket, &[("LoadState", Some("masked"))])),
    ));
    cases.push(case(
        ok(&text(
            &service,
            &[("ExecStart", Some("argv[]=/usr/bin/dockerd ;"))],
        )),
        ok(&text(&socket, &[("DropInPaths", Some("/etc/x.conf"))])),
    ));
    let mut missing = case(good_service, good_socket);
    missing["missing"] = json!(["systemctl"]);
    cases.push(missing);
    (cases, stricter)
}

/// What an executable that is not there leaves the command module with,
/// under a task that never fails.
fn missing(item: &Value) -> Value {
    json!({"changed": false, "failed": false, "item": item, "rc": 2, "stdout": "",
        "stdout_lines": [], "stderr": "", "stderr_lines": []})
}

/// The registered result of a looped command for a case's answers to each
/// of `asked`, or nothing where the module refuses an answer.
fn looped(
    case: &Value,
    asked: &[(String, Value)],
    missing_as: impl Fn(&Value) -> Value,
) -> Option<Value> {
    let mut results = Vec::new();
    for (argv, item) in asked {
        let program = argv.split(' ').next().unwrap();
        let gone = case["missing"]
            .as_array()
            .is_some_and(|missing| missing.contains(&json!(program)));
        let result = if gone {
            missing_as(item)
        } else {
            command_result(&case["answers"][argv.as_str()], item, false)?
        };
        results.push(result);
    }
    Some(json!({"changed": false, "results": results}))
}

/// Both inspections of the Docker units read the same definitions with the
/// same arguments and decide them with the same rules, and those rules
/// decide every recorded definition as the program did, except where they
/// are deliberately stricter. A unit that is not found is accepted; a found
/// one must be loaded from its packaged file without drop-ins. The service
/// must print one command, the packaged daemon command, with no host option
/// beside its `-H fd://`, and every listener line of the socket must be
/// `/run/docker.sock` or `/var/run/docker.sock` as a stream, in whatever
/// order they are printed. An executable that is not there is refused as
/// uninspectable.
#[test]
fn every_recorded_docker_unit_definition_is_answered_as_the_program_did() {
    let read = task(
        "docker.yml",
        "Read the existing Docker service and socket definitions",
    );
    let decided = task(
        "docker.yml",
        "Verify that existing Docker service and socket definitions remain local",
    );
    let reread = task(
        "docker.yml",
        "Read the packaged service and socket definitions before enabling Docker",
    );
    let redecided = task(
        "docker.yml",
        "Verify the packaged service and socket before enabling Docker",
    );
    let argv = json!([
        "/usr/bin/systemctl",
        "show",
        "{{ item }}",
        "--property=LoadState",
        "--property=FragmentPath",
        "--property=DropInPaths",
        "--property=ExecStart",
        "--property=Listen"
    ]);
    for read in [&read, &reread] {
        assert_eq!(read["ansible.builtin.command"], json!({"argv": argv}));
        assert_eq!(read["loop"], json!(["docker.service", "docker.socket"]));
        assert_eq!(read["register"], "scbench_docker_units");
        assert_eq!(read["failed_when"], false);
        assert_eq!(read["changed_when"], false);
        assert_eq!(read["check_mode"], false);
        assert_eq!(read["timeout"], 15);
    }
    assert_eq!(read["tags"], json!(["always"]));
    assert_eq!(decided["tags"], json!(["always"]));
    for key in ["ansible.builtin.assert", "vars", "loop", "loop_control"] {
        assert_eq!(decided[key], redecided[key], "{key}");
    }
    assert_eq!(decided["loop"], "{{ scbench_docker_units.results }}");
    let asked: Vec<(String, Value)> = ["docker.service", "docker.socket"]
        .iter()
        .map(|unit| (show(unit), json!(unit)))
        .collect();
    let results = |case: &Value| {
        let units = looped(case, &asked, missing)?;
        Some(json!({"scbench_docker_units": units}))
    };
    let (cases, stricter) = unit_cases();
    assert_eq!(stricter.len(), 12);
    assert_eq!(
        replay(
            recorded(UNITS),
            UNITS,
            &[decided],
            &cases,
            results,
            &stricter
        ),
        71
    );
}

const PACKAGED: &str =
    "{ path=/usr/bin/containerd ; argv[]=/usr/bin/containerd ; ignore_errors=no ; pid=0 }";
const MANAGED: &str = "/usr/local/lib/simplestchat-runtime/2.4.1/bin/containerd";
const DROPIN: &str = "/etc/systemd/system/containerd.service.d/simplestchat-runtime.conf";
const CONTAINERD_SHOW: &str = concat!(
    "systemctl show containerd.service --property=LoadState --property=FragmentPath ",
    "--property=DropInPaths --property=ExecStart"
);

fn containerd_fields() -> Vec<(&'static str, &'static str)> {
    vec![
        ("LoadState", "loaded"),
        ("FragmentPath", "/usr/lib/systemd/system/containerd.service"),
        ("DropInPaths", ""),
        ("ExecStart", PACKAGED),
    ]
}

/// The containerd definitions the program was given, in its order.
fn containerd_cases() -> Vec<Value> {
    let case = |answer: Value| json!({"answers": {CONTAINERD_SHOW: answer}});
    let fields = containerd_fields();
    let text = |changes: &[(&str, Option<&str>)]| unit_text(&changed(&fields, changes));
    let managed = format!("{{ path={MANAGED} ; argv[]={MANAGED} ; ignore_errors=no }}");
    let both = format!("{DROPIN} /etc/systemd/system/containerd.service.d/custom.conf");
    let mut cases = vec![
        case(ok(&text(&[]))),
        case(ok(&text(&[
            ("DropInPaths", Some(DROPIN)),
            ("ExecStart", Some(managed.as_str())),
        ]))),
        case(ok(&text(&[(
            "FragmentPath",
            Some("/lib/systemd/system/containerd.service"),
        )]))),
    ];
    let changes: [(&str, Option<&str>); 20] = [
        ("LoadState", Some("not-found")),
        ("LoadState", Some("masked")),
        ("LoadState", None),
        (
            "FragmentPath",
            Some("/etc/systemd/system/containerd.service"),
        ),
        ("FragmentPath", None),
        ("FragmentPath", Some("")),
        (
            "DropInPaths",
            Some("/etc/systemd/system/containerd.service.d/custom.conf"),
        ),
        ("DropInPaths", Some(both.as_str())),
        ("DropInPaths", None),
        (
            "ExecStart",
            Some("{ path=/usr/bin/containerd ; argv[]=/usr/bin/containerd --custom ; }"),
        ),
        (
            "ExecStart",
            Some("{ argv[]=/usr/bin/containerd ; } { argv[]=/usr/bin/containerd ; }"),
        ),
        (
            "ExecStart",
            Some("{ argv[]=/usr/local/lib/simplestchat-runtime/latest/bin/containerd ; }"),
        ),
        (
            "ExecStart",
            Some("{ argv[]=/usr/local/lib/simplestchat-runtime/2.4/bin/containerd ; }"),
        ),
        (
            "ExecStart",
            Some("{ argv[]=/usr/local/lib/simplestchat-runtime/2.4.1.0/bin/containerd ; }"),
        ),
        (
            "ExecStart",
            Some("{ argv[]=/usr/local/lib/simplestchat-runtime/\u{662}.4.1/bin/containerd ; }"),
        ),
        ("ExecStart", Some("{ argv[]=/opt/containerd ; }")),
        ("ExecStart", Some("{ argv[]=/usr/bin/containerd;}")),
        ("ExecStart", Some("xargv[]=/usr/bin/containerd ;")),
        ("ExecStart", Some("")),
        ("ExecStart", None),
    ];
    for change in changes {
        cases.push(case(ok(&text(&[change]))));
    }
    let unchanged = text(&[]);
    cases.push(case(json!({"status": 1, "stdout": unchanged})));
    cases.push(case(json!({"status": 3, "stdout": unchanged})));
    cases.push(case(ok(&format!("{unchanged}DropInPaths=/etc/x.conf\n"))));
    cases.push(case(ok(&format!("DropInPaths=/etc/x.conf\n{unchanged}"))));
    cases.push(case(ok(&text(&[
        ("FragmentPath", Some("/etc/x")),
        ("DropInPaths", Some("/etc/y")),
    ]))));
    cases.push(case(ok(&unchanged.replace('\n', "\r\n"))));
    let undecodable = format!("{}ff0a", hex(&unchanged));
    cases.push(case(json!({"status": 0, "stdoutHex": undecodable})));
    cases.push(case(
        json!({"status": 0, "stdout": unchanged, "stderrHex": "ff"}),
    ));
    cases.push(case(
        json!({"status": 0, "stdout": unchanged, "sleep": true}),
    ));
    let mut gone = case(ok(""));
    gone["missing"] = json!(["systemctl"]);
    cases.push(gone);
    cases
}

/// The containerd service keeps its packaged unit, at most the one drop-in
/// this selection writes, and one command running the packaged or a
/// selected containerd without arguments: every recorded definition is
/// decided as the program decided it.
#[test]
fn every_recorded_containerd_service_is_answered_as_the_program_did() {
    let read = task(
        "runtime-binaries.yml",
        "Read the containerd service definition before selecting upstream binaries",
    );
    let decided = task(
        "runtime-binaries.yml",
        "Refuse unreviewed containerd service overrides before selecting upstream binaries",
    );
    let argv = json!([
        "/usr/bin/systemctl",
        "show",
        "containerd.service",
        "--property=LoadState",
        "--property=FragmentPath",
        "--property=DropInPaths",
        "--property=ExecStart"
    ]);
    assert_eq!(read["ansible.builtin.command"], json!({"argv": argv}));
    assert_eq!(read["register"], "scbench_containerd_unit");
    assert_eq!(
        (
            &read["failed_when"],
            &read["changed_when"],
            &read["check_mode"],
            &read["timeout"]
        ),
        (&json!(false), &json!(false), &json!(false), &json!(15))
    );
    assert!(read.get("loop").is_none() && read.get("when").is_none());
    let asked = [(CONTAINERD_SHOW.to_owned(), Value::Null)];
    let results = |case: &Value| {
        let unit = looped(case, &asked, missing)?;
        Some(json!({"scbench_containerd_unit": unit["results"][0]}))
    };
    assert_eq!(
        replay(
            recorded(CONTAINERD),
            CONTAINERD,
            &[decided],
            &containerd_cases(),
            results,
            &[]
        ),
        33
    );
}

/// The version answers the program was given, in its order.
fn version_cases() -> Vec<Value> {
    let good_containerd = ok("containerd github.com/containerd/containerd/v2 v2.4.1 0123abc\n");
    let good_buildx = ok("github.com/docker/buildx v0.37.2 0123abc\n");
    let case = |containerd: &Value, buildx: &Value| {
        json!({"arguments": ["2.4.1", "0.37.2"],
            "answers": {"containerd --version": containerd, "docker buildx version": buildx}})
    };
    let mut cases = vec![case(&good_containerd, &good_buildx)];
    let line = "containerd github.com/containerd/containerd/v2 v2.4.1 x\n";
    for answer in [
        ok("containerd github.com/containerd/containerd/v2 v2.4.1"),
        ok("  containerd\tgithub.com/containerd/containerd/v2\u{a0}v2.4.1\n"),
        ok("containerd github.com/containerd/containerd/v2 v2.4.0 0123abc\n"),
        ok("containerd github.com/containerd/containerd/v2 2.4.1 0123abc\n"),
        ok("containerd github.com/containerd/containerd/v2\n"),
        ok(""),
        ok("containerd v2.4.1 github.com/containerd/containerd/v2\n"),
        json!({"status": 1, "stdout": line}),
        json!({"status": 0, "stdoutHex": "636f6e7461696e65726420782076322e342e3120ff0a"}),
        json!({"status": 0, "stdout": line, "stderrHex": "ff"}),
        json!({"status": 0, "stdout": good_containerd["stdout"], "sleep": true}),
    ] {
        cases.push(case(&answer, &good_buildx));
    }
    let buildx = "6769746875622e636f6d2f646f636b65722f6275696c647820762e302e33372e32ff0a";
    for answer in [
        ok("github.com/docker/buildx v0.37.2"),
        ok("github.com/docker/buildx v0.37.1 0123abc\n"),
        ok("github.com/docker/buildx 0.37.2 0123abc\n"),
        ok("buildx v0.37.2 0123abc\n"),
        ok("github.com/docker/buildx\n"),
        ok("x github.com/docker/buildx v0.37.2\n"),
        json!({"status": 1, "stdout": "github.com/docker/buildx v0.37.2 x\n"}),
        json!({"status": 0, "stdoutHex": buildx}),
        json!({"status": 0, "stdout": good_buildx["stdout"], "sleep": true}),
    ] {
        cases.push(case(&good_containerd, &answer));
    }
    cases.push(case(&ok("x"), &ok("y")));
    for program in ["containerd", "docker"] {
        let mut gone = case(&good_containerd, &good_buildx);
        gone["missing"] = json!([program]);
        cases.push(gone);
    }
    cases
}

/// What `env -i` leaves a task that never fails with when the program it
/// was to run is not there.
fn not_executed(item: &Value) -> Value {
    json!({"changed": false, "failed": false, "item": item, "rc": 127, "stdout": "",
        "stdout_lines": [], "stderr": "env: No such file or directory",
        "stderr_lines": ["env: No such file or directory"]})
}

/// The selected containerd reports its version third and Buildx its version
/// second, each read with the environment cleared; every recorded answer is
/// judged as the program judged it.
#[test]
fn every_recorded_upstream_version_answer_is_judged_as_the_program_did() {
    let read = task(
        "runtime-binaries.yml",
        "Read the selected containerd and Buildx versions",
    );
    let decided = task(
        "runtime-binaries.yml",
        "Verify the selected Buildx plugin and containerd executable versions",
    );
    let cleared = concat!(
        "{{ ['/usr/bin/env', '-i', 'PATH=/usr/sbin:/usr/bin:/sbin:/bin', 'LC_ALL=C'] ",
        "+ item }}"
    );
    assert_eq!(read["ansible.builtin.command"], json!({"argv": cleared}));
    assert_eq!(
        read["loop"],
        json!([
            [SELECTED, "--version"],
            ["/usr/bin/docker", "buildx", "version"]
        ])
    );
    assert_eq!(read["register"], "scbench_upstream_versions");
    assert_eq!(
        (&read["failed_when"], &read["timeout"], &read["when"]),
        (&json!(false), &json!(15), &json!("not ansible_check_mode"))
    );
    assert_eq!(decided["when"], "not ansible_check_mode");
    let asked = [
        (
            "containerd --version".to_owned(),
            json!([MANAGED, "--version"]),
        ),
        (
            "docker buildx version".to_owned(),
            json!(["/usr/bin/docker", "buildx", "version"]),
        ),
    ];
    let results = |case: &Value| {
        let versions = looped(case, &asked, not_executed)?;
        Some(json!({"scbench_upstream_versions": versions,
            "scbench_containerd_upstream_version": "2.4.1",
            "scbench_docker_buildx_upstream_version": "0.37.2"}))
    };
    assert_eq!(
        replay(
            recorded(VERSIONS),
            VERSIONS,
            &[decided],
            &version_cases(),
            results,
            &[]
        ),
        24
    );
}

/// The selected containerd executable, as the version check names it.
const SELECTED: &str = concat!(
    "/usr/local/lib/simplestchat-runtime/{{ scbench_containerd_upstream_version }}",
    "/bin/containerd"
);
const SERVER: &str = "docker --host unix:///var/run/docker.sock version --format {{json .Server}}";
const COMPOSE: &str = "docker --host unix:///var/run/docker.sock compose version --short";
const BUILDX: &str = "docker --host unix:///var/run/docker.sock buildx version";

/// The engine's description as the recorder spelled it, with `changes`.
fn server(changes: &[(&str, Option<Value>)]) -> Value {
    let mut value = json!({
        "Platform": {"Name": "Docker Engine - Community"}, "Version": "29.8.2",
        "Components": [
            {"Name": "Engine", "Version": "29.8.2", "Details": {"ApiVersion": "1.52"}},
            {"Name": "containerd", "Version": "v2.4.1", "Details": {"GitCommit": "x"}},
            {"Name": "runc", "Version": "1.5.2", "Details": {"GitCommit": "y"}},
            {"Name": "docker-init", "Version": "0.19.0"},
        ],
    });
    for (key, item) in changes {
        match item {
            Some(item) => value[*key] = item.clone(),
            None => {
                value.as_object_mut().unwrap().remove(*key);
            }
        }
    }
    value
}

fn dump(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}

/// The identity answers the program was given, in its order.
fn identity_cases() -> Vec<Value> {
    let good = json!({SERVER: ok(&format!("{}\n", dump(&server(&[])))),
        COMPOSE: ok("5.6.0\n"), BUILDX: ok("github.com/docker/buildx v0.37.2 0123abc\n")});
    let case = |answers: &[(&str, Value)]| {
        let mut described = json!({"arguments": ["29.8.2", "5.6.0", "2.4.1", "0.37.2", "1.5.2"],
            "answers": good.clone()});
        for (key, answer) in answers {
            described["answers"][*key] = answer.clone();
        }
        described
    };
    let engine = |answer: Value| case(&[(SERVER, answer)]);
    let components = |items: Value| {
        let changed = server(&[("Components", Some(items))]);
        engine(ok(&format!("{}\n", dump(&changed))))
    };
    let base = server(&[])["Components"].as_array().unwrap().clone();
    let named = |name: &'static str| -> Vec<Value> {
        base.iter()
            .filter(|item| item["Name"] != name)
            .cloned()
            .collect()
    };
    let versioned = |name: &str, version: Option<Value>| {
        let items = base.iter().map(|item| {
            let mut item = item.clone();
            if item["Name"] == name {
                match &version {
                    Some(version) => item["Version"] = version.clone(),
                    None => {
                        item.as_object_mut().unwrap().remove("Version");
                    }
                }
            }
            item
        });
        components(Value::Array(items.collect()))
    };
    let padded = dump(&server(&[]));
    let with = |extra: Vec<Value>, before: bool| {
        let items: Vec<Value> = if before {
            extra.into_iter().chain(base.clone()).collect()
        } else {
            base.clone().into_iter().chain(extra).collect()
        };
        components(Value::Array(items))
    };
    let undecodable = format!("{}2c2278223a22ff227d", hex(&padded[..padded.len() - 1]));
    let mut cases = vec![
        case(&[]),
        engine(ok(&padded)),
        engine(ok(&format!("  {padded}\n\n"))),
        engine(ok(&format!("\u{1c}{padded}"))),
        engine(ok(&format!("\u{a0}{padded}\u{2028}"))),
        engine(json!({"status": 0, "stdout": padded, "padTo": 65536})),
        engine(json!({"status": 0, "stdout": padded, "padTo": 65537})),
        engine(json!({"status": 1, "stdout": padded})),
        engine(json!({"status": 0, "stdout": padded, "sleep": true})),
        engine(json!({"status": 0, "stdoutHex": undecodable})),
        engine(json!({"status": 0, "stdout": padded, "stderrHex": "ff"})),
    ];
    for text in ["not json", "", "[]", "null", "\"text\"", "7"] {
        cases.push(engine(ok(text)));
    }
    for change in [
        ("Version", Some(json!("29.7.2"))),
        ("Version", Some(json!(29))),
        ("Version", None),
        ("Components", None),
        ("Components", Some(Value::Null)),
        ("Components", Some(json!({}))),
        ("Components", Some(json!({"containerd": "v2.4.1"}))),
        ("Components", Some(json!(""))),
        ("Components", Some(json!("ab"))),
        ("Components", Some(json!(7))),
    ] {
        cases.push(engine(ok(&dump(&server(&[change])))));
    }
    cases.push(components(json!([])));
    cases.push(components(Value::Array(named("containerd"))));
    cases.push(components(Value::Array(named("runc"))));
    cases.push(with(
        vec![json!({"Name": "containerd", "Version": "v2.4.1"})],
        false,
    ));
    cases.push(versioned("containerd", Some(json!("2.4.1"))));
    cases.push(versioned("containerd", Some(json!("vv2.4.1"))));
    cases.push(versioned("containerd", Some(json!("v2.3.6"))));
    cases.push(versioned("containerd", None));
    cases.push(versioned("containerd", Some(json!(2))));
    cases.push(versioned("containerd", Some(Value::Null)));
    cases.push(versioned("runc", Some(json!("v1.5.2"))));
    cases.push(versioned("runc", Some(json!("1.5.1"))));
    cases.push(versioned("Engine", Some(json!(7))));
    cases.push(with(vec![json!("containerd")], false));
    cases.push(with(vec![json!("containerd")], true));
    cases.push(with(vec![Value::Null], false));
    cases.push(with(vec![json!({"Version": "x"})], true));
    cases.push(with(vec![json!({"Name": 7, "Version": "x"})], true));
    let mut renamed = vec![json!({"Name": "Containerd", "Version": "v2.4.1"})];
    renamed.extend(named("containerd"));
    cases.push(components(Value::Array(renamed)));
    let repeated = format!("{},\"Version\":\"29.7.2\"}}", &padded[..padded.len() - 1]);
    cases.push(engine(ok(&repeated)));
    let compose = |answer: Value| case(&[(COMPOSE, answer)]);
    for text in ["5.6.0", " 5.6.0 \n", "v5.6.0\n", "5.5.1\n", ""] {
        cases.push(compose(ok(text)));
    }
    cases.push(compose(json!({"status": 1, "stdout": "5.6.0\n"})));
    for size in [65536, 65537] {
        cases.push(compose(
            json!({"status": 0, "stdout": "5.6.0", "padTo": size}),
        ));
    }
    cases.push(compose(ok("5.6.0\u{1f}")));
    cases.push(compose(json!({"status": 0, "stdoutHex": "352e362e30ff"})));
    cases.push(compose(
        json!({"status": 0, "stdout": "5.6.0\n", "sleep": true}),
    ));
    let buildx = |answer: Value| case(&[(BUILDX, answer)]);
    for text in [
        "github.com/docker/buildx v0.37.2",
        "github.com/docker/buildx v0.37.1 x\n",
        "x github.com/docker/buildx v0.37.2\n",
        "github.com/docker/buildx\n",
        "",
    ] {
        cases.push(buildx(ok(text)));
    }
    cases.push(buildx(
        json!({"status": 1, "stdout": "github.com/docker/buildx v0.37.2\n"}),
    ));
    for size in [65536, 65537] {
        let answer = json!({"status": 0, "stdout": "github.com/docker/buildx v0.37.2",
            "padTo": size});
        cases.push(buildx(answer));
    }
    let undecodable = "6769746875622e636f6d2f646f636b65722f6275696c64782076302e33372e32ff";
    cases.push(buildx(json!({"status": 0, "stdoutHex": undecodable})));
    let older = ok(&dump(&server(&[("Version", Some(json!("29.7.2")))])));
    cases.push(case(&[(SERVER, older), (COMPOSE, ok("x\n"))]));
    cases.push(case(&[
        (COMPOSE, ok("x\n")),
        (BUILDX, json!({"status": 1, "stdout": ""})),
    ]));
    let mut gone = case(&[]);
    gone["missing"] = json!(["docker"]);
    cases.push(gone);
    cases
}

/// The active engine, its containerd and runc, Compose and Buildx are each
/// read once with the environment cleared and their whole answer kept, and
/// decided in the program's order: every recorded answer is judged as the
/// program judged it, except that an answer whose error stream is not
/// UTF-8 is refused, which the program let pass.
#[test]
fn every_recorded_runtime_identity_answer_is_judged_as_the_program_did() {
    let tasks = document("ops/ansible/tasks/runtime-verify.yml");
    let tasks = tasks.as_array().unwrap();
    assert_eq!(tasks.len(), 2);
    let (read, decided) = (&tasks[0], &tasks[1]);
    let argv = concat!(
        "{{ ['/usr/bin/env', '-i', 'PATH=/usr/sbin:/usr/bin:/sbin:/bin', 'LC_ALL=C', ",
        "'/usr/bin/docker', '--host', 'unix:///var/run/docker.sock'] + item }}"
    );
    assert_eq!(
        read["ansible.builtin.command"],
        json!({"argv": argv, "strip_empty_ends": false})
    );
    assert_eq!(read["register"], "scbench_runtime_identities");
    assert_eq!(
        (&read["failed_when"], &read["timeout"], &read["when"]),
        (&json!(false), &json!(20), &json!("not ansible_check_mode"))
    );
    let items = [
        (
            SERVER,
            json!([
                "version",
                "--format",
                "{% raw %}{{json .Server}}{% endraw %}"
            ]),
        ),
        (COMPOSE, json!(["compose", "version", "--short"])),
        (BUILDX, json!(["buildx", "version"])),
    ];
    let listed: Vec<&Value> = items.iter().map(|(_, item)| item).collect();
    assert_eq!(read["loop"], json!(listed));
    let results = |case: &Value| {
        let mut results = Vec::new();
        for (argv, item) in &items {
            let gone = case["missing"] == json!(["docker"]);
            let answer = &case["answers"][*argv];
            results.push(if gone {
                not_executed(item)
            } else {
                command_result(answer, item, true)?
            });
        }
        Some(
            json!({"scbench_runtime_identities": {"changed": false, "results": results},
            "scbench_docker_engine_version": "5:29.8.2-1~debian.13~trixie",
            "scbench_docker_compose_version": "5.6.0-1~debian.13~trixie",
            "scbench_containerd_upstream_version": "2.4.1",
            "scbench_docker_buildx_upstream_version": "0.37.2",
            "scbench_runc_upstream_version": "1.5.2"}),
        )
    };
    let cases = identity_cases();
    let stricter = cases
        .iter()
        .position(|case| case["answers"][SERVER].get("stderrHex").is_some())
        .unwrap();
    assert_eq!(
        replay(
            recorded(IDENTITIES),
            IDENTITIES,
            std::slice::from_ref(decided),
            &cases,
            results,
            &[(stricter, None)]
        ),
        70
    );
}

/// The commands run with exactly the arguments the checks read, the raw
/// Go template kept as written, and an environment holding only the path
/// and the C locale. A stand-in at each fixed executable path prints what
/// it was given.
#[test]
fn the_runtime_commands_run_with_their_arguments_and_a_cleared_environment() {
    let scratch = super::ansible::Scratch::new();
    let stand_in = scratch.path().join("stand-in");
    let script = "#!/bin/sh\nprintf '%s|' \"$@\"\nprintf '\\n'\nenv | sort\n";
    std::fs::write(&stand_in, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
    let stand_in = stand_in.to_str().unwrap();
    let identities = &document("ops/ansible/tasks/runtime-verify.yml")[0];
    let versions = task(
        "runtime-binaries.yml",
        "Read the selected containerd and Buildx versions",
    );
    let mut tasks = Vec::new();
    for (index, task) in [identities, &versions].into_iter().enumerate() {
        let text = serde_json::to_string(task).unwrap();
        let text = text
            .replace("/usr/bin/docker", stand_in)
            .replace(SELECTED, stand_in);
        let mut task: Value = serde_json::from_str(&text).unwrap();
        task["register"] = json!(format!("ran_{index}"));
        tasks.push(task);
    }
    tasks.push(json!({"ansible.builtin.copy": {
        "content": "{{ [ran_0, ran_1] | to_json }}", "dest": "{{ scratch }}/ran.json",
        "mode": "0600"}}));
    let variables = json!({"scbench_containerd_upstream_version": "2.4.1"});
    let ran = play(&Value::Array(tasks), &variables, &[]);
    let ran: Vec<Value> =
        serde_json::from_slice(&std::fs::read(ran.path().join("ran.json")).unwrap()).unwrap();
    let said = |index: usize, item: usize| {
        let stdout = ran[index]["results"][item]["stdout"]
            .as_str()
            .unwrap()
            .to_owned();
        let (arguments, environment) = stdout.split_once('\n').unwrap();
        let names: Vec<String> = environment
            .lines()
            .map(|line| line.split_once('=').unwrap().0.to_owned())
            .filter(|name| !["PWD", "SHLVL", "_"].contains(&name.as_str()))
            .collect();
        assert!(environment.contains("PATH=/usr/sbin:/usr/bin:/sbin:/bin\n"));
        assert!(environment.contains("LC_ALL=C\n"));
        assert_eq!(names, ["LC_ALL", "PATH"], "{index} {item}");
        arguments.to_owned()
    };
    let host = "--host|unix:///var/run/docker.sock|";
    assert_eq!(
        said(0, 0),
        format!("{host}version|--format|{{{{json .Server}}}}|")
    );
    assert_eq!(said(0, 1), format!("{host}compose|version|--short|"));
    assert_eq!(said(0, 2), format!("{host}buildx|version|"));
    assert_eq!(said(1, 0), "--version|");
    assert_eq!(said(1, 1), "buildx|version|");
    // Exact answers keep their trailing line breaks; the versions are stripped.
    let printed = ran[0]["results"][0]["stdout"].as_str().unwrap();
    assert!(printed.ends_with('\n'));
    let stripped = ran[1]["results"][0]["stdout"].as_str().unwrap();
    assert!(!stripped.ends_with('\n'));
}
