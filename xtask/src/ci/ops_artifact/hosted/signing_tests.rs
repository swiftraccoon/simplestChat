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

fn native_observation(catalog: &Catalog) -> Value {
    let mut libraries = serde_json::Map::new();
    for name in [
        "crt1.o",
        "Scrt1.o",
        "rcrt1.o",
        "crti.o",
        "crtn.o",
        "libc.a",
        "libm.a",
        "librt.a",
        "libpthread.a",
        "libcrypt.a",
        "libutil.a",
        "libxnet.a",
        "libresolv.a",
        "libdl.a",
        "libssl.a",
        "libcrypto.a",
    ] {
        let prefix = if ["libssl.a", "libcrypto.a"].contains(&name) {
            "openssl"
        } else {
            "musl"
        };
        let object = name.ends_with(".o");
        let empty = !object && !["libc.a", "libssl.a", "libcrypto.a"].contains(&name);
        libraries.insert(
            format!("native/{prefix}/lib/{name}"),
            json!({
                "sha256":"a".repeat(64),"bytes":if empty{8}else{64},
                "objects":if empty{0}else{1},
                "format":if object{"ELF64LE-ET_REL-EM_X86_64"}else{"ar-ELF64LE-ET_REL-EM_X86_64"}
            }),
        );
    }
    let pins = catalog
        .artifacts
        .iter()
        .filter(|a| a.format == super::catalog::Format::NativeGzip)
        .map(|a| {
            (
                a.name.clone(),
                json!({"sha256":a.sha256,"bytes":a.bytes,"version":a.version}),
            )
        })
        .collect::<BTreeMap<_, _>>();
    json!({
        "schemaVersion":1,"scope":"OpsMuslStaticNativeBuild","target":super::context::TARGET,
        "muslSourceCommit":super::catalog::MUSL_COMMIT,"opensslVersion":"4.1.0",
        "opensslBuildRecipe":{"profile":"ops-openssl-build-v1","sourceDateEpoch":0},
        "sourceArchivePins":pins,"configdataSha256":"b".repeat(64),
        "stockMuslSpecsSha256":"c".repeat(64),
        "disabled":{"shared":true,"dso":true,"module":true,"engine":true},
        "outputInventory":{"algorithm":"sha256-length-framed-v1","sha256":"d".repeat(64),
            "files":20,"bytes":1024,"mode":"0600"},
        "libraries":libraries,"licenseNotices":{
            "musl":{"sha256":super::super::sha(b"Inert musl notice\n"),
                "bytes":b"Inert musl notice\n".len()},
            "openssl":{"sha256":super::super::sha(b"Inert OpenSSL notice\n"),
                "bytes":b"Inert OpenSSL notice\n".len()}
        }
    })
}

pub(super) fn recipe_observation() -> Value {
    let environment = [
        "CARGO_HOME",
        "HOME",
        "TMPDIR",
        "PATH",
        "RUSTC",
        "CC",
        "CC_x86_64_unknown_linux_musl",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER",
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
        "OPS_MUSL_ROOT",
        "X86_64_UNKNOWN_LINUX_MUSL_OPENSSL_DIR",
        "X86_64_UNKNOWN_LINUX_MUSL_OPENSSL_STATIC",
        "X86_64_UNKNOWN_LINUX_MUSL_OPENSSL_NO_VENDOR",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_INCREMENTAL",
        "CARGO_TERM_COLOR",
        "LC_ALL",
        "TZ",
        "GCC_EXEC_PREFIX",
        "COMPILER_PATH",
        "LIBRARY_PATH",
    ];
    json!({
        "schemaVersion":1,"profile":"ops-producer-recipe-v2","workingDirectory":"freshRunRoot",
        "manifest":"stagedSource/Cargo.toml","configuration":"generatedDirectorySourceOnly",
        "environmentPolicy":"clearThenSet","environmentNames":environment,
        "rustflags":simplestchat_ops::distribution::build_plan::MUSL_RUSTFLAGS,
        "target":super::context::TARGET,"host":super::context::HOST,
        "toolchainVersion":super::context::VERSION,"defaultFeatures":false,
        "features":["maintain-guest"],"staticPieExpectation":"authenticatedMuslStaticPieCrt",
        "candidateExecuted":false
    })
}

pub(super) fn toolchain_observation() -> Value {
    let ordinary = json!({"sha256":"a".repeat(64),"bytes":100});
    let probe = json!({"version":super::context::VERSION,"host":super::context::HOST,
        "sha256":"b".repeat(64),"bytes":100});
    json!({
        "schemaVersion":1,"kind":"ops-producer-tool-observations",
        "selectedTools":{
            "cargo":ordinary,"rustc":ordinary,"rustLld":ordinary,
            "hostCc":{"sha256":super::super::sha(super::shim::CC),"bytes":super::shim::CC.len()},
            "muslCc":{"sha256":super::super::sha(super::shim::MUSL_CC),
                "bytes":super::shim::MUSL_CC.len()},
            "nativeLicenseNotices":[
                {"component":"musl","sha256":super::super::sha(b"Inert musl notice\n"),
                    "text":"Inert musl notice\n"},
                {"component":"openssl","sha256":super::super::sha(b"Inert OpenSSL notice\n"),
                    "text":"Inert OpenSSL notice\n"}
            ]
        },
        "versionProbes":[probe,probe],
        "sysroot":{"sha256":"c".repeat(64),"bytes":100,"selectedStagedSysroot":true},
        "authentication":"callerPrerequisite","runtimeClosure":"unproven"
    })
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
            "compilerRuntime":"hostedOsAndDynamicLoaderTrusted",
            "native":native_observation(&catalog)}),
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
        ("producer-recipe.json", recipe_observation()),
        (
            "producer-resolution.json",
            json!({
                "schemaVersion":1,"profile":"ops-producer-resolution-v2",
                "originalSha256":"a".repeat(64),"originalBytes":100,"packages":100,
                "workspaceMembers":4,"scope":"observedOpsFeaturesAndManifestLocations",
                "opsFeatures":["maintain-guest"]
            }),
        ),
        ("producer-toolchain.json", toolchain_observation()),
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
fn signer_rejects_wrong_native_catalog_build_or_output_facts_after_valid_assembly() {
    for (pointer, value) in [
        ("/native/scope", json!("other")),
        ("/native/schemaVersion", json!(true)),
        ("/native/target", json!("x86_64-unknown-linux-gnu")),
        ("/native/muslSourceCommit", json!("b".repeat(40))),
        ("/native/opensslVersion", json!("4.0.0")),
        ("/native/opensslBuildRecipe/profile", json!("wall-clock")),
        ("/native/opensslBuildRecipe/sourceDateEpoch", json!(1)),
        ("/native/opensslBuildRecipe/sourceDateEpoch", json!("0")),
        ("/native/opensslBuildRecipe/sourceDateEpoch", json!(0.0)),
        ("/native/opensslBuildRecipe/sourceDateEpoch", json!(true)),
        ("/native/opensslBuildRecipe/sourceDateEpoch", Value::Null),
        (
            "/native/opensslBuildRecipe",
            json!({"profile":"ops-openssl-build-v1","sourceDateEpoch":0,"extra":true}),
        ),
        (
            "/native/sourceArchivePins/openssl/sha256",
            json!("e".repeat(64)),
        ),
        ("/native/sourceArchivePins/musl/bytes", json!(1)),
        ("/native/sourceArchivePins/musl/version", json!("1.2.6")),
        ("/native/configdataSha256", Value::Null),
        ("/native/stockMuslSpecsSha256", json!("PRIVATE-invalid")),
        ("/native/disabled/shared", json!(false)),
        ("/native/disabled/module", json!("true")),
        ("/native/outputInventory/algorithm", json!("sha256")),
        ("/native/outputInventory/sha256", Value::Null),
        ("/native/outputInventory/files", json!(19)),
        ("/native/outputInventory/bytes", json!(1)),
        ("/native/outputInventory/mode", json!("0755")),
        (
            "/native/libraries/native~1musl~1lib~1crt1.o/format",
            json!("ELF32"),
        ),
        (
            "/native/libraries/native~1musl~1lib~1crt1.o/objects",
            json!(2),
        ),
        (
            "/native/libraries/native~1musl~1lib~1libc.a/objects",
            json!(0),
        ),
        (
            "/native/libraries/native~1musl~1lib~1libm.a/objects",
            json!(1),
        ),
        (
            "/native/libraries/native~1openssl~1lib~1libssl.a/bytes",
            json!(134217729),
        ),
        (
            "/native/libraries/native~1openssl~1lib~1libcrypto.a/sha256",
            Value::Null,
        ),
        ("/native/licenseNotices/musl/bytes", json!(0)),
    ] {
        let fixture = fixture::Fixture::new();
        let context = prepare(&fixture, Some(("hosted-kit.json", pointer, value)));
        refuse_signing(&fixture, &context);
    }
}

fn refuse_signing(fixture: &fixture::Fixture, context: &Context) {
    let budget = Budget::default();
    fixture::expect_error(
        signing::prepare(
            context,
            &fixture.root.join("unsigned"),
            &fixture.root.join("signed"),
            &budget,
            &mut || Ok(()),
        ),
        1,
    );
    assert!(!fixture.root.join("signed").exists());
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts().1, 0);
}

#[test]
fn signer_requires_selected_guest_recipe_observation_and_actual_measured_shims() {
    for (name, pointer, value) in [
        (
            "producer-recipe.json",
            "/profile",
            json!("ops-producer-recipe-v1"),
        ),
        (
            "producer-recipe.json",
            "/rustflags",
            json!("-Cprefer-dynamic"),
        ),
        ("producer-recipe.json", "/features", json!([])),
        ("producer-recipe.json", "/defaultFeatures", json!(true)),
        ("producer-recipe.json", "/candidateExecuted", json!(true)),
        (
            "producer-recipe.json",
            "/environmentNames",
            json!(["PATH", "LD_PRELOAD"]),
        ),
        (
            "producer-resolution.json",
            "/profile",
            json!("ops-producer-resolution-v1"),
        ),
        ("producer-resolution.json", "/opsFeatures", json!([])),
        ("producer-resolution.json", "/originalSha256", Value::Null),
        ("producer-resolution.json", "/originalBytes", json!(0)),
        ("producer-resolution.json", "/workspaceMembers", json!(65)),
        (
            "producer-toolchain.json",
            "/selectedTools/muslCc/sha256",
            json!("e".repeat(64)),
        ),
        (
            "producer-toolchain.json",
            "/selectedTools/hostCc/bytes",
            json!(1),
        ),
        (
            "producer-toolchain.json",
            "/selectedTools/cargo/sha256",
            Value::Null,
        ),
        (
            "producer-toolchain.json",
            "/versionProbes/0/version",
            json!("1.98.0"),
        ),
        (
            "producer-toolchain.json",
            "/sysroot/selectedStagedSysroot",
            json!(false),
        ),
        (
            "producer-toolchain.json",
            "/authentication",
            json!("selfReported"),
        ),
        (
            "producer-toolchain.json",
            "/runtimeClosure",
            json!("proven"),
        ),
        (
            "producer-toolchain.json",
            "/selectedTools/nativeLicenseNotices/0/text",
            json!("other"),
        ),
        (
            "producer-toolchain.json",
            "/selectedTools/nativeLicenseNotices/1/component",
            json!("musl"),
        ),
    ] {
        let fixture = fixture::Fixture::new();
        let context = prepare(&fixture, Some((name, pointer, value)));
        refuse_signing(&fixture, &context);
    }
}

#[test]
fn missing_native_roles_and_paired_null_license_hashes_never_supply_signing_authority() {
    for (name, parent, key) in [
        ("hosted-kit.json", "", "native"),
        ("hosted-kit.json", "/native", "licenseNotices"),
        ("hosted-kit.json", "/native", "opensslBuildRecipe"),
        ("hosted-kit.json", "/native/sourceArchivePins", "openssl"),
        (
            "hosted-kit.json",
            "/native/libraries",
            "native/musl/lib/rcrt1.o",
        ),
        ("producer-toolchain.json", "/selectedTools", "muslCc"),
        (
            "producer-toolchain.json",
            "/selectedTools",
            "nativeLicenseNotices",
        ),
        ("producer-resolution.json", "", "opsFeatures"),
    ] {
        let fixture = fixture::Fixture::new();
        let context = context(&fixture.root);
        let mut docs = documents(&context);
        let mut data: Value = serde_json::from_slice(&docs[name]).unwrap();
        data.pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        docs.insert(name.into(), provenance::encoded(&data).unwrap());
        let context = assembled(&fixture, context, docs);
        refuse_signing(&fixture, &context);
    }
    for missing in [false, true] {
        let fixture = fixture::Fixture::new();
        let context = context(&fixture.root);
        let mut docs = documents(&context);
        for (name, parent) in [
            ("hosted-kit.json", "/native/licenseNotices/musl"),
            (
                "producer-toolchain.json",
                "/selectedTools/nativeLicenseNotices/0",
            ),
        ] {
            let mut data: Value = serde_json::from_slice(&docs[name]).unwrap();
            let object = data.pointer_mut(parent).unwrap().as_object_mut().unwrap();
            if missing {
                object.remove("sha256");
            } else {
                object.insert("sha256".into(), Value::Null);
            }
            docs.insert(name.into(), provenance::encoded(&data).unwrap());
        }
        let context = assembled(&fixture, context, docs);
        refuse_signing(&fixture, &context);
    }
}

#[test]
fn a_self_consistent_changed_notice_still_must_join_the_hosted_source_bytes() {
    let fixture = fixture::Fixture::new();
    let context = context(&fixture.root);
    let mut docs = documents(&context);
    let name = "producer-toolchain.json";
    let mut data: Value = serde_json::from_slice(&docs[name]).unwrap();
    data["selectedTools"]["nativeLicenseNotices"][0]["text"] = json!("changed notice\n");
    data["selectedTools"]["nativeLicenseNotices"][0]["sha256"] =
        json!(super::super::sha(b"changed notice\n"));
    docs.insert(name.into(), provenance::encoded(&data).unwrap());
    let context = assembled(&fixture, context, docs);
    refuse_signing(&fixture, &context);
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
