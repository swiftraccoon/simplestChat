//! The actual unchanged Python module executes against a second owned tree.
//! Only filesystem semantics are compared; no installed runtime is invoked.
use super::{
    tests::{Fixture, capture, mode, revision},
    *,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, symlink},
    path::Path,
    process::Command,
};

const PYTHON: &str = r#"
import hashlib,json,os,pathlib,stat,sys
source=pathlib.Path(sys.argv[1]);request=json.loads(sys.argv[2])
for name,digest in request['sources'].items():
 assert hashlib.sha256((source/name).read_bytes()).hexdigest()==digest
sys.path.insert(0,str(source));import release_assets as a
root=pathlib.Path(request['root']);owner=os.geteuid()
def tree():
 out=[];inodes={}
 if not root.exists():return out
 for path in [root]+sorted(root.rglob('*')):
  m=path.lstat();key=(m.st_dev,m.st_ino)
  kind='directory' if stat.S_ISDIR(m.st_mode) else 'link' if stat.S_ISLNK(m.st_mode) else 'file'
  data=os.readlink(path) if kind=='link' else path.read_bytes().hex() if kind=='file' else ''
  if key not in inodes:inodes[key]=len(inodes)
  out.append(['' if path==root else str(path.relative_to(root)),kind,data,stat.S_IMODE(m.st_mode),m.st_nlink,inodes[key]])
 return out
rows=[]
for op in request['operations']:
 try:
  if op['action']=='initialize':result=a.initialize(root,owner=owner)
  elif op['action']=='publish':
   captures={rev:{name:bytes.fromhex(data) for name,data in files} for rev,files in op['captures']}
   result=a.publish(root,captures,op['now'],owner=owner)
  elif op['action']=='prune':result=a.prune(root,op['now'],owner=owner)
  elif op['action']=='select':result=a.select_version(root,op['revision'],owner=owner)
  elif op['action']=='mode':(root/op['path']).chmod(op['mode']);result=None
  elif op['action']=='version-link':
   path=root/'public/app-version.json';path.unlink();os.link(root/op['path'],path);result=None
  elif op['action']=='schema':
   index=root/'index.json';old=index.read_bytes();end=old.index(b',')
   new=b'{"schemaVersion":'+op['value'].encode()+old[end:]
   new=new.replace(b'"createdAt":0',b'"createdAt":'+op.get('created','0').encode())
   before='union-'+hashlib.sha256(old).hexdigest();after='union-'+hashlib.sha256(new).hexdigest()
   (root/before).rename(root/after);(root/'public/assets').unlink();(root/'public/assets').symlink_to('../'+after)
   index.write_bytes(new);result=None
  else:raise AssertionError('unknown fixture operation')
  rows.append({'passed':True,'result':result,'tree':tree()})
 except (OSError,ValueError) as error:
  rows.append({'passed':False,'tree':tree()})
print(json.dumps(rows,separators=(',',':')))
"#;
fn sources() -> Value {
    json!({
        "release_assets.py":"314c91956f13e72a867ac624c8027bf9c5fff740e953255dd5fbf2d87b6e72f2",
        "release_json.py":"0012aa8c99115d409a9955c11b675fab6683dfa91b5c633f75bde5ee67df7e97",
        "browser_assets.py":"6fd40b970008481a27660e3a5389ce942adab98cf093143055fa27ddbf72e12d",
        "release_public.py":"4d9a712aabee6432c98b2ebdd2b5a40c989deef5e0e16aa2c7c1dbd7284cb420",
    })
}
const BROWSER: &str = r#"
import hashlib,json,os,pathlib,sys
source=pathlib.Path(sys.argv[1]);request=json.loads(sys.argv[2])
for name,digest in request['sources'].items():
 assert hashlib.sha256((source/name).read_bytes()).hexdigest()==digest
sys.path.insert(0,str(source));import browser_assets as b;import release_public as p
fixture=pathlib.Path(request['root']);owner=os.geteuid()
p.ROOT=fixture/'srv/simplestchat-public';p.CONFIG=fixture/'etc/simplestchat-public';p.WORK=fixture/'run/simplestchat-bench'
p.ROOT_UID=owner;b.ROOT=p.ROOT;b.ROOT_UID=owner;b.time.time=lambda:100
sys.argv=['browser_assets',request['action']]
raise SystemExit(b.main())
"#;
fn browser(fixture: &Fixture, action: &str) -> std::process::Output {
    Command::new("python3")
        .args(["-c", BROWSER])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("ansible/files"))
        .arg(json!({"root":fixture.path,"action":action,"sources":sources()}).to_string())
        .output()
        .unwrap()
}
#[test]
fn actual_python_browser_command_uses_the_same_flock_and_receipts() {
    let fixture = Fixture::new();
    let held = fixture.guard();
    let output = browser(&fixture, "initialize");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"{\"passed\":true,\"busy\":true,\"changed\":false}\n"
    );
    assert!(!fixture.root().exists());
    held.finish().unwrap();
    for (action, expected) in [
        (
            "initialize",
            b"{\"passed\":true,\"changed\":true}\n".as_slice(),
        ),
        ("initialize", b"{\"passed\":true,\"changed\":false}\n"),
        ("prune", b"{\"passed\":true}\n"),
    ] {
        let output = browser(&fixture, action);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, expected);
        assert!(output.stderr.is_empty());
        let mut guard = fixture.guard();
        assert_eq!(
            Store::open(&mut guard)
                .unwrap()
                .inspect()
                .unwrap()
                .generations,
            0
        );
        guard.finish().unwrap();
    }
    let reference = Fixture::new();
    let output = browser(&reference, "prune");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{\"passed\":true}\n");
    let fresh = Fixture::new();
    let mut guard = fresh.guard();
    let mut store = Store::open(&mut guard).unwrap();
    store.initialize().unwrap();
    store.prune_at(100).unwrap();
    guard.finish().unwrap();
    let mut actual = tree(&fresh.root());
    let mut expected = tree(&reference.root());
    // Production managed admission is Linux-only. Its symlink permissions are
    // not a policy field. macOS exposes Python CLI umask077 versus the library
    // fixture's ambient umask on links; never change process-global umask here.
    for value in [&mut actual, &mut expected] {
        for row in value.as_array_mut().unwrap() {
            if row[1] == "link" {
                row[3] = Value::Null;
            }
        }
    }
    assert_eq!(actual, expected);
}
#[test]
fn actual_python_journal_text_gates_refuse_bom_and_wide_encodings_for_both_journals() {
    let text = r#"{"schemaVersion":1,"finalized":true,"note":"escaped\u0000private"}"#;
    let mut cases = vec![("utf8", text.as_bytes().to_vec(), true)];
    let mut bom = b"\xef\xbb\xbf".to_vec();
    bom.extend_from_slice(text.as_bytes());
    cases.push(("utf8-bom", bom, false));
    let mut raw = text.as_bytes().to_vec();
    raw.push(0);
    cases.push(("raw-nul", raw, false));
    let mut invalid = vec![0xff];
    invalid.extend_from_slice(text.as_bytes());
    cases.push(("invalid-utf8", invalid, false));
    for (label, little, bom) in [
        ("utf16le", true, false),
        ("utf16be", false, false),
        ("utf16le-bom", true, true),
        ("utf16be-bom", false, true),
    ] {
        let mut bytes = vec![];
        if bom {
            bytes.extend_from_slice(if little { b"\xff\xfe" } else { b"\xfe\xff" });
        }
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        cases.push((label, bytes, false));
    }
    for (label, little, bom) in [
        ("utf32le", true, false),
        ("utf32be", false, false),
        ("utf32le-bom", true, true),
        ("utf32be-bom", false, true),
    ] {
        let mut bytes = vec![];
        if bom {
            bytes.extend_from_slice(if little {
                b"\xff\xfe\0\0"
            } else {
                b"\0\0\xfe\xff"
            });
        }
        for unit in text.chars().map(u32::from) {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        cases.push((label, bytes, false));
    }
    for journal in [
        "run/simplestchat-bench/current.json",
        "srv/simplestchat-public/release-state.json",
    ] {
        for (label, bytes, expected) in &cases {
            let fixture = Fixture::new();
            fixture.guard().finish().unwrap();
            let path = fixture.path.join(journal);
            fs::write(&path, bytes).unwrap();
            mode(&path, 0o600);
            let native = match fixture.hook(None) {
                Ok(guard) => guard.finish().is_ok(),
                Err(_) => false,
            };
            let original = browser(&fixture, "initialize");
            assert_eq!(native, *expected, "{journal}: {label}");
            assert_eq!(original.status.success(), *expected, "{journal}: {label}");
            assert!(!String::from_utf8_lossy(&original.stdout).contains("private"));
            if !expected {
                assert!(!fixture.root().exists());
            }
        }
    }
}
fn tree(root: &Path) -> Value {
    fn visit(root: &Path, path: &Path, paths: &mut Vec<std::path::PathBuf>) {
        paths.push(path.into());
        if fs::symlink_metadata(path).unwrap().is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), paths);
            }
        }
        let _ = root;
    }
    if !root.exists() {
        return json!([]);
    }
    let mut paths = vec![];
    visit(root, root, &mut paths);
    paths.sort();
    let mut inodes = std::collections::BTreeMap::new();
    let mut output = vec![];
    for path in paths {
        let stamp = fs::symlink_metadata(&path).unwrap();
        let kind = if stamp.is_dir() {
            "directory"
        } else if stamp.is_symlink() {
            "link"
        } else {
            "file"
        };
        let data = if stamp.is_symlink() {
            fs::read_link(&path).unwrap().display().to_string()
        } else if stamp.is_file() {
            fs::read(&path)
                .unwrap()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        } else {
            String::new()
        };
        let next = inodes.len();
        let inode = *inodes.entry((stamp.dev(), stamp.ino())).or_insert(next);
        output.push(json!([
            path.strip_prefix(root).unwrap().display().to_string(),
            kind,
            data,
            stamp.mode() & 0o7777,
            stamp.nlink(),
            inode
        ]));
    }
    Value::Array(output)
}
pub(super) fn native(fixture: &Fixture, op: &Value) -> std::result::Result<Value, Failure> {
    if op["action"] == "mode" {
        mode(
            &fixture.root().join(op["path"].as_str().unwrap()),
            op["mode"].as_u64().unwrap() as u32,
        );
        return Ok(Value::Null);
    }
    if op["action"] == "version-link" {
        let version = fixture.root().join("public/app-version.json");
        fs::remove_file(&version).unwrap();
        fs::hard_link(fixture.root().join(op["path"].as_str().unwrap()), version).unwrap();
        return Ok(Value::Null);
    }
    if op["action"] == "schema" {
        let root = fixture.root();
        let path = root.join("index.json");
        let old = fs::read(&path).unwrap();
        let text = String::from_utf8(old.clone()).unwrap();
        let (_, tail) = text.split_once(',').unwrap();
        let new = format!(
            "{{\"schemaVersion\":{},{}",
            op["value"].as_str().unwrap(),
            tail
        )
        .replace(
            "\"createdAt\":0",
            &format!("\"createdAt\":{}", op["created"].as_str().unwrap_or("0")),
        );
        fs::rename(
            root.join(format!("union-{}", model::sha(&old))),
            root.join(format!("union-{}", model::sha(new.as_bytes()))),
        )
        .unwrap();
        fs::remove_file(root.join("public/assets")).unwrap();
        symlink(
            format!("../union-{}", model::sha(new.as_bytes())),
            root.join("public/assets"),
        )
        .unwrap();
        fs::write(path, new).unwrap();
        return Ok(Value::Null);
    }
    let mut guard = fixture.guard();
    let result = (|| {
        let mut store = Store::open(&mut guard)?;
        match op["action"].as_str().unwrap() {
            "initialize" => Ok(json!(store.initialize()?)),
            "publish" => {
                let captures = op["captures"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| {
                        let pairs = entry[1]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|item| {
                                let text = item[1].as_str().unwrap();
                                let bytes = (0..text.len())
                                    .step_by(2)
                                    .map(|index| {
                                        u8::from_str_radix(&text[index..index + 2], 16).unwrap()
                                    })
                                    .collect();
                                (item[0].as_str().unwrap().to_string(), bytes)
                            })
                            .collect();
                        (entry[0].as_str().unwrap().into(), Capture(pairs))
                    })
                    .collect();
                Ok(
                    serde_json::to_value(store.publish_at(captures, op["now"].as_u64().unwrap())?)
                        .unwrap(),
                )
            }
            "prune" => {
                store.prune_at(op["now"].as_u64().unwrap())?;
                Ok(Value::Null)
            }
            "select" => Ok(json!(
                store.select_version(op["revision"].as_str().unwrap())?
            )),
            _ => unreachable!(),
        }
    })();
    let finished = guard.finish();
    result.and_then(|value| finished.map(|()| value))
}
fn publish_op(rev: char, now: u64, entries: &[(&str, &[u8])]) -> Value {
    let files = entries
        .iter()
        .map(|(name, bytes)| {
            json!([
                name,
                bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ])
        })
        .collect::<Vec<_>>();
    json!({"action":"publish","now":now,"captures":[[revision(rev),files]]})
}
#[test]
fn actual_python_store_matches_complete_ordered_lifecycle_and_hardlink_graph() {
    let mut operations = vec![
        json!({"action":"initialize"}),
        json!({"action":"initialize"}),
        json!({"action":"select","revision":revision('f')}),
        publish_op(
            'a',
            0,
            &[("z-abcdef.js", b"same"), ("a-abcdef.css", b"style")],
        ),
        publish_op(
            'a',
            0,
            &[("z-abcdef.js", b"same"), ("a-abcdef.css", b"style")],
        ),
        json!({"action":"schema","value":"true","created":"-0"}),
        json!({"action":"initialize"}),
        json!({"action":"schema","value":"1.0"}),
        json!({"action":"initialize"}),
        publish_op(
            'a',
            1,
            &[("a-abcdef.css", b"style"), ("z-abcdef.js", b"same")],
        ),
        publish_op('b', 2, &[("z-abcdef.js", b"same")]),
        publish_op('c', 2, &[("other-abcdef.js", b"third")]),
        publish_op('d', 3, &[("fourth-abcdef.js", b"fourth")]),
        publish_op('e', 4, &[("fifth-abcdef.js", b"fifth")]),
        publish_op('f', 5, &[("z-abcdef.js", b"different")]),
        json!({"action":"mode","path":"public/app-version.json","mode":0o600}),
        json!({"action":"select","revision":revision('f')}),
        json!({"action":"prune","now":MAX_AGE+4}),
    ];
    // Several single and two-required captures exercise capacity/order without
    // duplicating Python's retention implementation in the oracle driver.
    operations.push(json!({"action":"publish","now":MAX_AGE+5,"captures":[
        [revision('a'),[["left-abcdef.js","6c656674"]]],
        [revision('b'),[["right-abcdef.js","7269676874"]]]]}));
    let version = format!("{{\"revision\":\"{}\"}}\n", revision('f'));
    operations.push(publish_op(
        'c',
        MAX_AGE + 6,
        &[("version-abcdef.js", version.as_bytes())],
    ));
    operations.push(
        json!({"action":"version-link","path":format!("{}/version-abcdef.js",revision('c'))}),
    );
    operations.push(json!({"action":"select","revision":revision('e')}));
    let reference = Fixture::new();
    let native_fixture = Fixture::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("ansible/files");
    let output = Command::new("python3")
        .args(["-c", PYTHON])
        .arg(source)
        .arg(
            json!({"root":reference.root(),"operations":operations,"sources":sources()})
                .to_string(),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    for (index, (op, expected)) in operations.iter().zip(expected).enumerate() {
        let result = native(&native_fixture, op);
        let actual = match result {
            Ok(value) => json!({"passed":true,"result":value,"tree":tree(&native_fixture.root())}),
            Err(_) => json!({"passed":false,"tree":tree(&native_fixture.root())}),
        };
        assert_eq!(actual, expected, "operation {index}: {op}");
    }
    // Confirm the public capture wrapper itself agrees with the model used by
    // these fixtures; actual tar parity belongs to the separate capture suite.
    assert_eq!(capture(&[("left-abcdef.js", b"left")]).files(), 1);
}
