//! Owned inert fixtures for the native DTLS controller.

use super::*;
use crate::ci::native_dtls::{
    files,
    fixtures::{DIAGNOSTIC, Fixture},
};
use std::{cell::Cell, ffi::OsString, fs, os::unix::fs::PermissionsExt, rc::Rc};

struct Time(Rc<Cell<Instant>>);
impl Clock for Time {
    fn now(&self) -> Instant {
        self.0.get()
    }
}
struct Fake {
    prefix: std::path::PathBuf,
    temporary: std::path::PathBuf,
    calls: Vec<(Program, Vec<OsString>)>,
    fail: Option<(usize, u8)>,
    replay_failure: Option<u8>,
    unproven: bool,
    cancel_after_publication: bool,
    deadline_after_publication: Option<(Rc<Cell<Instant>>, Instant)>,
    wrong_log: bool,
    replaced_log: bool,
    late_replaced_log: bool,
    replayed: Vec<(bool, Vec<u8>)>,
}
impl Fake {
    fn new(owned: &Owned, context: &Context) -> Self {
        Self {
            prefix: owned.path.clone(),
            temporary: context.temporary.clone(),
            calls: vec![],
            fail: None,
            replay_failure: None,
            unproven: false,
            cancel_after_publication: false,
            deadline_after_publication: None,
            wrong_log: false,
            replaced_log: false,
            late_replaced_log: false,
            replayed: vec![],
        }
    }
}
impl Executor for Fake {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let stage = self.calls.len();
        assert_eq!(command.capture, None);
        assert!(command.stdout.is_none());
        command.dtls.as_ref().unwrap().validate(stage == 2).unwrap();
        assert_eq!(
            command.program,
            if stage == 2 {
                Program::DtlsWorker
            } else {
                Program::DtlsPython
            }
        );
        assert!(command.timeout <= plan::BUDGET - plan::RESERVE);
        self.calls
            .push((command.program, command.arguments.clone()));
        let log = Arc::try_unwrap(command.combined_log.take().unwrap()).unwrap();
        let mut writer = log.writer().unwrap();
        use std::io::Write;
        writer
            .write_all(if stage == 1 && !self.wrong_log && !self.replaced_log {
                DIAGNOSTIC.as_bytes()
            } else {
                b"owned fixture output\n"
            })
            .unwrap();
        files::close(writer).unwrap();
        assert!(log.finish().await.unwrap().complete);
        if stage == 1 && self.replaced_log {
            fs::rename(
                self.prefix.join("dtls.log"),
                self.prefix.join("detached-stream"),
            )
            .unwrap();
            fs::write(self.prefix.join("dtls.log"), DIAGNOSTIC).unwrap();
            fs::set_permissions(
                self.prefix.join("dtls.log"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        if stage == 1 {
            let path = self.prefix.join("build-test");
            fs::create_dir_all(&path).unwrap();
            let worker = path.join("mediasoup-worker-test");
            fs::write(&worker, b"owned-inert-output").unwrap();
            fs::set_permissions(worker, fs::Permissions::from_mode(0o700)).unwrap();
        }
        if let Some((selected, code)) = self.fail
            && selected == stage
        {
            return Err(failure(code));
        }
        Ok(vec![])
    }
    async fn ready(&mut self) -> Result<()> {
        if self.late_replaced_log && self.calls.len() == 3 {
            self.late_replaced_log = false;
            fs::rename(
                self.prefix.join("dtls.log"),
                self.prefix.join("late-detached-stream"),
            )
            .unwrap();
            fs::write(self.prefix.join("dtls.log"), "PRIVATE-REPLACEMENT").unwrap();
            fs::set_permissions(
                self.prefix.join("dtls.log"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        if self.temporary.join("native-dtls-verified").exists() {
            if self.cancel_after_publication {
                return Err(failure(143));
            }
            if let Some((clock, deadline)) = &self.deadline_after_publication {
                clock.set(*deadline);
            }
        }
        Ok(())
    }
    fn cleanup_passed(&self) -> bool {
        !self.unproven
    }
    async fn replay(&mut self, bytes: &[u8], stderr: bool, _: Instant) -> Result<()> {
        self.replayed.push((stderr, bytes.to_vec()));
        match self.replay_failure {
            Some(code) => Err(failure(code)),
            None => Ok(()),
        }
    }
}
fn record(fixture: &mut Fixture) {
    fixture.context.revision = Some("a".repeat(40));
}

#[tokio::test]
async fn fixed_three_phase_argv_canonical_roles_closed_logs_and_success_cleanup_match_reference() {
    let mut fixture = Fixture::new();
    record(&mut fixture);
    let owned = Owned::new(&fixture.context).unwrap();
    let path = owned.path.clone();
    let tools = Tools::open(&fixture.context).unwrap();
    let mut fake = Fake::new(&owned, &fixture.context);
    execute(
        &fixture.context,
        owned,
        tools,
        &mut fake,
        Instant::now() + plan::BUDGET,
    )
    .await
    .unwrap();
    assert_eq!(fake.calls.len(), 3);
    assert_eq!(
        fake.calls[0].1,
        vec![
            OsString::from("-m"),
            "pip".into(),
            "install".into(),
            "--no-user".into(),
            "--target".into(),
            path.join("out/pip_invoke").into(),
            "--require-hashes".into(),
            "--only-binary=:all:".into(),
            "--requirement".into(),
            path.join("worker/python-invoke-requirements.txt").into()
        ]
    );
    assert_eq!(
        fake.calls[1].1,
        vec![
            OsString::from("-m"),
            "invoke".into(),
            "--search-root".into(),
            path.join("worker").into(),
            "test".into()
        ]
    );
    assert_eq!(fake.calls[2].1, vec![OsString::from("[dtls-close]")]);
    assert!(!path.exists());
    assert!(fixture.prefixes().is_empty());
    assert_eq!(
        fs::read(fixture.context.temporary.join("native-dtls-verified")).unwrap(),
        format!("{}\n", "a".repeat(40)).as_bytes()
    );
    assert_eq!(fake.replayed.len(), 1);
    assert!(!fake.replayed[0].0);
    assert!(fake.replayed[0].1.ends_with(
        b"Bandwidth constraint updates preserve controller state and reach the pacer.\n"
    ));
}

#[tokio::test]
async fn every_phase_failure_retains_logs_removes_tools_and_preserves_known_status() {
    for stage in 0..3 {
        for code in [42, 124, 130, 143] {
            let mut fixture = Fixture::new();
            record(&mut fixture);
            let owned = Owned::new(&fixture.context).unwrap();
            let path = owned.path.clone();
            let tools = Tools::open(&fixture.context).unwrap();
            let mut fake = Fake::new(&owned, &fixture.context);
            fake.fail = Some((stage, code));
            assert_eq!(
                execute(
                    &fixture.context,
                    owned,
                    tools,
                    &mut fake,
                    Instant::now() + plan::BUDGET
                )
                .await
                .unwrap_err()
                .code,
                code
            );
            assert_eq!(fake.calls.len(), stage + 1);
            assert!(path.join("build.log").exists());
            assert!(!path.join("worker").exists());
            assert!(!path.join("build-test").exists());
            assert!(
                !fixture
                    .context
                    .temporary
                    .join("native-dtls-verified")
                    .exists()
            );
            assert!(fake.replayed.iter().all(|(stderr, _)| *stderr));
        }
    }
}

#[tokio::test]
async fn failed_assertions_replay_or_group_proof_cannot_publish_and_keep_private_logs() {
    for case in [
        "assertion",
        "replay",
        "cleanup",
        "replacement",
        "late-replacement",
    ] {
        let mut fixture = Fixture::new();
        record(&mut fixture);
        let owned = Owned::new(&fixture.context).unwrap();
        let path = owned.path.clone();
        let tools = Tools::open(&fixture.context).unwrap();
        let mut fake = Fake::new(&owned, &fixture.context);
        match case {
            "assertion" => fake.wrong_log = true,
            "replay" => fake.replay_failure = Some(124),
            "replacement" => fake.replaced_log = true,
            "late-replacement" => fake.late_replaced_log = true,
            _ => fake.unproven = true,
        }
        assert!(
            execute(
                &fixture.context,
                owned,
                tools,
                &mut fake,
                Instant::now() + plan::BUDGET
            )
            .await
            .is_err()
        );
        assert!(path.join("dtls.log").is_file());
        assert!(!path.join("worker").exists());
        assert!(
            !fixture
                .context
                .temporary
                .join("native-dtls-verified")
                .exists()
        );
    }
}

#[tokio::test]
async fn late_cancel_and_deadline_after_marker_publication_withdraw_only_owned_marker() {
    for deadline_case in [false, true] {
        let mut fixture = Fixture::new();
        record(&mut fixture);
        let owned = Owned::new(&fixture.context).unwrap();
        let retained = owned.path.with_file_name(format!(
            "{}.failed",
            owned.path.file_name().unwrap().to_str().unwrap()
        ));
        let tools = Tools::open(&fixture.context).unwrap();
        let mut fake = Fake::new(&owned, &fixture.context);
        let now = Instant::now();
        let deadline = now + plan::BUDGET;
        let clock = Rc::new(Cell::new(now));
        if deadline_case {
            fake.deadline_after_publication = Some((clock.clone(), deadline));
        } else {
            fake.cancel_after_publication = true;
        }
        let error = execute_with_clock(
            &fixture.context,
            owned,
            tools,
            &mut fake,
            deadline,
            &Time(clock),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, if deadline_case { 124 } else { 143 });
        assert!(retained.join("dtls.log").is_file());
        assert!(!retained.join("worker").exists());
        assert!(
            !fixture
                .context
                .temporary
                .join("native-dtls-verified")
                .exists()
        );
    }
}

#[tokio::test]
async fn exhausted_body_reserve_dispatches_no_installer_and_tail_projection_is_byte_bounded() {
    let fixture = Fixture::new();
    let owned = Owned::new(&fixture.context).unwrap();
    let tools = Tools::open(&fixture.context).unwrap();
    let mut fake = Fake::new(&owned, &fixture.context);
    let deadline = Instant::now() + plan::RESERVE;
    let code = execute_with_clock(
        &fixture.context,
        owned,
        tools,
        &mut fake,
        deadline,
        &Time(Rc::new(Cell::new(deadline - plan::RESERVE))),
    )
    .await
    .unwrap_err()
    .code;
    assert_eq!(code, 124);
    assert!(fake.calls.is_empty());
    assert_eq!(tail(b"one\ntwo\nthree\n", 2), b"two\nthree\n");
    assert_eq!(tail(b"one\ntwo\nthree", 2), b"two\nthree");
    assert_eq!(tail(b"single line", 60), b"single line");
}

struct NativeWithCapturedReplay<'a> {
    native: Native<'a>,
    output: Vec<u8>,
}
impl Executor for NativeWithCapturedReplay<'_> {
    async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        self.native.run(command).await
    }
    async fn ready(&mut self) -> Result<()> {
        self.native.ready().await
    }
    fn cleanup_passed(&self) -> bool {
        self.native.cleanup_passed()
    }
    async fn replay(&mut self, bytes: &[u8], _: bool, _: Instant) -> Result<()> {
        self.output.extend_from_slice(bytes);
        Ok(())
    }
}
#[tokio::test]
async fn actual_inert_selected_python_and_worker_receive_literal_argv_and_clean_environment() {
    use std::os::unix::fs::symlink;
    let mut fixture = Fixture::new();
    let capture = fixture.root.join("observed.jsonl");
    let worker_script = "#!/usr/bin/python3\nimport sys\nassert sys.argv[1:]==['[dtls-close]']\nprint('quiet owned worker fixture')\n";
    let interpreter = format!(
        r#"#!/usr/bin/python3
import json,os,sys
from pathlib import Path
args=sys.argv[1:]
with Path({capture}).open('a') as stream:
    stream.write(json.dumps({{'selected':sys.argv[0],'args':args,'python':os.environ['PYTHON'],
      'cc':os.environ['CC'],'cxx':os.environ['CXX'],'path':os.environ['PYTHONPATH'],
      'private':any(name in os.environ for name in ['PRIVATE_SENTINEL','PIP_EXTRA_INDEX_URL']),
      'cargo':any(name.startswith('CARGO_') for name in os.environ)}})+'\n')
assert args[:2]==['-m','pip'] or args[:2]==['-m','invoke']
if args[1]=='invoke':
    build=Path(os.environ['BUILD_DIR']+'-test');build.mkdir()
    binary=build/'mediasoup-worker-test';binary.write_text({worker});binary.chmod(0o700)
    print({diagnostic})
else: print('owned wheel-plan fixture; no installer executed')
"#,
        capture = serde_json::to_string(capture.to_str().unwrap()).unwrap(),
        worker = serde_json::to_string(worker_script).unwrap(),
        diagnostic = serde_json::to_string(DIAGNOSTIC).unwrap()
    );
    fs::write(fixture.root.join("bin/python named"), interpreter).unwrap();
    let venv = fixture.context.cwd.join("venv");
    fs::create_dir_all(venv.join("bin")).unwrap();
    fs::write(
        venv.join("pyvenv.cfg"),
        "owned inert config; never interpreted",
    )
    .unwrap();
    let selected = venv.join("bin/python");
    symlink(fixture.root.join("bin/python named"), &selected).unwrap();
    fixture.context.python = "./venv/bin/python".into();
    let owned = Owned::new(&fixture.context).unwrap();
    let prefix = owned.path.clone();
    let tools = Tools::open(&fixture.context).unwrap();
    let environment = fixture.context.clean_environment(&prefix);
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    let native = Native::new(
        &fixture.context.cwd,
        &environment,
        Signals::injected(receiver),
    );
    let mut runner = NativeWithCapturedReplay {
        native,
        output: vec![],
    };
    execute(
        &fixture.context,
        owned,
        tools,
        &mut runner,
        Instant::now() + plan::BUDGET,
    )
    .await
    .unwrap();
    let observations: Vec<serde_json::Value> = fs::read_to_string(capture)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(observations.len(), 2);
    for observed in observations {
        assert_eq!(
            observed["selected"],
            fixture
                .context
                .cwd
                .join("./venv/bin/python")
                .to_str()
                .unwrap()
        );
        assert_eq!(observed["python"], "./venv/bin/python");
        assert_eq!(observed["cc"], "clang");
        assert_eq!(observed["cxx"], "clang++");
        assert_eq!(
            observed["path"],
            prefix.join("out/pip_invoke").to_str().unwrap()
        );
        assert_eq!(observed["private"], false);
        assert_eq!(observed["cargo"], false);
    }
    assert!(!prefix.exists());
    assert!(runner.cleanup_passed());
    assert!(!runner.output.is_empty());
}
