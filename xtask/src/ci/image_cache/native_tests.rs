//! The shared native owner runs only PATH-confined fake metadata commands.

use super::*;
use crate::ci::{browser_suite::process::Signals, process::Native};

fn executable(fixture: &mut Fixture, mode: &str) {
    let bin = fixture.root.join("fake commands");
    fs::create_dir(&bin).unwrap();
    let path = bin.join("python3");
    fs::write(&path, r#"#!/usr/bin/python3
import json,os,signal,sys,time
from pathlib import Path
root=Path(os.environ['FIXTURE_ROOT']);args=sys.argv[1:];mode=os.environ['FIXTURE_MODE']
assert args[0]=='build/security_image_cache.py'
stage='prefix' if args[1:]==['--prefix'] else 'hint' if len(args)==1 else 'status'
with (root/'events').open('a') as out:out.write(json.dumps(args)+'\n')
if mode==stage+'-hang':
    signal.signal(signal.SIGINT,signal.SIG_IGN);signal.signal(signal.SIGTERM,signal.SIG_IGN)
    (root/'ready').write_text(str(os.getpid()))
    while True:time.sleep(.01)
if mode==stage+'-fail':
    print('PRIVATE-SENTINEL',file=sys.stderr);sys.exit(42)
prefix='grype-db-v2-main-'+'a'*64+'-'
if mode==stage+'-overflow':print('x'*4097)
elif mode==stage+'-invalid':print('PRIVATE-SENTINEL\nprivate=injected')
elif stage=='prefix':print(prefix)
else:
    if stage=='status':
        assert args[1]=='--status'
        assert args[2] in [str(root/'runner temp/image-database-preparation/database-status.json'),str(root/'runner temp/production-security/image/database-status.json')]
    print(prefix+'b'*64)
"#).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    for (key, value) in [
        ("PATH", bin.into_os_string()),
        ("FIXTURE_ROOT", fixture.root.clone().into_os_string()),
        ("FIXTURE_MODE", mode.into()),
    ] {
        fixture.context.environment.insert(key.into(), value);
    }
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}

#[tokio::test]
async fn actual_fake_python_processes_preserve_success_and_both_status_paths() {
    for operation in [
        Operation::Hint,
        Operation::Preparation,
        Operation::Production,
    ] {
        let mut fixture = Fixture::new();
        executable(&mut fixture, "");
        let mut native = Native::new(&fixture.root, &fixture.context.environment, signals());
        execute::execute(&fixture.context, operation, &mut native, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(fixture.root.join("events"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert!(
            String::from_utf8(fixture.bytes())
                .unwrap()
                .contains(&format!("key={}\n", key("main")))
        );
        assert!(native.cleanup_passed());
        assert!(!fixture.root.join("unowned").exists());
    }
}

#[tokio::test]
async fn actual_hint_failure_falls_back_but_prefix_status_and_invalid_output_failures_do_not() {
    for (mode, operation, succeeds, count) in [
        ("hint-fail", Operation::Hint, true, 2),
        ("hint-overflow", Operation::Hint, true, 2),
        ("prefix-fail", Operation::Hint, false, 1),
        ("prefix-overflow", Operation::Hint, false, 1),
        ("prefix-invalid", Operation::Hint, false, 1),
        ("hint-invalid", Operation::Hint, false, 2),
        ("status-fail", Operation::Preparation, false, 2),
        ("status-fail", Operation::Production, false, 2),
    ] {
        let mut fixture = Fixture::new();
        executable(&mut fixture, mode);
        let mut native = Native::new(&fixture.root, &fixture.context.environment, signals());
        let result =
            execute::execute(&fixture.context, operation, &mut native, &mut Vec::new()).await;
        assert_eq!(result.is_ok(), succeeds, "{mode}");
        if succeeds {
            assert_eq!(
                fixture.bytes(),
                format!(
                    "prior=preserved\nkey={}\nprefix={}\n",
                    prefix("main"),
                    prefix("main")
                )
                .as_bytes()
            );
        } else {
            assert_eq!(fixture.bytes(), b"prior=preserved\n");
        }
        assert_eq!(
            fs::read_to_string(fixture.root.join("events"))
                .unwrap()
                .lines()
                .count(),
            count
        );
        assert!(native.cleanup_passed());
    }
}

struct Short<'a>(Native<'a>);
impl Executor for Short<'_> {
    fn cleanup_passed(&self) -> bool {
        self.0.cleanup_passed()
    }
    async fn ready(&mut self) -> Result<()> {
        self.0.ready().await
    }
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        command.timeout = Duration::from_secs(2);
        command.grace = Duration::from_millis(30);
        self.0.run(command).await
    }
}
async fn ready(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancellation_never_becomes_a_hint_miss_and_timeout_fallback_requires_reaped_group() {
    for (code, stage) in [(130, "hint"), (143, "hint"), (124, "hint"), (124, "prefix")] {
        let mut fixture = Fixture::new();
        executable(&mut fixture, &format!("{stage}-hang"));
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = Short(Native::new(
            &fixture.root,
            &fixture.context.environment,
            Signals::injected(receiver),
        ));
        let cancel = async {
            ready(&fixture.root.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let mut warnings = Vec::new();
        let (result, ()) = tokio::join!(
            execute::execute(
                &fixture.context,
                Operation::Hint,
                &mut native,
                &mut warnings
            ),
            cancel
        );
        if code == 124 && stage == "hint" {
            result.unwrap();
            assert_eq!(
                fixture.bytes(),
                format!(
                    "prior=preserved\nkey={}\nprefix={}\n",
                    prefix("main"),
                    prefix("main")
                )
                .as_bytes()
            );
        } else {
            assert_eq!(result.unwrap_err().code, code);
            assert_eq!(fixture.bytes(), b"prior=preserved\n");
        }
        assert_eq!(warnings.is_empty(), !(code == 124 && stage == "hint"));
        assert!(native.cleanup_passed());
        let pid: i32 = fs::read_to_string(fixture.root.join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap())
                .is_err()
        );
    }
}
