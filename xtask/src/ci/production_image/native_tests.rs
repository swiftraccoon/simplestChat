//! Real owned fixture processes exercise the production supervisor, never Docker.

use super::super::process::Native;
use super::*;
use crate::ci::browser_suite::process::Signals;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}

fn executable(fixture: &mut Fixture, body: &str) {
    let directory = fixture.root.join("fake commands");
    fs::create_dir(&directory).unwrap();
    let source = format!(
        r#"#!/usr/bin/python3
import json,os,signal,sys,time
from pathlib import Path
root=Path(os.environ['FIXTURE_ROOT'])
args=sys.argv[1:]
with (root/'events').open('a') as output:
    output.write(json.dumps([Path(sys.argv[0]).name,args])+'\n')
{body}
"#
    );
    for program in ["git", "docker"] {
        let path = directory.join(program);
        fs::write(&path, &source).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fixture
        .context
        .environment
        .insert("PATH".into(), directory.into_os_string());
    fixture
        .context
        .environment
        .insert("FIXTURE_ROOT".into(), fixture.root.clone().into_os_string());
}

async fn wait(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn actual_fake_git_and_docker_execute_journal_build_cleanup_then_append() {
    let mut fixture = Fixture::new();
    executable(
        &mut fixture,
        r#"
if Path(sys.argv[0]).name=='git':
    if args==['rev-parse','HEAD']: print(os.environ['GITHUB_SHA'])
    elif args!=['status','--porcelain=v1','--untracked-files=all']: sys.exit(91)
elif args[:2]==['buildx','create']:
    name=args[args.index('--name')+1]
    token=name.removeprefix('simplestchat-ci-')
    journal=json.loads((Path(os.environ['RUNNER_TEMP'])/('production-builder-'+token)/'ownership.json').read_text())
    assert journal['builder']==name and journal['state']=='creation-planned'
elif args[:2]==['buildx','build']:
    assert args[args.index('--builder')+1].startswith('simplestchat-ci-')
    Path(args[args.index('--iidfile')+1]).write_text('sha256:'+'b'*64+'\n')
    destination=Path(os.environ['RUNNER_TEMP'])/'buildx-cache-next'
    destination.mkdir();(destination/'index.json').write_text('completed')
elif args[:2]==['image','inspect']:
    print(json.dumps({'Id':'sha256:'+'b'*64,'Config':{'User':'10001:10001',
        'Labels':{'org.opencontainers.image.revision':os.environ['GITHUB_SHA']},
        'Env':['SOURCE_REVISION='+os.environ['GITHUB_SHA']]}}))
elif args[:2]==['buildx','rm']:
    assert (root/'environment').read_text()=='EXISTING=value\n'
else: sys.exit(92)
"#,
    );
    let mut native = Native::new(&fixture.context, signals());
    execute::build(&fixture.context, &mut native).await.unwrap();
    assert_eq!(
        fixture.output(),
        format!("EXISTING=value\nPRODUCTION_IMAGE={IMAGE}\n")
    );
    let events: Vec<Value> = fs::read_to_string(fixture.root.join("events"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 8);
    assert_eq!(events[5][1][1], "rm");
    assert!(!fixture.root.join("literal").exists());
}

#[tokio::test]
async fn actual_command_exit_spawn_refusals_and_private_capture_fail_closed() {
    let mut fixture = Fixture::new();
    executable(
        &mut fixture,
        "print('PRIVATE-SENTINEL',file=sys.stderr)\nsys.exit(42)",
    );
    let mut native = Native::new(&fixture.context, signals());
    let error = native
        .run(Command::capture(Program::Docker, &["fixture"]))
        .await
        .unwrap_err();
    assert_eq!(error.code, 42);
    assert!(!error.message.contains("PRIVATE-SENTINEL"));
    fs::set_permissions(
        fixture.root.join("fake commands/docker"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(
        native
            .run(Command::capture(Program::Docker, &["fixture"]))
            .await
            .unwrap_err()
            .code,
        126
    );
    fs::remove_file(fixture.root.join("fake commands/docker")).unwrap();
    fixture.context.environment.insert(
        "PATH".into(),
        fixture.root.join("fake commands").into_os_string(),
    );
    let mut native = Native::new(&fixture.context, signals());
    assert_eq!(
        native
            .run(Command::capture(Program::Docker, &["fixture"]))
            .await
            .unwrap_err()
            .code,
        127
    );
}

#[tokio::test]
async fn timeout_and_interrupt_preserve_status_and_retire_owned_group() {
    for code in [124, 130, 143] {
        let mut fixture = Fixture::new();
        executable(
            &mut fixture,
            r#"
if args==['cleanup']:
    (root/'cleanup-ready').write_text('done');sys.exit(0)
signal.signal(signal.SIGTERM,signal.SIG_IGN)
signal.signal(signal.SIGINT,signal.SIG_IGN)
(root/'ready').write_text(str(os.getpid()))
while True: time.sleep(.01)
"#,
        );
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = Native::new(&fixture.context, Signals::injected(receiver));
        let mut request = Command::capture(Program::Docker, &["fixture"]);
        request.grace = Duration::from_millis(30);
        request.timeout = Duration::from_secs(3);
        if code == 124 {
            request.timeout = Duration::from_millis(200);
        }
        let signal = async {
            wait(&fixture.root.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let started = Instant::now();
        let (result, ()) = tokio::join!(native.run(request), signal);
        assert_eq!(result.unwrap_err().code, code);
        assert!(started.elapsed() < Duration::from_secs(4));
        let pid: i32 = fs::read_to_string(fixture.root.join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap())
                .is_err()
        );
        if code != 124 {
            assert_eq!(
                native
                    .run(Command::capture(Program::Docker, &["blocked"]))
                    .await
                    .unwrap_err()
                    .code,
                code
            );
            let mut cleanup = Command::capture(Program::Docker, &["cleanup"]);
            cleanup.cleanup = true;
            cleanup.timeout = Duration::from_secs(3);
            cleanup.grace = Duration::from_millis(20);
            native.run(cleanup).await.unwrap();
            assert!(fixture.root.join("cleanup-ready").exists());
            assert_eq!(
                fs::read_to_string(fixture.root.join("events"))
                    .unwrap()
                    .lines()
                    .count(),
                2
            );
        }
    }
}

#[tokio::test]
async fn output_bound_failure_reclaims_process_before_return_and_never_echoes_bytes() {
    let mut fixture = Fixture::new();
    executable(
        &mut fixture,
        r#"
(root/'ready').write_text(str(os.getpid()))
sys.stdout.write('PRIVATE-SENTINEL'*10000);sys.stdout.flush()
while True: time.sleep(.01)
"#,
    );
    let mut native = Native::new(&fixture.context, signals());
    let mut command = Command::capture(Program::Docker, &["fixture"]);
    command.capture = Some(16);
    command.grace = Duration::from_millis(30);
    let error = native.run(command).await.unwrap_err();
    assert_eq!(error.code, 1);
    assert!(!error.message.contains("PRIVATE-SENTINEL"));
    let pid: i32 = fs::read_to_string(fixture.root.join("ready"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_err()
    );
}

#[tokio::test]
async fn exited_leader_descendants_are_retired_without_signaling_unrelated_group() {
    use std::os::unix::process::CommandExt;
    let mut unrelated_command = std::process::Command::new("/bin/sleep");
    unrelated_command.arg("30").process_group(0);
    let mut unrelated = unrelated_command.spawn().unwrap();
    let mut fixture = Fixture::new();
    executable(
        &mut fixture,
        r#"
pid=os.fork()
if pid==0:
    signal.signal(signal.SIGTERM,signal.SIG_IGN)
    os.close(1);os.close(2)
    while True: time.sleep(.01)
(root/'descendant').write_text(str(pid))
"#,
    );
    let mut native = Native::new(&fixture.context, signals());
    let result = native
        .run(Command::capture(Program::Docker, &["fixture"]))
        .await;
    let unrelated_survived = unrelated.try_wait().unwrap().is_none();
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    result.unwrap();
    assert!(unrelated_survived);
    let pid: i32 = fs::read_to_string(fixture.root.join("descendant"))
        .unwrap()
        .parse()
        .unwrap();
    // Darwin may retain an already-dead orphan zombie briefly; the production
    // cleanup proof separately rejects live descendants and permits only zombies.
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let status = String::from_utf8(output.stdout).unwrap();
    assert!(status.trim().is_empty() || status.trim_start().starts_with('Z'));
}

#[tokio::test]
async fn interrupted_real_fake_verification_reclaims_inspected_container_after_cli_exit() {
    let mut fixture = Fixture::new();
    fixture.context.image = IMAGE.into();
    executable(
        &mut fixture,
        r#"
state=root/'container.json'
if Path(sys.argv[0]).name=='git':
    if args[0]=='rev-parse': print(os.environ['GITHUB_SHA'])
elif args[:2]==['image','inspect']:
    print(json.dumps({'Id':os.environ['PRODUCTION_IMAGE'],'Config':{'User':'10001:10001',
        'Labels':{'org.opencontainers.image.revision':os.environ['GITHUB_SHA']},
        'Env':['SOURCE_REVISION='+os.environ['GITHUB_SHA']]}}))
elif args[:2]==['container','create']:
    name=args[args.index('--name')+1]
    token=name.removeprefix('simplestchat-ci-verify-')
    intent=Path(os.environ['RUNNER_TEMP'])/('production-verification-'+token)/'ownership.json'
    assert json.loads(intent.read_text())['container']==name
    state.write_text(json.dumps({'Id':'d'*64,'Name':'/'+name,'Image':os.environ['PRODUCTION_IMAGE'],
        'Config':{'Labels':{'com.simplestchat.ci.production-verification':name}},
        'State':{'Running':False,'Status':'created','ExitCode':0}}))
    print('d'*64)
elif args[:2]==['container','inspect']:
    assert args[-1]=='d'*64
    print(state.read_text())
elif args[:2]==['container','start']:
    assert args==['container','start','--attach','d'*64]
    (root/'ready').write_text('started')
    while True: time.sleep(.01)
elif args[:2]==['container','ls']:
    if state.exists():
        assert 'id='+'d'*64 in args
        print('d'*64)
elif args[:2]==['container','rm']:
    assert args==['container','rm','--force','d'*64]
    state.unlink()
else: sys.exit(93)
"#,
    );
    fixture
        .context
        .environment
        .insert("PRODUCTION_IMAGE".into(), IMAGE.into());
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.context, Signals::injected(receiver));
    let cancel = async {
        wait(&fixture.root.join("ready")).await;
        sender.send(143).unwrap();
    };
    let (result, ()) = tokio::join!(execute::verify(&fixture.context, &mut native), cancel);
    assert_eq!(result.unwrap_err().code, 143);
    assert!(!fixture.root.join("container.json").exists());
    assert_eq!(fixture.output(), "EXISTING=value\n");
    let events: Vec<Value> = fs::read_to_string(fixture.root.join("events"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| event[1][1] == "rm"));
}
