//! The retained Python helper is the reference; subprocess tests use only
//! synthetic identity and owned files. No installed helper or host check runs.

use super::{
    tests::{FixtureHost, write},
    *,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    process::{Command, Stdio},
};

const ORACLE: &str = r#"
import contextlib,importlib.util,json,os,pathlib,platform,stat,sys,tempfile,types
from unittest.mock import patch
spec=importlib.util.spec_from_file_location('owned_preflight',sys.argv[1])
p=importlib.util.module_from_spec(spec);spec.loader.exec_module(p)
original_lstat=pathlib.Path.lstat
def denied(event,args):
 if event in ('subprocess.Popen','os.system','socket.connect','socket.bind'):
  raise AssertionError('forbidden runtime operation')
sys.addaudithook(denied)
output=[]
for case in json.load(sys.stdin):
 with tempfile.TemporaryDirectory(prefix='ops-preflight-oracle-') as rawroot:
  outer=pathlib.Path(rawroot).resolve();root=outer/'host';root.mkdir();root.chmod(0o755)
  for name,mode in [('etc',0o755),('srv',0o755),('usr',0o755),('usr/lib',0o755),
    ('usr/local',0o755),('usr/local/libexec',0o755),
    ('usr/local/libexec/simplestchat-public',0o755),('etc/simplestchat-public',0o700),
    ('srv/simplestchat-public',0o700),('srv/simplestchat-public/releases',0o700)]:
   path=root/name;path.mkdir();path.chmod(mode)
  def write(name,data,mode):
   path=root/name;path.write_bytes(data);path.chmod(mode)
  write('etc/os-release',case.get('release','ID=debian\nVERSION_ID=13\n').encode(),0o644)
  write('etc/simplestchat-public/images.json',b'PRIVATE_NOT_JSON\xff\n',0o600)
  for name in p.BASE_HELPERS|p.FETCH_HELPERS|p.FORCE_HELPERS:
   write('usr/local/libexec/simplestchat-public/'+name,b'PRIVATE_INSTALLED_HELPER_CONTENT\n',0o644)
  change=case.get('change');name=case.get('path');path=root/name if name is not None else None
  if change=='mode':path.chmod(case['mode'])
  if change=='size':path.write_bytes(b'x'*case['size'])
  if change=='missing':path.unlink() if path.is_file() else path.rmdir()
  if change=='digest':path.write_bytes(b'CHANGED_INSTALLED_HELPER_CONTENT\n')
  if change=='link':
   retained=path.with_name(path.name+'.retained');path.rename(retained);path.symlink_to(retained)
  if change=='oslink':
   (root/'etc/os-release').rename(root/'usr/lib/os-release')
   (root/'etc/os-release').symlink_to('../usr/lib/os-release')
  if change=='fallback':(root/'etc/os-release').rename(root/'usr/lib/os-release')
  external=set(root.parents)
  def metadata(path):
   if path in external:
    return types.SimpleNamespace(st_uid=0,st_gid=0,st_mode=stat.S_IFDIR|0o755,st_size=0)
   value=original_lstat(path)
   uid,gid=case.get('owner',[0,0]) if change=='owner' and path==root/name else (0,0)
   return types.SimpleNamespace(st_uid=uid,st_gid=gid,st_mode=value.st_mode,st_size=value.st_size)
  def snapshot():
   return [(str(x.relative_to(root)),original_lstat(x).st_mode,original_lstat(x).st_ino,
     original_lstat(x).st_size,original_lstat(x).st_mtime_ns,original_lstat(x).st_ctime_ns,
     x.read_bytes() if x.is_file() and not x.is_symlink() else None)
     for x in [root,*sorted(root.rglob('*'))]]
  before=snapshot()
  with contextlib.ExitStack() as stack:
   for key,value in [('CONFIG',root/'etc/simplestchat-public'),
    ('ROOT',root/'srv/simplestchat-public'),
    ('HELPERS',root/'usr/local/libexec/simplestchat-public')]:
    stack.enter_context(patch.object(p,key,value))
   stack.enter_context(patch.object(os,'geteuid',return_value=case.get('euid',0)))
   stack.enter_context(patch.object(platform,'system',return_value=case.get('system','Linux')))
   stack.enter_context(patch.object(platform,'machine',return_value=case.get('machine','x86_64')))
   stack.enter_context(patch.object(platform,'_os_release_candidates',
     (str(root/'etc/os-release'),str(root/'usr/lib/os-release'))))
   stack.enter_context(patch.object(platform,'_os_release_cache',None))
   run_id='01010101010141018101010101010101'
   stack.enter_context(patch.object(p.uuid,'uuid4',lambda:types.SimpleNamespace(hex=run_id)))
   stack.enter_context(patch.object(pathlib.Path,'lstat',metadata))
   try:result=p.check(json.loads(case['request']));record={'receipt':result}
   except p.PreflightError as error:record={'failure':str(error)}
   except (OSError,ValueError):record={'failure':'preflight_unavailable'}
  assert snapshot()==before
  assert not any(x.name=='__pycache__' for x in root.rglob('*'))
  output.append(record)
print(json.dumps(output))
"#;

fn cases() -> Vec<Value> {
    let host = FixtureHost::new();
    let base = host.request(&[]);
    let mut result = vec![
        json!({"request":"{}"}),
        json!({"request":base}),
        json!({"request":host.request(&request::FETCH)}),
        json!({"request":host.request(&["force_release.py"])}),
    ];
    for raw in ["[]", "null", "true", "{\"private\":null}"] {
        result.push(json!({"request":raw}));
    }
    for value in [
        Value::Null,
        json!(true),
        json!(4),
        json!("A".repeat(64)),
        json!("a".repeat(63)),
    ] {
        let mut raw: Value = serde_json::from_str(&base).unwrap();
        raw["release-public.py"] = value;
        result.push(json!({"request":raw.to_string()}));
    }
    for (key, values) in [
        ("euid", vec![json!(1)]),
        ("system", vec![json!("Darwin")]),
        ("machine", vec![json!("aarch64")]),
        (
            "release",
            vec![
                json!("ID=ubuntu\nVERSION_ID=13\n"),
                json!("ID=debian\nVERSION_ID=12\n"),
                json!("ID=debian\n"),
                json!("ID=debian\nVERSION_ID=13.\n"),
                json!("ID='debian'\rVERSION_ID=\"13.1\"\r\n"),
                json!("ID=ubuntu\nID=debian\nVERSION_ID=13..suffix\n"),
                json!("ID=debian \nVERSION_ID=13\n"),
            ],
        ),
    ] {
        for value in values {
            let mut case = json!({"request":base});
            case[key] = value;
            result.push(case);
        }
    }
    for (path, mode) in [
        ("etc/simplestchat-public", 0o700),
        ("srv/simplestchat-public", 0o700),
        ("srv/simplestchat-public/releases", 0o700),
        ("usr/local", 0o755),
        ("usr/local/libexec/simplestchat-public", 0o755),
        (
            "usr/local/libexec/simplestchat-public/release-public.py",
            0o644,
        ),
        ("etc/simplestchat-public/images.json", 0o600),
    ] {
        for extra in [0o002, 0o1000, 0o4000] {
            result.push(json!({"request":base,"change":"mode","path":path,"mode":mode|extra}));
        }
        for owner in [[1, 0], [0, 1]] {
            result.push(json!({"request":base,"change":"owner","path":path,"owner":owner}));
        }
    }
    for (path, limit) in [
        ("etc/simplestchat-public/images.json", 65536),
        (
            "usr/local/libexec/simplestchat-public/release-public.py",
            1024 * 1024,
        ),
    ] {
        for size in [0, limit + 1] {
            result.push(json!({"request":base,"change":"size","path":path,"size":size}));
        }
    }
    for change in ["digest", "missing", "link"] {
        result.push(json!({"request":base,"change":change,
            "path":"usr/local/libexec/simplestchat-public/release-public.py"}));
    }
    for change in ["oslink", "fallback"] {
        result.push(json!({"request":base,"change":change}));
    }
    result
}

fn rust(case: &Value) -> Value {
    let mut host = FixtureHost::new();
    if let Some(value) = case.get("euid") {
        host.euid = value.as_u64().unwrap() as u32;
    }
    if let Some(value) = case.get("system") {
        host.system = value.as_str().unwrap().as_bytes().to_vec();
    }
    if let Some(value) = case.get("machine") {
        host.machine = value.as_str().unwrap().as_bytes().to_vec();
    }
    if let Some(value) = case.get("release") {
        write(
            &host.root.join("etc/os-release"),
            value.as_str().unwrap().as_bytes(),
            0o644,
        );
    }
    let path = case
        .get("path")
        .and_then(Value::as_str)
        .map(|name| host.root.join(name));
    match case.get("change").and_then(Value::as_str) {
        Some("mode") => fs::set_permissions(
            path.unwrap(),
            fs::Permissions::from_mode(case["mode"].as_u64().unwrap() as u32),
        )
        .unwrap(),
        Some("owner") => {
            host.access.0.borrow_mut().owners.insert(
                case["path"].as_str().unwrap().into(),
                (
                    case["owner"][0].as_u64().unwrap() as u32,
                    case["owner"][1].as_u64().unwrap() as u32,
                ),
            );
        }
        Some("size") => fs::write(
            path.unwrap(),
            vec![b'x'; case["size"].as_u64().unwrap() as usize],
        )
        .unwrap(),
        Some("missing") => fs::remove_file(path.unwrap()).unwrap(),
        Some("digest") => fs::write(path.unwrap(), b"CHANGED_INSTALLED_HELPER_CONTENT\n").unwrap(),
        Some("link") => {
            let path = path.unwrap();
            let retained = path.with_extension("retained");
            fs::rename(&path, &retained).unwrap();
            symlink(retained, path).unwrap();
        }
        Some("oslink" | "fallback") => {
            fs::rename(
                host.root.join("etc/os-release"),
                host.root.join("usr/lib/os-release"),
            )
            .unwrap();
            if case["change"] == "oslink" {
                symlink("../usr/lib/os-release", host.root.join("etc/os-release")).unwrap();
            }
        }
        _ => {}
    }
    let result = match host.check(case["request"].as_str().unwrap()) {
        Ok(receipt) => json!({"receipt":receipt}),
        Err(error) => json!({"failure":error.code()}),
    };
    host.settled();
    result
}

#[test]
fn actual_python_reference_matches_complete_host_contract_and_public_failures() {
    let cases = cases();
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ansible/files/release_preflight.py");
    let mut child = Command::new("python3")
        .args(["-I", "-B", "-c", ORACLE])
        .arg(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&cases).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(cases.len() >= 65);
    for (index, (case, expected)) in cases.iter().zip(expected).enumerate() {
        assert_eq!(rust(case), expected, "case {index}");
    }
}
