//! Exact decoded source73 Bash scalar with owned fake tools; no Git runtime.
use super::{
    super::plan,
    fixture::{Fixture, REV},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};
const ORIGINAL: &[u8] = include_bytes!("reference-build.sh");
const SOURCE: &str = "d835ec261c15d926a32328bd7d7fcb0ec49257e0";
const WORKFLOW: &str = "0ffc53e450724e8e3d3ea7240530a9bcf32dc5d5a9a4624d159dc2ece2b7b921";
const RECORDER: &str = r#"#!/usr/bin/python3
import json,os,sys
from pathlib import Path
role=Path(sys.argv[0]).name
args=sys.argv[1:]
temp=Path(os.environ['RUNNER_TEMP'])
phase=role
if role=='docker':
 if args[:3]==['buildx','rm','--force']:
  phase='initial-rm' if not (temp/'builder-created').exists() else 'final-rm'
 elif args[:2]==['buildx','create']: phase='create'
 elif args[:2]==['buildx','build']: phase='build'
 elif args[0]=='save': phase='save'
 elif args[:2]==['image','inspect']: phase='inspect'
 else: raise AssertionError('unexpected fake Docker request')
event={'role':role,'phase':phase,'args':args,'cwd':os.getcwd()}
fd=os.open(os.environ['ORACLE_RECORD'],os.O_WRONLY|os.O_CREAT|os.O_APPEND,0o600)
os.write(fd,(json.dumps(event)+'\n').encode());os.close(fd)
if phase=='initial-rm': sys.exit(9)
if os.environ['ORACLE_FAILURE']==phase: sys.exit(int(os.environ['ORACLE_STATUS']))
if role=='git': print(os.environ['GITHUB_SHA'] if os.environ['ORACLE_FAILURE']!='head' else 'b'*40)
elif phase=='create': (temp/'builder-created').write_bytes(b'inert')
elif role=='timeout':
 assert args[:3]==['--signal=TERM','--kill-after=2m','40m']
 os.execv(str(Path(os.environ['PATH'])/'docker'),args[3:])
elif role=='mkdir':
 assert args==['-p',str(temp/'loadtest-image')]
 (temp/'loadtest-image').mkdir()
elif phase=='save': sys.stdout.buffer.write(b'inert saved tar bytes')
elif role=='zstd':
 assert args[:3]==['-T0','-3','-o']
 Path(args[3]).write_bytes(b'inert compressed '+sys.stdin.buffer.read())
elif phase=='inspect': print('sha256:'+'c'*64)
elif role=='ls': assert args==['-la',str(temp/'loadtest-image')]
"#;
fn bash() -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|path| path.join("bash"))
        .find(|path| {
            path.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .expect("installed Bash required by retained reference oracle")
        .canonicalize()
        .unwrap()
}
async fn reference(
    cache: bool,
    failure: &str,
    status: u8,
) -> (Fixture, Vec<serde_json::Value>, u8) {
    assert_eq!(SOURCE.len(), 40);
    assert_eq!(WORKFLOW.len(), 64);
    assert_eq!(ORIGINAL.len(), 1087);
    let sha: String = Sha256::digest(ORIGINAL)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        sha,
        "0e156c8cc97e2368f64c4c37da67e0f1b8a67382a19193d92db1a3b02889a48b"
    );
    let selected_bash = bash();
    let fixture = Fixture::new();
    for role in ["git", "docker", "timeout", "mkdir", "zstd", "ls"] {
        let path = fixture.root.join("bin").join(role);
        fs::write(&path, RECORDER).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    if cache {
        let path = fixture.context.temporary.join("buildx-cache");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("index.json"), b"{}").unwrap();
    }
    let script = fixture.root.join("reference.sh");
    fs::write(&script, ORIGINAL).unwrap();
    let record = fixture.root.join("oracle.ndjson");
    let mut command = Command::new(selected_bash);
    command
        .args(["--noprofile", "--norc"])
        .arg(&script)
        .current_dir(&fixture.root)
        .env_clear()
        .envs(&fixture.context.environment)
        .env("ORACLE_RECORD", &record)
        .env("ORACLE_FAILURE", failure)
        .env("ORACLE_STATUS", status.to_string())
        .stderr(Stdio::null());
    let (status, _) = crate::test_env::helper_with_signals(
        command,
        Duration::from_secs(5),
        Some(64 * 1024),
        None,
    )
    .await
    .unwrap();
    let bytes = fs::read(record).unwrap();
    assert!(bytes.len() <= 64 * 1024);
    let events = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (fixture, events, u8::try_from(status).unwrap())
}
#[tokio::test]
async fn actual_retained_shell_fixed_recipe_cache_and_exact_artifacts_match_owner() {
    for cache in [false, true] {
        let (fixture, events, status) = reference(cache, "", 0).await;
        assert_eq!(status, 0);
        let build = events
            .iter()
            .find(|event| event["phase"] == "build")
            .unwrap();
        let context = &fixture.context;
        let command = context.build("unix:///owned/docker.sock", "nonce-owned", cache);
        let mut args = command
            .arguments
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(&args[..2], ["--host", "unix:///owned/docker.sock"]);
        args.drain(..2);
        let builder = args.iter().position(|v| v == "--builder").unwrap();
        args.drain(builder..builder + 2);
        assert_eq!(build["args"], serde_json::json!(args));
        assert_eq!(build["cwd"], fixture.root.to_str().unwrap());
        let directory = context.temporary.join("loadtest-image");
        assert_eq!(
            fs::read(directory.join("image-id.txt")).unwrap(),
            format!("sha256:{}\n", "c".repeat(64)).as_bytes()
        );
        assert_eq!(
            fs::read(directory.join(context.archive_name())).unwrap(),
            b"inert compressed inert saved tar bytes"
        );
        assert_eq!(fs::read_dir(directory).unwrap().count(), 2);
        assert!(events.iter().any(|event| event["phase"] == "timeout"));
        assert_eq!(context.revision, REV);
        assert_eq!(plan::TAG, "simplestchat-loadtest:ci");
    }
}
#[tokio::test]
async fn actual_retained_shell_keeps_known_failures_and_never_reaches_later_inspection() {
    for (phase, status) in [
        ("head", 1),
        ("create", 41),
        ("build", 42),
        ("timeout", 124),
        ("final-rm", 43),
        ("save", 44),
        ("zstd", 45),
        ("inspect", 46),
    ] {
        let (_fixture, events, observed) = reference(false, phase, status).await;
        assert_eq!(observed, status, "{phase}");
        assert!(!events.iter().any(|event| event["phase"] == "ls"));
        if !matches!(phase, "inspect") {
            assert!(!events.iter().any(|event| event["phase"] == "inspect"));
        }
    }
}
