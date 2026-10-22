//! Synthetic image metadata and inert layer bytes only; no engine, compression,
//! build, candidate, image load or registry operation is used by these fixtures.

use super::*;
use crate::test_support::Fixture;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Cursor, Read, Seek, SeekFrom},
    os::unix::fs::symlink,
};

const REV: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const OCI_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const OCI_RAW: &str = "application/vnd.oci.image.layer.v1.tar";
const OCI_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
#[derive(Clone)]
struct Entry {
    name: String,
    bytes: Vec<u8>,
    kind: u8,
}
fn entry(name: &str, bytes: impl AsRef<[u8]>) -> Entry {
    Entry {
        name: name.into(),
        bytes: bytes.as_ref().to_vec(),
        kind: b'0',
    }
}
fn tar(entries: &[Entry]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for value in entries {
        let mut header = tar::Header::new_gnu();
        header.set_path(&value.name).unwrap();
        header.set_size(value.bytes.len() as u64);
        header.set_mode(0o600);
        header.set_entry_type(tar::EntryType::new(value.kind));
        header.set_cksum();
        builder.append(&header, Cursor::new(&value.bytes)).unwrap();
    }
    builder.into_inner().unwrap()
}
fn config(diff_ids: &[String]) -> Value {
    json!({"os":"linux","architecture":"amd64","rootfs":{"type":"layers","diff_ids":diff_ids},
        "config":{"User":"10001:10001","WorkingDir":"/results","Entrypoint":["/app/load_test"],
        "Cmd":["--help"],"Env":[format!("SOURCE_REVISION={REV}"),"PRIVATE_SECRET=never_print"],
        "Labels":{"org.opencontainers.image.revision":REV,"PRIVATE_KEY":"PRIVATE_VALUE"}}})
}
fn raw_layers() -> Vec<Vec<u8>> {
    vec![
        b"inert raw layer one".to_vec(),
        b"inert raw layer two".to_vec(),
    ]
}
fn config_for_raw() -> Value {
    config(
        &raw_layers()
            .iter()
            .map(|bytes| format!("sha256:{}", sha256(bytes)))
            .collect::<Vec<_>>(),
    )
}
fn classic(cfg: Value) -> (Vec<Entry>, String) {
    let bytes = cfg.to_string().into_bytes();
    let id = format!("sha256:{}", sha256(&bytes));
    let mut entries = vec![
        entry(
            "manifest.json",
            json!([{"Config":"config.json",
        "RepoTags":[TAG],"Layers":["layer-0.tar","layer-1.tar"]}])
            .to_string(),
        ),
        entry("config.json", bytes),
    ];
    for (index, bytes) in raw_layers().iter().enumerate() {
        entries.push(entry(&format!("layer-{index}.tar"), bytes));
    }
    (entries, id)
}
fn blob(entries: &mut Vec<Entry>, bytes: Vec<u8>, media: &str) -> Value {
    let hash = sha256(&bytes);
    let size = bytes.len();
    entries.push(entry(&format!("blobs/sha256/{hash}"), bytes));
    json!({"digest":format!("sha256:{hash}"),"size":size,"mediaType":media})
}
fn path(desc: &Value) -> String {
    format!(
        "blobs/sha256/{}",
        desc["digest"]
            .as_str()
            .unwrap()
            .strip_prefix("sha256:")
            .unwrap()
    )
}
fn graph(depth: usize, compressed: bool, mutate: impl FnOnce(&mut Value)) -> (Vec<Entry>, String) {
    let mut entries = Vec::new();
    let cfg = blob(
        &mut entries,
        config_for_raw().to_string().into_bytes(),
        OCI_CONFIG,
    );
    let mut layers = Vec::new();
    for (index, raw) in raw_layers().into_iter().enumerate() {
        let bytes = if compressed {
            format!("inert stored compressed blob {index}").into_bytes()
        } else {
            raw
        };
        layers.push(blob(
            &mut entries,
            bytes,
            if compressed { OCI_GZIP } else { OCI_RAW },
        ));
    }
    let mut image = json!({"schemaVersion":2,"mediaType":OCI_MANIFEST,
        "config":cfg,"layers":layers});
    mutate(&mut image);
    let mut current = blob(&mut entries, image.to_string().into_bytes(), OCI_MANIFEST);
    for _ in 0..depth {
        current = blob(
            &mut entries,
            json!({"schemaVersion":2,"mediaType":OCI_INDEX,
            "manifests":[current]})
            .to_string()
            .into_bytes(),
            OCI_INDEX,
        );
    }
    let selected = current["digest"].as_str().unwrap().to_owned();
    current["annotations"] = json!({"io.containerd.image.name":CANONICAL,
        "org.opencontainers.image.ref.name":"ci"});
    entries.push(entry(
        "index.json",
        json!({"schemaVersion":2,"manifests":[current]}).to_string(),
    ));
    entries.push(entry("oci-layout", b"{\"imageLayoutVersion\":\"1.0.0\"}"));
    entries.push(entry(
        "manifest.json",
        json!([{"Config":path(&cfg),"RepoTags":[TAG],
        "Layers":layers.iter().map(path).collect::<Vec<_>>()}])
        .to_string(),
    ));
    (entries, selected)
}
fn value(entries: &[Entry], name: &str) -> Value {
    serde_json::from_slice(&entries.iter().find(|v| v.name == name).unwrap().bytes).unwrap()
}
fn replace(entries: &mut [Entry], name: &str, value: Value) {
    entries.iter_mut().find(|v| v.name == name).unwrap().bytes = value.to_string().into_bytes();
}
fn verify(entries: &[Entry], id: &str) -> Result<GeneratorIdentity> {
    let root = Fixture::new();
    let path = root.0.join("generator.tar");
    fs::write(&path, tar(entries)).unwrap();
    verify_generator_archive(&path, &GeneratorSelection::new(id, REV).unwrap())
}

#[test]
fn classic_binds_actual_config_and_every_ordered_raw_layer_without_printing_private_values() {
    let (entries, id) = classic(config_for_raw());
    let bytes = tar(&entries);
    let identity = verify(&entries, &id).unwrap();
    let value = serde_json::to_value(&identity).unwrap();
    assert_eq!(identity.archive_sha256(), sha256(&bytes));
    assert_eq!(identity.archive_bytes(), bytes.len() as u64);
    assert_eq!(value["format"], "classic-docker");
    assert_eq!(value["selectedRootDigest"], Value::Null);
    assert_eq!(value["configSha256"], id.strip_prefix("sha256:").unwrap());
    assert_eq!(value["layers"], 2);
    assert_eq!(value["imageTag"], TAG);
    assert!(!value.to_string().contains("PRIVATE"));
    assert!(!format!("{identity:?}").contains("PRIVATE"));
    let mut changed = entries.clone();
    changed[2].bytes[0] ^= 1;
    assert!(matches!(verify(&changed, &id), Err(E::LayersMismatch)));
    let mut changed = entries.clone();
    let mut manifest = value_for_manifest(&changed);
    manifest[0]["Layers"] = json!(["layer-1.tar", "layer-0.tar"]);
    replace(&mut changed, "manifest.json", manifest);
    assert!(matches!(verify(&changed, &id), Err(E::LayersMismatch)));
    assert!(verify(&entries, &format!("sha256:{}", "b".repeat(64))).is_err());
}
fn value_for_manifest(entries: &[Entry]) -> Value {
    value(entries, "manifest.json")
}

#[test]
fn containerd_parent_or_manifest_id_reaches_exact_config_and_all_stored_blobs() {
    for depth in [0, 1, 7] {
        for compressed in [false, true] {
            let (entries, id) = graph(depth, compressed, |_| {});
            let identity = verify(&entries, &id).unwrap();
            let value = serde_json::to_value(identity).unwrap();
            assert_eq!(value["format"], "containerd-oci");
            assert_eq!(value["selectedRootDigest"], id);
            assert_ne!(value["configSha256"], id.strip_prefix("sha256:").unwrap());
            assert_eq!(value["layers"], 2);
            assert!(!value.to_string().contains("PRIVATE"));
        }
    }
    let (entries, id) = graph(8, false, |_| {});
    assert!(matches!(verify(&entries, &id), Err(E::IndexDepth)));
}

#[test]
fn each_referenced_blob_is_size_and_hash_verified_and_missing_payloads_never_pass() {
    let (entries, id) = graph(2, true, |_| {});
    for index in 0..entries.len() {
        if !entries[index].name.starts_with("blobs/") {
            continue;
        }
        let mut changed = entries.clone();
        changed[index].bytes[0] ^= 1;
        assert!(verify(&changed, &id).is_err(), "changed blob {index}");
        let mut changed = entries.clone();
        changed.remove(index);
        assert!(verify(&changed, &id).is_err(), "missing blob {index}");
    }
    for mutation in [
        "size",
        "float",
        "urls",
        "data",
        "media",
        "platform",
        "declared-media",
    ] {
        let (entries, id) = graph(0, true, |image| match mutation {
            "size" => image["layers"][0]["size"] = json!(0),
            "float" => image["layers"][0]["size"] = json!(30.0),
            "urls" => image["layers"][0]["urls"] = json!(["https://PRIVATE.invalid"]),
            "data" => image["layers"][0]["data"] = json!("PRIVATE"),
            "media" => image["layers"][0]["mediaType"] = json!("application/unsupported"),
            "declared-media" => image["mediaType"] = json!(OCI_INDEX),
            _ => image["layers"][0]["platform"] = json!({"os":"linux","architecture":"arm64"}),
        });
        assert!(verify(&entries, &id).is_err(), "{mutation}");
    }
}

#[test]
fn incomplete_multiarch_remote_cycles_and_ambiguous_tag_graphs_are_refused() {
    for mutation in [
        "multi",
        "missing-name",
        "missing-ref",
        "wrong-name",
        "wrong-ref",
        "root-id",
    ] {
        let (mut entries, mut id) = graph(1, false, |_| {});
        let mut index = value(&entries, "index.json");
        match mutation {
            "multi" => {
                let extra = index["manifests"][0].clone();
                index["manifests"].as_array_mut().unwrap().push(extra);
            }
            "missing-name" => {
                index["manifests"][0]["annotations"]
                    .as_object_mut()
                    .unwrap()
                    .remove("io.containerd.image.name");
            }
            "missing-ref" => {
                index["manifests"][0]["annotations"]
                    .as_object_mut()
                    .unwrap()
                    .remove("org.opencontainers.image.ref.name");
            }
            "wrong-name" => {
                index["manifests"][0]["annotations"]["io.containerd.image.name"] = json!(TAG)
            }
            "wrong-ref" => {
                index["manifests"][0]["annotations"]["org.opencontainers.image.ref.name"] =
                    json!(CANONICAL)
            }
            _ => id = format!("sha256:{}", "b".repeat(64)),
        }
        replace(&mut entries, "index.json", index);
        assert!(verify(&entries, &id).is_err(), "{mutation}");
    }
    let (mut entries, _) = graph(0, false, |_| {});
    let mut index = value(&entries, "index.json");
    let child = index["manifests"][0].clone();
    let mut parent = blob(
        &mut entries,
        json!({"schemaVersion":2,"manifests":[child.clone(),child]})
            .to_string()
            .into_bytes(),
        OCI_INDEX,
    );
    let id = parent["digest"].as_str().unwrap().to_owned();
    parent["annotations"] = index["manifests"][0]["annotations"].clone();
    index["manifests"] = json!([parent]);
    replace(&mut entries, "index.json", index);
    assert!(matches!(verify(&entries, &id), Err(E::IndexCount)));
    let (entries, id) = graph(0, false, |image| {
        image["config"]["digest"] = json!(format!("sha256:{}", "f".repeat(64)))
    });
    assert!(verify(&entries, &id).is_err());
}

#[test]
fn classic_optional_oci_must_self_validate_even_for_compressed_distribution_hints() {
    let (mut entries, _) = graph(0, false, |_| {});
    let manifest = value(&entries, "manifest.json");
    let config_name = manifest[0]["Config"].as_str().unwrap();
    let id = format!(
        "sha256:{}",
        sha256(
            &entries
                .iter()
                .find(|v| v.name == config_name)
                .unwrap()
                .bytes
        )
    );
    assert_eq!(
        serde_json::to_value(verify(&entries, &id).unwrap()).unwrap()["format"],
        "classic-docker"
    );
    // A distribution descriptor can truthfully identify compressed bytes while
    // Docker's classic saved Layers contain raw DiffID bytes. That optional OCI
    // graph is not self-contained, and this profile refuses instead of treating
    // compressed descriptor hashes as uncompressed DiffIDs.
    let mut index = value(&entries, "index.json");
    let old = path(&index["manifests"][0]);
    let mut image = value(&entries, &old);
    image["layers"][0]["digest"] = json!(format!("sha256:{}", "c".repeat(64)));
    image["layers"][0]["mediaType"] = json!(OCI_GZIP);
    let mut replacement = blob(&mut entries, image.to_string().into_bytes(), OCI_MANIFEST);
    replacement["annotations"] = index["manifests"][0]["annotations"].clone();
    index["manifests"] = json!([replacement]);
    replace(&mut entries, "index.json", index);
    assert!(verify(&entries, &id).is_err());
    entries.retain(|value| value.name != "oci-layout");
    assert!(verify(&entries, &id).is_err());
}

#[test]
fn unused_distribution_registration_hints_cannot_supply_config_or_layer_proof() {
    let (mut entries, id) = classic(config_for_raw());
    let mut manifest = value(&entries, "manifest.json");
    manifest[0]["LayerSources"] = json!({
        "sha256:unverified-hint": {"digest":format!("sha256:{}", "b".repeat(64)),
            "size":999,"urls":["https://PRIVATE.invalid/not-fetched"]},
    });
    replace(&mut entries, "manifest.json", manifest);
    let identity = verify(&entries, &id).unwrap();
    assert!(
        !serde_json::to_string(&identity)
            .unwrap()
            .contains("PRIVATE")
    );
    assert!(verify(&entries, &format!("sha256:{}", "b".repeat(64))).is_err());
    entries
        .iter_mut()
        .find(|value| value.name == "layer-0.tar")
        .unwrap()
        .bytes[0] ^= 1;
    assert!(matches!(verify(&entries, &id), Err(E::LayersMismatch)));
}

#[test]
fn runtime_schema_revision_source_env_and_fixed_tag_cannot_be_substituted() {
    for mutation in [
        "os",
        "arch",
        "user",
        "revision",
        "source",
        "duplicate-source",
        "entry",
        "cmd",
        "cwd",
        "diffid",
    ] {
        let mut cfg = config_for_raw();
        match mutation {
            "os" => cfg["os"] = json!("windows"),
            "arch" => cfg["architecture"] = json!("arm64"),
            "user" => cfg["config"]["User"] = json!("root"),
            "revision" => {
                cfg["config"]["Labels"]["org.opencontainers.image.revision"] = json!("b".repeat(40))
            }
            "source" => cfg["config"]["Env"] = json!(["SOURCE_REVISION=PRIVATE"]),
            "duplicate-source" => {
                cfg["config"]["Env"] = json!([
                    format!("SOURCE_REVISION={REV}"),
                    format!("SOURCE_REVISION={REV}")
                ])
            }
            "entry" => cfg["config"]["Entrypoint"] = json!(["/app/simplestChat"]),
            "cmd" => cfg["config"]["Cmd"] = json!(["run"]),
            "cwd" => cfg["config"]["WorkingDir"] = json!("/app"),
            _ => cfg["rootfs"]["diff_ids"][0] = json!("not-a-digest"),
        }
        let (entries, id) = classic(cfg);
        assert!(verify(&entries, &id).is_err(), "{mutation}");
    }
    for bad in [
        "sha256:short",
        &"a".repeat(64),
        &format!("sha256:{}", "A".repeat(64)),
    ] {
        assert!(GeneratorSelection::new(bad, REV).is_err());
    }
    assert!(
        GeneratorSelection::new(&format!("sha256:{}", "a".repeat(64)), &"A".repeat(40)).is_err()
    );
    let (mut entries, id) = classic(config_for_raw());
    let mut manifest = value(&entries, "manifest.json");
    manifest[0]["RepoTags"] = json!(["PRIVATE:foreign"]);
    replace(&mut entries, "manifest.json", manifest);
    assert!(matches!(verify(&entries, &id), Err(E::Tag)));
}

#[test]
fn callbacks_fence_every_phase_and_mutable_snapshot_changes_never_publish_identity() {
    let (entries, id) = classic(config_for_raw());
    let bytes = tar(&entries);
    let root = Fixture::new();
    let path = root.0.join("generator.tar");
    fs::write(&path, &bytes).unwrap();
    let selected = GeneratorSelection::new(&id, REV).unwrap();
    let mut count = 0;
    verify_generator_archive_with_checkpoint(&path, &selected, || {
        count += 1;
        true
    })
    .unwrap();
    assert!(count > 10);
    for stop in 1..=count {
        let mut current = 0;
        assert!(
            matches!(
                verify_generator_archive_with_checkpoint(&path, &selected, || {
                    current += 1;
                    current < stop
                }),
                Err(E::Budget)
            ),
            "checkpoint {stop}"
        );
    }
    for mutation in ["write", "replace", "ancestor"] {
        let outer = root.0.join(mutation);
        fs::create_dir(&outer).unwrap();
        let path = outer.join("generator.tar");
        fs::write(&path, &bytes).unwrap();
        let mut seen = 0;
        let result = verify_generator_archive_with_checkpoint(&path, &selected, || {
            seen += 1;
            if seen == 4 {
                match mutation {
                    "write" => {
                        let mut next = bytes.clone();
                        next[0] ^= 1;
                        fs::write(&path, next).unwrap();
                    }
                    "replace" => {
                        fs::rename(&path, outer.join("retained")).unwrap();
                        fs::write(&path, &bytes).unwrap();
                    }
                    _ => {
                        fs::rename(&outer, root.0.join("retired")).unwrap();
                        fs::create_dir(&outer).unwrap();
                        fs::write(&path, &bytes).unwrap();
                    }
                }
            }
            true
        });
        assert!(result.is_err(), "{mutation}");
    }
}

#[test]
fn generator_tar_tail_aliases_conflicts_and_input_limits_are_closed_independently() {
    let (entries, id) = classic(config_for_raw());
    let root = Fixture::new();
    let path = root.0.join("generator.tar");
    let selected = GeneratorSelection::new(&id, REV).unwrap();
    let bytes = tar(&entries);
    let mut trailing = bytes.clone();
    trailing.extend_from_slice(&[b'X'; 512]);
    fs::write(&path, &trailing).unwrap();
    assert!(verify_generator_archive(&path, &selected).is_err());
    let mut alias = entries.clone();
    alias[1].name = "config.json/".into();
    assert!(verify(&alias, &id).is_err());
    let mut conflict = entries.clone();
    conflict.push(entry("layer-0.tar/child", b"inert"));
    assert!(verify(&conflict, &id).is_err());
    let mut metadata = entries.clone();
    metadata[1].bytes = vec![b' '; 1024 * 1024 + 1];
    assert!(matches!(verify(&metadata, &id), Err(E::Metadata)));
    fs::write(&path, &bytes).unwrap();
    let link = root.0.join("link");
    symlink(&path, &link).unwrap();
    assert!(matches!(
        verify_generator_archive(&link, &selected),
        Err(E::Input)
    ));
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(super::super::MAX_ARCHIVE_BYTES + 1).unwrap();
    drop(file);
    assert!(matches!(
        verify_generator_archive(&path, &selected),
        Err(E::Input)
    ));
}

struct Traced {
    inner: Cursor<Vec<u8>>,
    largest: usize,
}
impl Read for Traced {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.largest = self.largest.max(bytes.len());
        self.inner.read(bytes)
    }
}
impl Seek for Traced {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}
#[test]
fn multi_megabyte_member_hashes_use_fixed_chunks_instead_of_metadata_allocation() {
    let payload = vec![b'Z'; 3 * 1024 * 1024 + 17];
    let bytes = tar(&[entry("large-layer", &payload)]);
    let mut stream = Traced {
        inner: Cursor::new(bytes.clone()),
        largest: 0,
    };
    let index = Index::read(&mut stream, bytes.len() as u64, Limits::default()).unwrap();
    stream.largest = 0;
    let mut calls = 0;
    let (actual, size) = index
        .hash_member(&mut stream, "large-layer", &mut || {
            calls += 1;
            true
        })
        .unwrap();
    assert_eq!(actual, sha256(&payload));
    assert_eq!(size, payload.len() as u64);
    assert!(stream.largest <= 65536);
    assert!(calls > 90);
    assert!(matches!(
        index.read_bytes(&mut stream, "large-layer"),
        Err(E::Metadata)
    ));
}

#[test]
fn exact_generator_layer_limit_is_admitted_and_the_next_layer_is_refused() {
    for count in [MAX_LAYERS, MAX_LAYERS + 1] {
        let raw = raw_layers();
        let diff_id = format!("sha256:{}", sha256(&raw[0]));
        let (mut entries, id) = classic(config(&vec![diff_id; count]));
        let mut image = value(&entries, "manifest.json");
        image[0]["Layers"] = json!(vec!["layer-0.tar"; count]);
        replace(&mut entries, "manifest.json", image);
        if count == MAX_LAYERS {
            assert_eq!(verify(&entries, &id).unwrap().layers, MAX_LAYERS);
        } else {
            assert!(matches!(verify(&entries, &id), Err(E::Layers)));
        }
    }
}
