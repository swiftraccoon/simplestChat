//! Owned inert tar/JSON fixtures and the maintained Python implementation oracle.

use super::*;
use crate::test_support::Fixture;
use serde_json::{Value, json};
use std::fs;
use std::io::{Cursor, Write};
use std::process::{Command, Stdio};

const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TAG: &str = "simplestchat-release/production:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";

#[derive(Clone)]
struct Entry {
    name: String,
    kind: u8,
    bytes: Vec<u8>,
}
fn entry(name: &str, bytes: impl AsRef<[u8]>) -> Entry {
    Entry {
        name: name.into(),
        kind: b'0',
        bytes: bytes.as_ref().into(),
    }
}
fn tar(entries: &[Entry]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for value in entries {
        let mut header = tar::Header::new_gnu();
        if value.name.len() <= 100 {
            header.as_mut_bytes()[..100].fill(0);
            header.as_mut_bytes()[..value.name.len()].copy_from_slice(value.name.as_bytes());
        } else {
            header.set_path(&value.name).unwrap();
        }
        header.set_mode(0o600);
        header.set_size(value.bytes.len() as u64);
        header.set_entry_type(tar::EntryType::new(value.kind));
        header.as_mut_bytes()[156] = value.kind;
        header.set_cksum();
        builder.append(&header, Cursor::new(&value.bytes)).unwrap();
    }
    builder.into_inner().unwrap()
}
fn config() -> Value {
    json!({"architecture":"amd64","os":"linux",
        "rootfs":{"type":"layers","diff_ids":["sha256:private-unverified"]},
        "config":{"User":"10001:10001","Cmd":["/app/simplestChat"],"Entrypoint":null,
        "Labels":{"org.opencontainers.image.revision":REVISION},
        "Env":["PRIVATE_SECRET=never_print"]}})
}
fn docker(config: Value) -> Vec<Entry> {
    vec![
        entry(
            "manifest.json",
            json!([{"Config":"config.json","RepoTags":[TAG],"Layers":["layer.tar"]}]).to_string(),
        ),
        entry("config.json", config.to_string()),
        entry("layer.tar", b"inert fixture layer"),
    ]
}
fn read_value(entries: &[Entry], name: &str) -> Value {
    serde_json::from_slice(&entries.iter().find(|e| e.name == name).unwrap().bytes).unwrap()
}
fn replace_value(entries: &mut [Entry], name: &str, value: Value) {
    entries.iter_mut().find(|e| e.name == name).unwrap().bytes = value.to_string().into();
}
fn manifest(bytes: &[u8]) -> Manifest {
    Manifest::decode(
        json!({"schemaVersion":1,"revision":REVISION,"platform":"linux/amd64",
        "archiveSha256":sha256(bytes),"imageTag":TAG,"migrations":{"1":"c".repeat(96)},
        "createdAt":"2026-10-21T01:02:03Z"})
        .to_string()
        .as_bytes(),
    )
    .unwrap()
}
fn verify(entries: &[Entry]) -> Result<VerifiedArchive> {
    let root = Fixture::new();
    let path = root.0.join("image.tar");
    let bytes = tar(entries);
    fs::write(&path, &bytes).unwrap();
    verify_archive(&path, &manifest(&bytes))
}
fn descriptor(files: &mut Vec<Entry>, bytes: Vec<u8>) -> Value {
    let hash = sha256(&bytes);
    let size = bytes.len();
    files.push(entry(&format!("blobs/sha256/{hash}"), bytes));
    json!({"digest":format!("sha256:{hash}"),"size":size})
}
fn oci(
    depth: usize,
    outer: impl FnOnce(&mut Value),
    image_manifest: impl FnOnce(&mut Value),
    cfg: Value,
) -> Vec<Entry> {
    let mut files = Vec::new();
    let config = descriptor(&mut files, cfg.to_string().into());
    let layer = descriptor(&mut files, b"inert OCI layer".to_vec());
    let mut image = json!({"schemaVersion":2,"config":config,"layers":[layer]});
    image_manifest(&mut image);
    let mut current = descriptor(&mut files, image.to_string().into());
    current["mediaType"] = json!(OCI_MANIFEST);
    for _ in 0..depth {
        current = descriptor(
            &mut files,
            json!({"schemaVersion":2,"manifests":[current]})
                .to_string()
                .into(),
        );
        current["mediaType"] = json!(OCI_INDEX);
    }
    current["annotations"] = json!({"io.containerd.image.name":TAG});
    let mut index = json!({"schemaVersion":2,"manifests":[current]});
    outer(&mut index);
    files.push(entry("index.json", index.to_string()));
    files.push(entry("oci-layout", b"{\"imageLayoutVersion\":\"1.0.0\"}"));
    let config_path = config["digest"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let layer_path = layer["digest"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    files.push(entry(
        "manifest.json",
        json!([{
            "Config":format!("blobs/sha256/{config_path}"), "RepoTags":[TAG],
            "Layers":[format!("blobs/sha256/{layer_path}")]
        }])
        .to_string(),
    ));
    files
}

fn oracle(cases: &[Vec<u8>]) {
    let root = Fixture::new();
    let mut requests = Vec::new();
    let mut actual = Vec::new();
    for (number, bytes) in cases.iter().enumerate() {
        let path = root.0.join(format!("image-{number}.tar"));
        fs::write(&path, bytes).unwrap();
        let manifest = manifest(bytes);
        requests.push(json!({"path":path,"manifest":manifest.fields()}));
        actual.push(match verify_archive(&path, &manifest) {
            Ok(_) => json!({"ok":true}),
            Err(error) => json!({"ok":false,"error":error.code()}),
        });
    }
    let script = r#"
import json,pathlib,sys
sys.path.insert(0,sys.argv[1]+'/ops/ansible/files')
import release_artifact as p
errors={
'Image archive must be a bounded regular file':'archive_input_refused',
'Image archive SHA256 does not match release manifest':'archive_digest_mismatch',
'Cannot read Docker image archive':'archive_unreadable',
'Invalid Docker archive member path':'archive_member_path',
'Noncanonical Docker archive member path':'archive_member_noncanonical',
'Duplicate or nonregular Docker archive member':'archive_member_type_or_duplicate',
'Too many Docker archive members':'archive_member_count',
'Missing or oversized Docker image metadata':'archive_metadata_missing_or_oversized',
'Unreadable Docker image metadata':'archive_unreadable',
'Invalid artifact JSON':'invalid_json',
'Archive must contain exactly one Docker image':'archive_image_count',
'Archive must contain exactly the declared release tag':'archive_tag',
'Docker image has no layers':'archive_layers',
'Docker image layer is missing or nonregular':'archive_layer_member',
'Unsupported OCI image layout':'oci_layout',
'Invalid OCI index':'oci_index',
'OCI index must reference exactly one image':'oci_index_count',
'Invalid OCI annotations':'oci_annotations',
'OCI index tag does not match release':'oci_annotation_tag',
'OCI index has no declared release tag':'oci_annotation_missing',
'Invalid OCI descriptor digest':'oci_descriptor_digest',
'OCI descriptor size or member mismatch':'oci_descriptor_size',
'External OCI descriptor URLs are unsupported':'oci_descriptor_urls',
'OCI metadata digest mismatch':'oci_metadata_digest',
'Unsupported OCI image manifest':'oci_image_manifest',
'Docker and OCI image configurations disagree':'oci_config_mismatch',
'Docker and OCI image layers disagree':'oci_layers_mismatch',
'OCI image index nesting is too deep':'oci_index_depth',
'Legacy Docker repository tags do not match release':'archive_repositories',
'Docker image platform does not match linux/amd64':'archive_platform',
'Docker image must run as 10001:10001':'archive_user',
'Docker image revision label does not match':'archive_revision',
'Docker image must start only the production server':'archive_command'}
output=[]
for request in json.load(sys.stdin):
 try:
  p.verify_archive(pathlib.Path(request['path']),request['manifest']);output.append({'ok':True})
 except p.ArtifactError as error:
  text=str(error)
  code='ambiguous_json' if text.startswith('Duplicate JSON key:') else errors[text]
  output.append({'ok':False,'error':code})
json.dump(output,sys.stdout)
"#;
    let mut child = Command::new("python3")
        .args(["-I", "-c", script])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&requests).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(expected.len(), actual.len());
    for (number, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(actual, expected, "oracle case {number}");
    }
    eprintln!("checked {} actual Python archive cases", cases.len());
}

#[test]
fn docker_policy_and_failure_order_match_the_actual_python_reader() {
    let mut cases = vec![tar(&docker(config()))];
    for (key, values) in [
        ("os", vec![json!("freebsd"), Value::Null]),
        ("architecture", vec![json!("arm64"), json!(false)]),
        ("config", vec![Value::Null, json!([]), json!({})]),
    ] {
        for value in values {
            let mut cfg = config();
            cfg[key] = value;
            cases.push(tar(&docker(cfg)));
        }
    }
    for (key, values) in [
        ("User", vec![json!("root"), json!(10001), Value::Null]),
        (
            "Labels",
            vec![
                Value::Null,
                json!({}),
                json!({"org.opencontainers.image.revision":"b".repeat(40)}),
            ],
        ),
        (
            "Cmd",
            vec![
                Value::Null,
                json!([]),
                json!(["/app/simplestChat", "extra"]),
                json!("/app/simplestChat"),
            ],
        ),
        (
            "Entrypoint",
            vec![
                Value::Null,
                json!([]),
                json!(["/bin/sh"]),
                json!(false),
                json!({}),
            ],
        ),
    ] {
        for value in values {
            let mut cfg = config();
            cfg["config"][key] = value;
            cases.push(tar(&docker(cfg)));
        }
    }
    for value in [
        Value::Null,
        json!({}),
        json!([]),
        json!([null]),
        json!([{}, {}]),
    ] {
        let mut files = docker(config());
        replace_value(&mut files, "manifest.json", value);
        cases.push(tar(&files));
    }
    for (key, values) in [
        (
            "RepoTags",
            vec![
                Value::Null,
                json!([]),
                json!([TAG, "other"]),
                json!(["wrong"]),
            ],
        ),
        (
            "Layers",
            vec![
                Value::Null,
                json!([]),
                json!(["absent"]),
                json!([false]),
                json!(["layer.tar/"]),
            ],
        ),
        (
            "Config",
            vec![
                Value::Null,
                json!("absent"),
                json!("../config.json"),
                json!("config.json/"),
            ],
        ),
    ] {
        for value in values {
            let mut files = docker(config());
            let mut image = read_value(&files, "manifest.json");
            image[0][key] = value;
            replace_value(&mut files, "manifest.json", image);
            cases.push(tar(&files));
        }
    }
    for repositories in [
        json!({"simplestchat-release/production":{REVISION:null}}),
        json!({"simplestchat-release/production":{REVISION:{"private":true}}}),
        json!({}),
        json!([]),
        json!({"wrong":{REVISION:"ignored"}}),
        json!({"simplestchat-release/production":{REVISION:"ignored","extra":"ignored"}}),
    ] {
        let mut files = docker(config());
        files.push(entry("repositories", repositories.to_string()));
        cases.push(tar(&files));
    }
    for bytes in [
        b"{\"nested\":[{\"key\":1,\"\\u006bey\":2}]}".as_slice(),
        b"{\"ignored\":1e400}",
        b"{invalid",
        b"",
    ] {
        let mut files = docker(config());
        files[1].bytes = bytes.into();
        cases.push(tar(&files));
    }
    // Full indexing precedes manifest decoding; repository validation precedes runtime checks.
    let mut files = docker(config());
    files[0].bytes = b"not json".to_vec();
    files.push(entry("../escape", b"x"));
    cases.push(tar(&files));
    let mut cfg = config();
    cfg["os"] = json!("bad");
    let mut files = docker(cfg);
    files.push(entry("repositories", b"{}"));
    cases.push(tar(&files));
    oracle(&cases);
}

#[test]
fn oci_descriptor_traversal_types_falsey_urls_and_error_order_match_python() {
    let mut cases = Vec::new();
    for depth in 0..=9 {
        cases.push(tar(&oci(depth, |_| {}, |_| {}, config())));
    }
    for value in [
        json!(2),
        json!(2.0),
        json!(true),
        json!(false),
        json!("2"),
        Value::Null,
        json!(3),
    ] {
        cases.push(tar(&oci(
            0,
            |v| v["schemaVersion"] = value,
            |_| {},
            config(),
        )));
    }
    for value in [
        Value::Null,
        json!({}),
        json!([]),
        json!([null]),
        json!([{}, {}]),
    ] {
        cases.push(tar(&oci(0, |v| v["manifests"] = value, |_| {}, config())));
    }
    for value in [
        Value::Null,
        json!({}),
        json!([]),
        json!(false),
        json!({"io.containerd.image.name":null,"org.opencontainers.image.ref.name":null}),
        json!({"io.containerd.image.name":format!("docker.io/{TAG}")}),
        json!({"org.opencontainers.image.ref.name":REVISION}),
        json!({"org.opencontainers.image.ref.name":TAG}),
        json!({"org.opencontainers.image.ref.name":"wrong"}),
        json!({"io.containerd.image.name":false}),
        json!({"io.containerd.image.name":TAG,"private":"allowed"}),
    ] {
        cases.push(tar(&oci(
            0,
            |v| v["manifests"][0]["annotations"] = value,
            |_| {},
            config(),
        )));
    }
    for (key, values) in [
        (
            "digest",
            vec![
                Value::Null,
                json!(false),
                json!("sha256:wrong"),
                json!(format!("sha256:{}", "A".repeat(64))),
                json!(format!("sha256:{}", "b".repeat(64))),
            ],
        ),
        (
            "size",
            vec![
                Value::Null,
                json!(false),
                json!(true),
                json!(0),
                json!(-1),
                json!(2.0),
                json!(9007199254740993_u64),
            ],
        ),
        (
            "urls",
            vec![
                Value::Null,
                json!(false),
                json!(0),
                json!(-0.0),
                json!(""),
                json!([]),
                json!({}),
                json!(["https://PRIVATE.invalid"]),
                json!(true),
                json!(1),
                json!("private"),
                json!({"url":"private"}),
            ],
        ),
        (
            "mediaType",
            vec![
                Value::Null,
                json!(false),
                json!("unsupported"),
                json!("application/vnd.docker.distribution.manifest.v2+json"),
                json!(OCI_INDEX),
            ],
        ),
    ] {
        for value in values {
            cases.push(tar(&oci(
                0,
                |v| v["manifests"][0][key] = value,
                |_| {},
                config(),
            )));
        }
    }
    for value in [json!(2.0), json!(false), Value::Null, json!(3)] {
        cases.push(tar(&oci(
            0,
            |_| {},
            |v| v["schemaVersion"] = value,
            config(),
        )));
    }
    for value in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"digest":"wrong","size":0}),
    ] {
        cases.push(tar(&oci(0, |_| {}, |v| v["config"] = value, config())));
    }
    for (key, value) in [
        ("size", json!(true)),
        ("urls", json!(["https://PRIVATE.invalid"])),
        ("mediaType", json!("ignored")),
    ] {
        cases.push(tar(&oci(0, |_| {}, |v| v["config"][key] = value, config())));
    }
    for value in [
        Value::Null,
        json!({}),
        json!([]),
        json!([null]),
        json!([{"digest":"wrong","size":0}]),
    ] {
        cases.push(tar(&oci(0, |_| {}, |v| v["layers"] = value, config())));
    }
    for value in [
        Value::Null,
        json!({}),
        json!({"imageLayoutVersion":"2.0.0"}),
        json!({"imageLayoutVersion":"1.0.0","extra":true}),
    ] {
        let mut files = oci(0, |_| {}, |_| {}, config());
        replace_value(&mut files, "oci-layout", value);
        cases.push(tar(&files));
    }
    for missing in ["oci-layout", "index.json"] {
        let mut files = oci(0, |_| {}, |_| {}, config());
        files.retain(|e| e.name != missing);
        cases.push(tar(&files));
    }
    // Parse descriptor JSON before checking its hash; annotations precede descriptor lookup.
    for payload in [
        b"{invalid".as_slice(),
        b"{\"x\":1,\"x\":2}",
        b"{\"x\":1e400}",
        b"{}",
    ] {
        let mut files = oci(0, |_| {}, |_| {}, config());
        let index = read_value(&files, "index.json");
        let digest = index["manifests"][0]["digest"].as_str().unwrap();
        let path = format!("blobs/sha256/{}", digest.strip_prefix("sha256:").unwrap());
        files.iter_mut().find(|e| e.name == path).unwrap().bytes = payload.into();
        let mut index = index;
        index["manifests"][0]["size"] = json!(payload.len());
        replace_value(&mut files, "index.json", index);
        cases.push(tar(&files));
    }
    cases.push(tar(&oci(
        0,
        |v| {
            v["manifests"][0]["annotations"] = json!({"io.containerd.image.name":"wrong"});
            v["manifests"][0]["digest"] = json!("wrong");
        },
        |_| {},
        config(),
    )));
    // Repeated layer entries remain allowed; their declared payload hash is not recomputed.
    let mut files = oci(0, |_| {}, |_| {}, config());
    let image = read_value(&files, "manifest.json");
    let layer = image[0]["Layers"][0].as_str().unwrap();
    let value = files.iter_mut().find(|e| e.name == layer).unwrap();
    value.bytes.fill(b'X');
    cases.push(tar(&files));
    let mut files = oci(0, |_| {}, |_| {}, config());
    let mut image = read_value(&files, "manifest.json");
    image[0]["Layers"] = json!(["layer.tar"]);
    replace_value(&mut files, "manifest.json", image);
    files.push(entry("layer.tar", b"inert"));
    cases.push(tar(&files));
    oracle(&cases);
}

#[test]
fn original_config_bytes_and_archive_identity_remain_private_and_exact() {
    let entries = docker(config());
    let bytes = tar(&entries);
    let verified = verify(&entries).unwrap();
    assert_eq!(verified.config_bytes(), entries[1].bytes);
    let result = serde_json::to_value(verified.identity()).unwrap();
    assert_eq!(
        result,
        json!({"archiveSha256":sha256(&bytes),"archiveBytes":bytes.len(),
        "configSha256":sha256(&entries[1].bytes),"revision":REVISION,
        "imageTag":TAG,"platform":"linux/amd64"})
    );
    assert!(!result.to_string().contains("PRIVATE_SECRET"));
    assert!(!result.to_string().contains("Env"));
    assert!(result.get("passed").is_none());
}

fn pax(fields: &[(&str, &[u8])]) -> Entry {
    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append_pax_extensions(fields.iter().copied())
        .unwrap();
    let bytes = builder.into_inner().unwrap();
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut bytes = Vec::new();
    archive
        .entries()
        .unwrap()
        .raw(true)
        .next()
        .unwrap()
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    Entry {
        name: "PaxHeader".into(),
        kind: b'x',
        bytes,
    }
}

#[test]
fn canonical_member_paths_types_collisions_and_supported_extensions_match_python() {
    let mut cases = Vec::new();
    for name in [
        "../escape",
        "a/../b",
        "/absolute",
        "a//b",
        "a/./b",
        ".",
        "./name",
        "back\\slash",
        "control\u{1}",
        "allowed\u{7f}",
        "unicode-☃",
    ] {
        let mut files = docker(config());
        files.push(entry(name, b"inert"));
        cases.push(tar(&files));
    }
    for kind in [b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'9', 0] {
        let mut files = docker(config());
        let mut extra = entry("extra", b"");
        extra.kind = kind;
        files.push(extra);
        cases.push(tar(&files));
    }
    for name in ["manifest.json", "layer.tar/", "layer.tar///"] {
        let mut files = docker(config());
        files.push(entry(name, b"duplicate"));
        cases.push(tar(&files));
    }
    let mut files = docker(config());
    let mut dir = entry("layer.tar/", b"");
    dir.kind = b'5';
    files.push(dir);
    cases.push(tar(&files));
    // Path overrides and ordinary metadata extensions use the mature decoder.
    for format in *b"xL" {
        let mut files = docker(config());
        let name = format!("metadata/{}", "a".repeat(220));
        let mut image = read_value(&files, "manifest.json");
        image[0]["Config"] = json!(name);
        replace_value(&mut files, "manifest.json", image);
        files[1].name = "placeholder".into();
        let extension = if format == b'x' {
            pax(&[("path", name.as_bytes()), ("mtime", b"12345.5")])
        } else {
            Entry {
                name: "././@LongLink".into(),
                kind: b'L',
                bytes: format!("{name}\0").into(),
            }
        };
        files.insert(1, extension);
        cases.push(tar(&files));
    }
    let mut files = docker(config());
    let size = files[1].bytes.len().to_string();
    files.insert(
        1,
        pax(&[
            ("size", size.as_bytes()),
            ("uid", b"123"),
            ("comment", b"ignored"),
        ]),
    );
    cases.push(tar(&files));
    oracle(&cases);
}

#[test]
fn explicit_extension_domain_refusals_precede_unbounded_mature_reader_allocations() {
    for extension in [
        pax(&[("path", b"config.json"), ("path", b"config.json")]),
        pax(&[("GNU.sparse.map", b"0,4")]),
        pax(&[("size", b"1")]),
        Entry {
            name: "global".into(),
            kind: b'g',
            bytes: pax(&[("comment", b"global")]).bytes,
        },
        Entry {
            name: "sparse".into(),
            kind: b'S',
            bytes: Vec::new(),
        },
        Entry {
            name: "link".into(),
            kind: b'K',
            bytes: b"private\0".to_vec(),
        },
    ] {
        let mut files = docker(config());
        files.insert(1, extension);
        assert_eq!(verify(&files).unwrap_err(), ArchiveError::UnsupportedTar);
    }
    let mut files = docker(config());
    files.insert(1, pax(&[("path", b"config.json")]));
    files.insert(1, pax(&[("mtime", b"1")]));
    assert_eq!(verify(&files).unwrap_err(), ArchiveError::UnsupportedTar);
    let mut files = docker(config());
    files.push(pax(&[("path", b"future")]));
    assert_eq!(verify(&files).unwrap_err(), ArchiveError::UnsupportedTar);
    let mut files = docker(config());
    files.insert(
        1,
        Entry {
            name: "long".into(),
            kind: b'L',
            bytes: vec![b'a'; 8193],
        },
    );
    assert_eq!(verify(&files).unwrap_err(), ArchiveError::ExtensionBudget);
    let mut files = docker(config());
    let mut extension = pax(&[("path", b"config.json")]);
    extension.bytes.insert(0, b'\n');
    files.insert(1, extension);
    assert_eq!(verify(&files).unwrap_err(), ArchiveError::UnsupportedTar);
}

#[test]
fn independent_resource_budgets_are_enforced_at_their_exact_boundaries() {
    let root = Fixture::new();
    let path = root.0.join("image.tar");
    let files = docker(config());
    let bytes = tar(&files);
    let manifest = manifest(&bytes);
    fs::write(&path, &bytes).unwrap();
    let limits = Limits {
        archive: bytes.len() as u64,
        members: 3,
        headers: 3,
        name: 13,
        names: 33,
        metadata: files[1].bytes.len() as u64,
        ..Limits::default()
    };
    assert!(verify_after(&path, &manifest, limits, |_| {}).is_ok());
    for (changed, error) in [
        (
            Limits {
                archive: bytes.len() as u64 - 1,
                ..limits
            },
            ArchiveError::Input,
        ),
        (
            Limits {
                members: 2,
                ..limits
            },
            ArchiveError::MemberCount,
        ),
        (
            Limits {
                headers: 2,
                ..limits
            },
            ArchiveError::PhysicalCount,
        ),
        (Limits { name: 12, ..limits }, ArchiveError::NameBudget),
        (
            Limits {
                names: 32,
                ..limits
            },
            ArchiveError::NameBudget,
        ),
        (
            Limits {
                metadata: files[1].bytes.len() as u64 - 1,
                ..limits
            },
            ArchiveError::Metadata,
        ),
    ] {
        assert_eq!(
            verify_after(&path, &manifest, changed, |_| {}).unwrap_err(),
            error
        );
    }
    let extension = pax(&[("comment", b"ignored")]);
    let size = extension.bytes.len() as u64;
    let mut files = files;
    files.insert(1, extension);
    let bytes = tar(&files);
    fs::write(&path, &bytes).unwrap();
    let manifest = super::tests::manifest(&bytes);
    assert!(
        verify_after(
            &path,
            &manifest,
            Limits {
                extension: size,
                ..Limits::default()
            },
            |_| {}
        )
        .is_ok()
    );
    assert_eq!(
        verify_after(
            &path,
            &manifest,
            Limits {
                extension: size - 1,
                ..Limits::default()
            },
            |_| {}
        )
        .unwrap_err(),
        ArchiveError::ExtensionBudget
    );
}

#[test]
fn transfer_hash_precedes_tar_and_changes_in_every_phase_refuse_success() {
    let root = Fixture::new();
    let path = root.0.join("image.tar");
    let bytes = tar(&docker(config()));
    let manifest = manifest(&bytes);
    fs::write(&path, b"not a tar").unwrap();
    assert_eq!(
        verify_archive(&path, &manifest).unwrap_err(),
        ArchiveError::Digest
    );
    for phase in ["hash", "index", "validation"] {
        for operation in ["write", "replace", "unlink"] {
            fs::write(&path, &bytes).unwrap();
            let result = verify_after(&path, &manifest, Limits::default(), |current| {
                if current == phase {
                    match operation {
                        "write" => {
                            let mut changed = bytes.clone();
                            changed[0] ^= 1;
                            fs::write(&path, changed).unwrap();
                        }
                        "replace" => {
                            let replacement = root.0.join("replacement");
                            fs::write(&replacement, &bytes).unwrap();
                            fs::rename(replacement, &path).unwrap();
                        }
                        "unlink" => fs::remove_file(&path).unwrap(),
                        _ => unreachable!(),
                    }
                }
            });
            assert_eq!(
                result.unwrap_err(),
                ArchiveError::Changed,
                "{phase}/{operation}"
            );
        }
    }
}

#[test]
fn truncated_or_unreadable_tar_never_becomes_an_archive_identity() {
    let mut cases = vec![b"not a tar".to_vec(), vec![0; 1024]];
    let mut bytes = tar(&docker(config()));
    bytes[0] ^= 1;
    cases.push(bytes);
    let bytes = tar(&docker(config()));
    cases.push(bytes[..bytes.len() - 1200].to_vec());
    oracle(&cases);
    let root = Fixture::new();
    let path = root.0.join("truncated.tar");
    let mut header = tar::Header::new_gnu();
    header.set_path("huge").unwrap();
    header.set_size(8 * 1024 * 1024 * 1024);
    header.set_cksum();
    let bytes = header.as_bytes().to_vec();
    fs::write(&path, &bytes).unwrap();
    assert_eq!(
        verify_archive(&path, &manifest(&bytes)).unwrap_err(),
        ArchiveError::Read
    );
}

#[test]
fn full_one_mib_metadata_limit_and_original_encoding_are_enforced() {
    let mut cases = Vec::new();
    let mut files = docker(config());
    files[1].bytes.resize(1024 * 1024, b' ');
    cases.push(tar(&files));
    files[1].bytes.push(b' ');
    cases.push(tar(&files));
    for little in [true, false] {
        let mut files = docker(config());
        let text = String::from_utf8(files[1].bytes.clone()).unwrap();
        let mut bytes = Vec::new();
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        files[1].bytes = bytes;
        cases.push(tar(&files));
    }
    let mut files = docker(config());
    files[2].bytes.clear();
    cases.push(tar(&files));
    let mut cfg = config();
    cfg["config"].as_object_mut().unwrap().remove("Entrypoint");
    cases.push(tar(&docker(cfg)));
    oracle(&cases);
}

#[test]
fn strict_descriptor_numbers_and_ordered_multiple_layers_match_python() {
    let mut cases = Vec::new();
    cases.push(tar(&oci(
        0,
        |v| {
            let size = v["manifests"][0]["size"].as_f64().unwrap();
            v["manifests"][0]["size"] = json!(size);
        },
        |_| {},
        config(),
    )));
    cases.push(tar(&oci(
        0,
        |_| {},
        |v| {
            let size = v["config"]["size"].as_f64().unwrap();
            v["config"]["size"] = json!(size);
        },
        config(),
    )));
    cases.push(tar(&oci(
        0,
        |_| {},
        |v| {
            let size = v["layers"][0]["size"].as_f64().unwrap();
            v["layers"][0]["size"] = json!(size);
        },
        config(),
    )));
    let mut files = oci(0, |_| {}, |_| {}, config());
    let second = descriptor(&mut files, b"second inert layer".to_vec());
    let mut index = read_value(&files, "index.json");
    let name = format!(
        "blobs/sha256/{}",
        index["manifests"][0]["digest"]
            .as_str()
            .unwrap()
            .strip_prefix("sha256:")
            .unwrap()
    );
    let mut image = read_value(&files, &name);
    image["layers"].as_array_mut().unwrap().push(second.clone());
    let updated = descriptor(&mut files, image.to_string().into());
    for key in ["digest", "size"] {
        index["manifests"][0][key] = updated[key].clone();
    }
    replace_value(&mut files, "index.json", index);
    let mut image = read_value(&files, "manifest.json");
    image[0]["Layers"]
        .as_array_mut()
        .unwrap()
        .push(json!(format!(
            "blobs/sha256/{}",
            second["digest"]
                .as_str()
                .unwrap()
                .strip_prefix("sha256:")
                .unwrap()
        )));
    replace_value(&mut files, "manifest.json", image.clone());
    cases.push(tar(&files));
    image[0]["Layers"].as_array_mut().unwrap().reverse();
    replace_value(&mut files, "manifest.json", image);
    cases.push(tar(&files));
    oracle(&cases);
}

#[test]
fn an_in_place_change_between_tar_passes_cannot_reach_the_allocating_reader() {
    use crate::files::CheckedStream;
    use std::io::{self, SeekFrom};
    struct Mutate<'a> {
        stream: CheckedStream<'a>,
        path: &'a Path,
        changed: Vec<u8>,
        returned: usize,
    }
    impl Read for Mutate<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let count = self.stream.read(bytes)?;
            if self.changed.is_empty() {
                self.returned += count;
            }
            Ok(count)
        }
    }
    impl Seek for Mutate<'_> {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            let result = self.stream.seek(position)?;
            if position == SeekFrom::Start(0) && !self.changed.is_empty() {
                fs::write(self.path, &self.changed)?;
                self.changed.clear();
            }
            Ok(result)
        }
    }
    let root = Fixture::new();
    let path = root.0.join("image.tar");
    let bytes = tar(&docker(config()));
    fs::write(&path, &bytes).unwrap();
    let mut snapshot = Snapshot::open(&path, bytes.len() as u64).unwrap();
    let length = snapshot.length();
    let mut changed = bytes;
    let header = tar::Header::from_byte_slice(&changed[..512]).clone();
    let mut header = header;
    header.set_entry_type(tar::EntryType::GNULongName);
    header.set_size(MAX_ARCHIVE_BYTES);
    header.set_cksum();
    changed[..512].copy_from_slice(header.as_bytes());
    let mut reader = Mutate {
        stream: snapshot.checked_stream(),
        path: &path,
        changed,
        returned: 0,
    };
    assert!(matches!(
        index::Index::read(&mut reader, length, Limits::default()),
        Err(ArchiveError::Read)
    ));
    assert!(
        reader.changed.is_empty(),
        "mutation must happen after the raw preflight"
    );
    assert_eq!(
        reader.returned, 0,
        "no changed header may reach normal tar iteration"
    );
    assert_eq!(snapshot.verify(), Err(crate::Error::Changed));
}

#[test]
fn legacy_directory_aliases_and_non_utf8_effective_names_are_explicit_refusals() {
    let mut files = docker(config());
    let mut directory = entry("legacy/", b"");
    directory.kind = 0;
    files.push(directory);
    assert_eq!(verify(&files).unwrap_err(), ArchiveError::UnsupportedTar);
    let mut files = docker(config());
    files.insert(
        1,
        Entry {
            name: "long".into(),
            kind: b'L',
            bytes: vec![0xff, 0],
        },
    );
    assert_eq!(verify(&files).unwrap_err(), ArchiveError::UnsupportedTar);
}
