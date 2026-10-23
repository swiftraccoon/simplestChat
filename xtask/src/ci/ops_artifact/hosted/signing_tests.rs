//! Full unchanged assembly/ELF admission with hosted evidence and exact copying.

use super::super::namespace::Budget;
use super::{
    catalog::Catalog,
    context::{ArtifactKind, Context},
    fixture, provenance, signing,
};
use serde_json::{Value, json};
use simplestchat_ops::distribution::manifest::{
    ArtifactManifest,
    assembly::{EvidenceSources, prepare_artifact},
};
use std::{collections::BTreeMap, fs};

fn context(root: &std::path::Path) -> Context {
    Context {
        repository: "swiftraccoon/simplestChat",
        revision: "a".repeat(40),
        package_version: env!("CARGO_PKG_VERSION"),
        protocol_version: 1,
        target: "x86_64-unknown-linux-musl",
        build_run_id: 123,
        run_attempt: 2,
        workspace: root.into(),
    }
}
fn documents(context: &Context) -> BTreeMap<String, Vec<u8>> {
    let catalog = Catalog::load().unwrap();
    [
        (
            "hosted-context.json",
            serde_json::to_value(context).unwrap(),
        ),
        (
            "hosted-source.json",
            json!({"schemaVersion":1,"selection":context,"commit":context.revision,
            "source":{"profile":"ops-producer-tree-v1","sha256":"1".repeat(64)}}),
        ),
        (
            "hosted-registry.json",
            json!({"schemaVersion":1,"kind":"ops-offline-registry-observation",
            "registry":{"sha256":"2".repeat(64)},"lockSha256":"3".repeat(64)}),
        ),
        (
            "hosted-kit.json",
            json!({"schemaVersion":1,"kind":"ops-hosted-kit-observation",
            "catalogSha256":catalog.identity.sha256,"catalogBytes":catalog.identity.bytes,
            "archives":catalog.identity.archives,"archiveBytes":catalog.identity.archive_bytes,
            "registrySha256":"2".repeat(64),"lockSha256":"3".repeat(64),
            "kit":{"sha256":"4".repeat(64)},"resolverProbes":super::kit::arguments().len(),
            "compilerRuntime":"hostedOsAndDynamicLoaderTrusted"}),
        ),
        (
            "producer-materials.json",
            json!({"schemaVersion":1,"source":{"sha256":"1".repeat(64)},
            "builderKit":{"sha256":"4".repeat(64)}}),
        ),
        (
            "producer-outcome.json",
            json!({"schemaVersion":1,"kind":"ops-cargo-observation","commandStatus":0,
            "cargoAndProbeCliGroupsSettled":true,"sourceConfigAndToolOwnersClosed":true}),
        ),
        ("producer-recipe.json", json!({"schemaVersion":1})),
        ("producer-resolution.json", json!({"schemaVersion":1})),
        ("producer-toolchain.json", json!({"schemaVersion":1})),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), provenance::encoded(&value).unwrap()))
    .collect()
}
fn prepare(fixture: &fixture::Fixture, changed: Option<(&str, &str, Value)>) -> Context {
    let context = context(&fixture.root);
    let mut documents = documents(&context);
    if let Some((name, pointer, value)) = changed {
        let mut data: Value = serde_json::from_slice(&documents[name]).unwrap();
        *data.pointer_mut(pointer).unwrap() = value;
        documents.insert(name.into(), provenance::encoded(&data).unwrap());
    }
    assembled(fixture, context, documents)
}
fn assembled(
    fixture: &fixture::Fixture,
    context: Context,
    documents: BTreeMap<String, Vec<u8>>,
) -> Context {
    let mut paths = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for (name, bytes) in &documents {
        let path = fixture.root.join(name);
        fs::write(&path, bytes).unwrap();
        paths.insert(name.clone(), path);
        hashes.insert(name.clone(), super::super::sha(bytes));
    }
    let bytes = super::super::fixture::binary();
    let binary = fixture.root.join("inert-elf-never-executed");
    fs::write(&binary, &bytes).unwrap();
    let mut manifest = serde_json::to_value(&context).unwrap();
    let object = manifest.as_object_mut().unwrap();
    object.insert("schemaVersion".into(), 1.into());
    object.insert("artifactType".into(), "simplestchat-ops".into());
    object.insert(
        "binary".into(),
        json!({"name":"simplestchat-ops","sha256":super::super::sha(&bytes),"bytes":bytes.len()}),
    );
    object.insert(
        "evidenceDigests".into(),
        serde_json::to_value(hashes).unwrap(),
    );
    object.insert(
        "inputDigests".into(),
        json!({"source-tree/frame-v1":"1".repeat(64),
        "builder-kit/frame-v1":"4".repeat(64),
        "hosted/catalog-v1":Catalog::load().unwrap().identity.sha256,
        "source-lock/original-sha256":"3".repeat(64)}),
    );
    let manifest = ArtifactManifest::decode(&provenance::encoded(&manifest).unwrap()).unwrap();
    prepare_artifact(
        &manifest,
        &binary,
        &context.selection().unwrap(),
        &EvidenceSources::new(paths).unwrap(),
        &fixture.root.join("unsigned"),
    )
    .unwrap();
    context
}

#[test]
fn signer_reassembles_original_fourteen_files_and_exact_three_subject_bytes() {
    let fixture = fixture::Fixture::new();
    let context = prepare(&fixture, None);
    let budget = Budget::default();
    let result = signing::prepare(
        &context,
        &fixture.root.join("unsigned"),
        &fixture.root.join("signed"),
        &budget,
        &mut || Ok(()),
    )
    .unwrap();
    let result: Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(result["subjectCount"], 3);
    assert_eq!(
        result["artifactName"],
        context.artifact_name(ArtifactKind::Signed)
    );
    let source = fs::read_dir(fixture.root.join("unsigned"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let copied = fs::read_dir(fixture.root.join("signed"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(source.len(), 14);
    assert_eq!(source, copied);
    for name in source {
        assert_eq!(
            fs::read(fixture.root.join("unsigned").join(&name)).unwrap(),
            fs::read(fixture.root.join("signed").join(name)).unwrap()
        );
    }
    assert_eq!(budget.count(), 0);
}

#[test]
fn valid_generic_assembly_cannot_override_hosted_stage_selection_or_content_pins() {
    for (name, pointer, value) in [
        ("hosted-context.json", "/revision", json!("b".repeat(40))),
        ("hosted-source.json", "/commit", json!("b".repeat(40))),
        ("hosted-kit.json", "/catalogSha256", json!("b".repeat(64))),
        ("hosted-kit.json", "/resolverProbes", json!(13)),
        (
            "hosted-registry.json",
            "/registry/sha256",
            json!("b".repeat(64)),
        ),
        ("producer-outcome.json", "/commandStatus", json!(42)),
        (
            "producer-outcome.json",
            "/sourceConfigAndToolOwnersClosed",
            json!(false),
        ),
    ] {
        let fixture = fixture::Fixture::new();
        let context = prepare(&fixture, Some((name, pointer, value)));
        let budget = Budget::default();
        fixture::expect_error(
            signing::prepare(
                &context,
                &fixture.root.join("unsigned"),
                &fixture.root.join("signed"),
                &budget,
                &mut || Ok(()),
            ),
            1,
        );
        assert!(!fixture.root.join("signed").exists());
        assert_eq!(budget.count(), 0);
    }
}

#[test]
fn signer_refuses_extra_member_corruption_foreign_mode_and_pending_checkpoint() {
    for case in ["extra", "bytes", "mode", "pending"] {
        let fixture = fixture::Fixture::new();
        let context = prepare(&fixture, None);
        let budget = Budget::default();
        let source = fixture.root.join("unsigned");
        match case {
            "extra" => fs::write(source.join("PRIVATE-extra"), b"never sign").unwrap(),
            "bytes" => fs::write(source.join("ops-evidence-00.bin"), b"different").unwrap(),
            "mode" => {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    source.join("simplestchat-ops"),
                    fs::Permissions::from_mode(0o666),
                )
                .unwrap();
            }
            _ => {}
        }
        fixture::expect_error(
            signing::prepare(
                &context,
                &source,
                &fixture.root.join("signed"),
                &budget,
                &mut || {
                    if case == "pending" {
                        Err(super::failure(143))
                    } else {
                        Ok(())
                    }
                },
            ),
            if case == "pending" { 143 } else { 1 },
        );
        assert!(!fixture.root.join("signed").exists());
        assert_eq!(budget.count(), 0);
    }
}

#[test]
fn paired_absent_or_invalid_registry_digests_never_match_as_untyped_nulls() {
    for absent in [true, false] {
        let fixture = fixture::Fixture::new();
        let context = context(&fixture.root);
        let mut docs = documents(&context);
        for (name, parent, key) in [
            ("hosted-registry.json", "/registry", "sha256"),
            ("hosted-kit.json", "", "registrySha256"),
        ] {
            let mut data: Value = serde_json::from_slice(&docs[name]).unwrap();
            let object = data.pointer_mut(parent).unwrap().as_object_mut().unwrap();
            if absent {
                object.remove(key);
            } else {
                object.insert(key.into(), json!("PRIVATE-bad"));
            }
            docs.insert(name.into(), provenance::encoded(&data).unwrap());
        }
        let context = assembled(&fixture, context, docs);
        let budget = Budget::default();
        fixture::expect_error(
            signing::prepare(
                &context,
                &fixture.root.join("unsigned"),
                &fixture.root.join("signed"),
                &budget,
                &mut || Ok(()),
            ),
            1,
        );
        assert!(!fixture.root.join("signed").exists());
        assert_eq!(budget.count(), 0);
    }
}
