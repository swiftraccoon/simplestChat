//! Inert tar fixtures and the actual unchanged Python reader. No engine,
//! application, network socket or extracted tar path is used.

use super::*;
use crate::test_support::Fixture;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Cursor, Write},
    os::unix::fs::symlink,
    process::{Command, Stdio},
};

const NAME: &str = "media-previous123.js";
const CONTENT: &[u8] = b"export const previous = true;";
#[derive(Clone)]
struct Entry {
    name: String,
    kind: u8,
    bytes: Vec<u8>,
    link: Option<&'static str>,
}
fn entry(name: &str, kind: u8, bytes: &[u8]) -> Entry {
    Entry {
        name: name.into(),
        kind,
        bytes: bytes.into(),
        link: None,
    }
}
fn asset() -> Entry {
    entry(&format!("assets/{NAME}"), b'0', CONTENT)
}
fn archive(entries: &[Entry]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for entry in entries {
        assert!(entry.name.len() <= 100);
        let mut header = tar::Header::new_gnu();
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..entry.name.len()].copy_from_slice(entry.name.as_bytes());
        header.set_entry_type(tar::EntryType::new(entry.kind));
        header.as_mut_bytes()[156] = entry.kind;
        header.set_mode(0o644);
        header.set_size(entry.bytes.len() as u64);
        if let Some(link) = entry.link {
            header.set_link_name(link).unwrap();
        }
        header.set_cksum();
        builder.append(&header, Cursor::new(&entry.bytes)).unwrap();
    }
    builder.into_inner().unwrap()
}
fn read_bytes(bytes: &[u8]) -> Result<model::Capture> {
    let fixture = Fixture::new();
    let path = fixture.0.join("capture.tar");
    fs::write(&path, bytes).unwrap();
    read(&path)
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn projection(capture: &model::Capture) -> Value {
    json!({"passed":true,"files":capture.0.iter().map(|(name,bytes)|
        json!({"name":name,"bytes":bytes.len(),"sha256":digest(bytes)})).collect::<Vec<_>>()})
}
fn pax(key: &str, value: &str) -> Vec<u8> {
    let body = format!(" {key}={value}\n");
    let mut length = body.len() + 1;
    loop {
        let next = body.len() + length.to_string().len();
        if next == length {
            return format!("{length}{body}").into_bytes();
        }
        length = next;
    }
}

#[test]
fn ordered_flat_payloads_accept_regular_contiguous_and_directory_root_aliases() {
    for root in ["assets", "assets/", "assets///"] {
        let values = [
            entry(root, b'5', b""),
            entry("assets/zeta-abcdef.css", b'7', b"body{}"),
            asset(),
            entry("assets/alpha-abcdef.js", 0, b"ok"),
        ];
        let capture = read_bytes(&archive(&values)).unwrap();
        assert_eq!(capture.0[0].0, "zeta-abcdef.css");
        assert_eq!(capture.0[1], (NAME.into(), CONTENT.into()));
        assert_eq!(capture.0[2].0, "alpha-abcdef.js");
        assert_eq!(capture.files(), 3);
        assert_eq!(capture.bytes(), (CONTENT.len() + 8) as u64);
    }
}

#[test]
fn paths_links_special_files_and_duplicate_effective_names_are_refused() {
    for name in [
        "../private",
        "/assets/media-previous123.js",
        "assets/../media-previous123.js",
        "assets/nested/media-previous123.js",
        "assets/index.html",
        "assets/plain.js",
        "assets/media-previous123.js.map",
        "assets/media-previous123.js/",
        "assets/media-previous123.js\\escape",
        "assets/media-private123.json",
    ] {
        assert!(
            read_bytes(&archive(&[entry(name, b'0', CONTENT)])).is_err(),
            "{name}"
        );
    }
    for kind in *b"123456SgK" {
        assert!(read_bytes(&archive(&[entry("assets/media-previous123.js", kind, b"")])).is_err());
    }
    let mut linked = asset();
    linked.link = Some("private");
    assert!(read_bytes(&archive(&[linked])).is_err());
    for entries in [
        vec![asset(), asset()],
        vec![
            entry("assets/", b'5', b""),
            entry("assets", b'5', b""),
            asset(),
        ],
        vec![
            entry("assets///", b'5', b""),
            entry("assets", b'5', b""),
            asset(),
        ],
    ] {
        assert_eq!(
            read_bytes(&archive(&entries)).unwrap_err().code(),
            "asset_tar_duplicate"
        );
    }
}

#[test]
fn local_name_extensions_are_bounded_before_interpretation_and_cannot_change_sizes() {
    let long = format!("assets/{}-abcdef.js", "a".repeat(130));
    let mut terminated = long.as_bytes().to_vec();
    terminated.push(0);
    for extension in [
        entry("././@LongLink", b'L', &terminated),
        entry("././@PaxHeader", b'x', &pax("path", &long)),
    ] {
        let result = read_bytes(&archive(&[extension, asset()])).unwrap();
        assert_eq!(
            result.0,
            vec![(long.strip_prefix("assets/").unwrap().into(), CONTENT.into())]
        );
    }
    let cases = [
        vec![
            entry("pax", b'x', &vec![b'x'; MAX_EXTENSION as usize + 1]),
            asset(),
        ],
        vec![entry("pax", b'x', &pax("size", "1")), asset()],
        vec![entry("pax", b'x', &pax("GNU.sparse.size", "1")), asset()],
        vec![
            entry("pax", b'x', &[pax("path", "a"), pax("path", "b")].concat()),
            asset(),
        ],
        vec![
            entry("pax", b'x', &pax("path", &long)),
            entry("long", b'L', &terminated),
            asset(),
        ],
        vec![entry("long", b'L', &terminated)],
        vec![
            entry("long", b'L', b"assets/a-abcdef.js\0hidden\0"),
            asset(),
        ],
        vec![entry("pax", b'x', b"\n12 path=a\n"), asset()],
    ];
    for entries in cases {
        assert!(read_bytes(&archive(&entries)).is_err());
    }
    let too_long = format!("assets/{}-abcdef.js", "a".repeat(250));
    assert!(
        read_bytes(&archive(&[
            entry("pax", b'x', &pax("path", &too_long)),
            asset(),
        ]))
        .is_err()
    );
}

#[test]
fn payload_count_total_file_and_physical_transfer_bounds_are_enforced() {
    let entries = (0..MAX_FILES)
        .map(|index| entry(&format!("assets/file{index}-abcdef.js"), b'0', b"x"))
        .collect::<Vec<_>>();
    assert_eq!(read_bytes(&archive(&entries)).unwrap().files(), MAX_FILES);
    let mut too_many = entries.clone();
    too_many.push(entry("assets/extra-abcdef.js", b'0', b"x"));
    assert!(read_bytes(&archive(&too_many)).is_err());
    let large = vec![b'x'; MAX_FILE as usize];
    assert_eq!(
        read_bytes(&archive(&[entry("assets/large-abcdef.js", b'0', &large)]))
            .unwrap()
            .bytes(),
        MAX_FILE
    );
    assert!(
        read_bytes(&archive(&[entry(
            "assets/large-abcdef.js",
            b'0',
            &vec![b'x'; MAX_FILE as usize + 1]
        )]))
        .is_err()
    );
    let all = (0..4)
        .map(|index| entry(&format!("assets/large{index}-abcdef.js"), b'0', &large))
        .collect::<Vec<_>>();
    assert_eq!(read_bytes(&archive(&all)).unwrap().bytes(), MAX_TOTAL);
    let mut excess = all;
    excess.push(entry("assets/extra-abcdef.js", b'0', b"x"));
    assert!(read_bytes(&archive(&excess)).is_err());
    let fixture = Fixture::new();
    let path = fixture.0.join("oversized.tar");
    fs::File::create(&path)
        .unwrap()
        .set_len(MAX_TAR + 1)
        .unwrap();
    assert_eq!(read(&path).unwrap_err().code(), "asset_tar_size");
    assert!(read_bytes(&archive(&[])).is_err());
    assert!(read_bytes(&archive(&[entry("assets", b'5', b"")])).is_err());
    assert!(read_bytes(&archive(&[entry("assets/empty-abcdef.js", b'0', b"")])).is_err());
}

#[test]
fn truncated_headers_payloads_and_linked_inputs_never_become_captures() {
    let bytes = archive(&[asset()]);
    for size in [0, 1, 511, 512, 512 + CONTENT.len() - 1, 1023] {
        assert!(read_bytes(&bytes[..size]).is_err(), "length {size}");
    }
    let mut broken = bytes.clone();
    broken[0] ^= 1;
    assert!(read_bytes(&broken).is_err());
    let fixture = Fixture::new();
    let real = fixture.0.join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("capture.tar"), bytes).unwrap();
    symlink(real.join("capture.tar"), fixture.0.join("leaf.tar")).unwrap();
    symlink(&real, fixture.0.join("alias")).unwrap();
    assert!(read(&fixture.0.join("leaf.tar")).is_err());
    assert!(read(&fixture.0.join("alias/capture.tar")).is_err());
    assert!(read(&real).is_err());
}

const PYTHON: &str = r#"
import hashlib,json,pathlib,sys
sys.dont_write_bytecode=True
sys.path.insert(0,sys.argv[1])
def denied(event,args):
 if event in ('subprocess.Popen','os.system','socket.connect','socket.bind'):
  raise AssertionError('forbidden operation')
sys.addaudithook(denied)
import release_assets as assets
rows=[]
for path in json.load(sys.stdin):
 try:
  capture=assets.read_capture(pathlib.Path(path))
  rows.append({'passed':True,'files':[{'name':name,'bytes':len(value),
   'sha256':hashlib.sha256(value).hexdigest()} for name,value in capture.items()]})
 except (OSError,ValueError):rows.append({'passed':False})
print(json.dumps(rows))
"#;

#[test]
fn actual_unchanged_python_reader_matches_payload_order_hashes_and_supported_refusals() {
    let reference = Path::new(env!("CARGO_MANIFEST_DIR")).join("ansible/files");
    for (name, expected) in [
        (
            "release_assets.py",
            "314c91956f13e72a867ac624c8027bf9c5fff740e953255dd5fbf2d87b6e72f2",
        ),
        (
            "release_json.py",
            "0012aa8c99115d409a9955c11b675fab6683dfa91b5c633f75bde5ee67df7e97",
        ),
    ] {
        assert_eq!(digest(&fs::read(reference.join(name)).unwrap()), expected);
    }
    let mut cases = Vec::new();
    for root in [None, Some("assets"), Some("assets/"), Some("assets///")] {
        for kind in [b'0', 0, b'7'] {
            let mut entries = root
                .map(|name| vec![entry(name, b'5', b"")])
                .unwrap_or_default();
            entries.push(entry("assets/zeta-abcdef.css", kind, b"body{}"));
            entries.push(asset());
            cases.push(archive(&entries));
        }
    }
    for name in [
        "../private",
        "/assets/media-previous123.js",
        "assets/../media-previous123.js",
        "assets/nested/media-previous123.js",
        "assets/index.html",
        "assets/plain.js",
        "assets/media-previous123.js.map",
        "assets/media-previous123.js/",
        "assets/media-previous123.js\\escape",
        "assets/media-private123.json",
    ] {
        cases.push(archive(&[entry(name, b'0', CONTENT)]));
    }
    for kind in *b"123456" {
        cases.push(archive(&[entry("assets/media-previous123.js", kind, b"")]));
    }
    cases.push(archive(&[asset(), asset()]));
    cases.push(archive(&[
        entry("assets/", b'5', b""),
        entry("assets", b'5', b""),
        asset(),
    ]));
    cases.push(archive(&[]));
    cases.push(archive(&[entry("assets/empty-abcdef.js", b'0', b"")]));
    let mut linked = asset();
    linked.link = Some("private");
    cases.push(archive(&[linked]));
    let long = format!("assets/{}-abcdef.js", "a".repeat(130));
    let mut name = long.as_bytes().to_vec();
    name.push(0);
    cases.push(archive(&[entry("././@LongLink", b'L', &name), asset()]));
    cases.push(archive(&[entry("pax", b'x', &pax("path", &long)), asset()]));
    let fixture = Fixture::new();
    let mut paths = Vec::new();
    let mut actual = Vec::new();
    for (number, bytes) in cases.iter().enumerate() {
        let path = fixture.0.join(format!("capture{number}.tar"));
        fs::write(&path, bytes).unwrap();
        actual
            .push(read(&path).map_or_else(|_| json!({"passed":false}), |value| projection(&value)));
        paths.push(path);
    }
    let mut child = Command::new("python3")
        .args(["-I", "-B", "-c", PYTHON])
        .arg(reference)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&paths).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(expected.len() >= 35);
    assert_eq!(actual.len(), expected.len());
    for (number, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(*actual, expected, "reference case {number}");
    }
}
