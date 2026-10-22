//! Owned inert fixtures for the native DTLS controller.

use super::*;
use crate::ci::native_dtls::fixtures::Fixture;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[tokio::test]
async fn private_snapshot_rewrites_only_copy_and_keeps_failure_logs_with_no_build_trees() {
    let fixture = Fixture::new();
    let original = fixture
        .context
        .root
        .join("vendor/mediasoup-sys-0.19.0/meson.build");
    let before = fs::read(&original).unwrap();
    let owned = Owned::new(&fixture.context).unwrap();
    let path = owned.path.clone();
    owned.prepare(&fixture.context).unwrap();
    let meson = fs::read_to_string(path.join("worker/meson.build")).unwrap();
    assert!(meson.contains("test_sources = [\n"));
    assert!(meson.contains("production = ['all.cpp']"));
    assert!(meson.contains("test/src/nested/media.cpp"));
    assert_eq!(fs::read(original).unwrap(), before);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o700);
    for name in plan::LOGS {
        let log = owned.log(name).unwrap();
        log.finish().await.unwrap();
    }
    let (parent, cleaned) = owned.cleanup(false);
    cleaned.unwrap();
    parent.finish().unwrap();
    assert!(!path.join("worker").exists());
    assert!(plan::LOGS.iter().all(|name| path.join(name).is_file()));
}

#[test]
fn symlink_special_oversized_and_deep_source_are_refused_without_touching_targets() {
    for case in ["link", "fifo", "large", "deep"] {
        let fixture = Fixture::new();
        let source = fixture.context.root.join("vendor/mediasoup-sys-0.19.0");
        let private = fixture.root.join("foreign");
        fs::write(&private, "preserved").unwrap();
        match case {
            "link" => symlink(&private, source.join("linked")).unwrap(),
            "fifo" => {
                assert!(
                    std::process::Command::new("mkfifo")
                        .arg(source.join("fifo"))
                        .status()
                        .unwrap()
                        .success()
                );
            }
            "large" => File::create(source.join("large"))
                .unwrap()
                .set_len(16 * 1024 * 1024 + 1)
                .unwrap(),
            _ => fs::create_dir_all(source.join("deep/".repeat(34))).unwrap(),
        }
        let owned = Owned::new(&fixture.context).unwrap();
        let path = owned.path.clone();
        assert!(owned.prepare(&fixture.context).is_err());
        let (parent, cleaned) = owned.cleanup(false);
        cleaned.unwrap();
        parent.finish().unwrap();
        assert!(!path.join("worker").exists());
        assert_eq!(fs::read_to_string(private).unwrap(), "preserved");
    }
}

#[test]
fn source_entry_count_and_cleanup_budget_are_real_limits() {
    let fixture = Fixture::new();
    let source = fixture.context.root.join("vendor/mediasoup-sys-0.19.0");
    for index in 0..4097 {
        fs::write(source.join(format!("small-{index}")), b"").unwrap();
    }
    let owned = Owned::new(&fixture.context).unwrap();
    assert!(owned.prepare(&fixture.context).is_err());
    let (parent, cleaned) = owned.cleanup(true);
    cleaned.unwrap();
    parent.finish().unwrap();
    let root = Directory::open(&fixture.context.temporary).unwrap();
    for name in ["one", "two"] {
        fs::write(fixture.context.temporary.join(name), b"owned").unwrap();
    }
    assert!(!remove_contents_bounded(root.file(), 0, &mut 0, 1, 16));
    assert_eq!(fs::read_dir(&fixture.context.temporary).unwrap().count(), 1);
    root.finish().unwrap();
}

#[test]
fn changed_or_initially_permissive_namespace_never_becomes_a_private_execution_prefix() {
    let fixture = Fixture::new();
    let owned = Owned::new(&fixture.context).unwrap();
    fs::set_permissions(&owned.path, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(owned.check().is_err());
    fs::set_permissions(&owned.path, fs::Permissions::from_mode(0o700)).unwrap();
    let (parent, cleaned) = owned.cleanup(true);
    cleaned.unwrap();
    parent.finish().unwrap();
    let path = fixture.context.temporary.join("permissive");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let owned = Owned {
        parent: Directory::open(&fixture.context.temporary).unwrap(),
        directory: Directory::open(&path).unwrap(),
        name: "permissive".into(),
        logs: RefCell::default(),
        path: path.clone(),
    };
    assert!(owned.check().is_err());
    let (parent, cleaned) = owned.cleanup(false);
    assert!(cleaned.is_err());
    parent.finish().unwrap();
    assert!(path.exists());
}

#[test]
fn shared_node_and_dtls_tool_selection_keep_path_order_slash_symlinks_and_single_values() {
    let fixture = Fixture::new();
    let bin = fixture.root.join("bin");
    let actual = bin.join("python named");
    symlink(&actual, bin.join("node")).unwrap();
    let node = selected::select(
        &fixture.context.cwd,
        &fixture.context.environment,
        OsStr::new("node"),
    )
    .unwrap();
    assert_eq!(node.validate().unwrap(), actual);
    node.close().unwrap();
    for name in [
        actual.into_os_string(),
        fixture
            .context
            .cwd
            .join("../bin/python named")
            .into_os_string(),
        "../bin/python named".into(),
        "python named".into(),
    ] {
        let proof =
            selected::select(&fixture.context.cwd, &fixture.context.environment, &name).unwrap();
        assert!(proof.validate().unwrap().ends_with("bin/python named"));
        proof.close().unwrap();
    }
    assert!(
        selected::select(
            &fixture.context.cwd,
            &fixture.context.environment,
            OsStr::new("python named --flag")
        )
        .is_err()
    );
    let tools = Tools::open(&fixture.context).unwrap();
    tools.validate().unwrap();
    tools.close().unwrap();
}

#[test]
fn captured_clean_environment_and_explicit_ci_marker_admission_do_not_inherit_private_options() {
    let fixture = Fixture::new();
    let prefix = fixture.context.temporary.join("owned");
    let environment = fixture.context.clean_environment(&prefix);
    let names: Vec<_> = environment
        .keys()
        .map(|key| key.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "BUILD_DIR",
            "CC",
            "CXX",
            "MEDIASOUP_INSTALL_DIR",
            "MEDIASOUP_OUT_DIR",
            "MEDIASOUP_TEST_TAGS",
            "MS_TEST_LOG_LEVEL",
            "MS_TEST_LOG_TAGS",
            "PATH",
            "PIP_CONFIG_FILE",
            "PIP_CONSTRAINT",
            "PIP_INDEX_URL",
            "PKG_CONFIG_PATH",
            "PYTHON",
            "PYTHONDONTWRITEBYTECODE",
            "PYTHONNOUSERSITE",
            "PYTHONPATH"
        ]
    );
    assert_eq!(environment[OsStr::new("PYTHON")], "python named");
    assert_eq!(
        environment[OsStr::new("PYTHONPATH")],
        prefix.join("out/pip_invoke").as_os_str()
    );
    assert!(!environment.values().any(|v| matches!(
        v.to_str(),
        Some(
            "private-module-injection"
                | "private-not-in-native-environment"
                | "https://private.invalid/"
        )
    )));
    assert!(
        Context::new(
            &fixture.context.root,
            &fixture.context.cwd,
            fixture.context.environment.clone(),
            "Linux",
            true
        )
        .is_err()
    );
    let mut environment = fixture.context.environment.clone();
    environment.insert("ACT".into(), "true".into());
    environment.insert("LOCAL_CI_DISPOSABLE".into(), "1".into());
    environment.insert("GITHUB_SHA".into(), "a".repeat(40).into());
    assert!(
        Context::new(
            &fixture.context.root,
            &fixture.context.cwd,
            environment.clone(),
            "Linux",
            true
        )
        .is_ok()
    );
    assert!(
        Context::new(
            &fixture.context.root,
            &fixture.context.cwd,
            environment,
            "Darwin",
            true
        )
        .is_err()
    );
}

#[test]
fn final_source_revalidation_refuses_in_place_change_to_an_already_closed_early_file() {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };
    let fixture = Fixture::new();
    let path = fixture.context.root.join("vendor/mediasoup-sys-0.19.0");
    let source = Directory::open(&path).unwrap();
    let target = fixture.root.join("copy");
    fs::create_dir(&target).unwrap();
    let target = Directory::open(&target).unwrap();
    let mut snapshot = snapshot::Snapshot::default();
    let first = Rc::new(RefCell::new(None::<String>));
    let count = Rc::new(Cell::new(0));
    snapshot.after_file = Some(Box::new(move |relative| {
        count.set(count.get() + 1);
        if count.get() == 1 {
            *first.borrow_mut() = Some(relative.to_owned());
        }
        if count.get() == 2 {
            fs::write(
                path.join(first.borrow().as_ref().unwrap()),
                b"in-place change",
            )
            .unwrap();
        }
    }));
    snapshot.copy(source.file(), target.file(), "", 0).unwrap();
    assert!(snapshot.verify(source.file()).is_err());
    source.finish().unwrap();
    target.finish().unwrap();
}

#[test]
fn namespace_witness_and_marker_uncertain_visibility_never_remove_foreign_entries() {
    use crate::ci::native_dtls::marker::Marker;
    for case in ["uncertain", "replacement", "namespace"] {
        let fixture = Fixture::new();
        let parent = Directory::open(&fixture.context.temporary).unwrap();
        let original = fixture.context.temporary.clone();
        let final_path = original.join("native-dtls-verified");
        let (marker, result) =
            Marker::publish_after(&parent, &original, &"a".repeat(40), "owned-test", || {
                match case {
                    "replacement" => {
                        fs::rename(&final_path, original.join("original-marker")).unwrap();
                        fs::write(&final_path, b"foreign-marker").unwrap();
                    }
                    "namespace" => {
                        fs::rename(&original, fixture.root.join("moved-original")).unwrap();
                        fs::create_dir(&original).unwrap();
                        fs::write(&final_path, b"foreign-marker").unwrap();
                    }
                    _ => {}
                }
                Err(failure(124))
            });
        assert_eq!(result.unwrap_err().code, 124);
        let _ = parent.finish();
        let rolled_back = marker.unwrap().rollback();
        if case == "uncertain" {
            rolled_back.unwrap();
            assert!(!final_path.exists());
        } else {
            assert!(rolled_back.is_err());
            assert_eq!(fs::read(final_path).unwrap(), b"foreign-marker");
        }
    }
}

#[test]
fn failed_leaf_admission_cannot_remove_a_foreign_empty_replacement() {
    let fixture = Fixture::new();
    let temporary = fixture.context.temporary.clone();
    let parked = fixture.root.join("parked-parent");
    let mut foreign = None;
    assert!(
        Owned::new_after(&fixture.context, |_, name| {
            fs::rename(&temporary, &parked).unwrap();
            fs::rename(parked.join(name), parked.join("original-nonce")).unwrap();
            let replacement = parked.join(name);
            fs::create_dir(&replacement).unwrap();
            foreign = Some(replacement);
            symlink(&parked, &temporary).unwrap();
        })
        .is_err()
    );
    assert!(foreign.unwrap().is_dir());
}

#[test]
fn venv_adjacent_selected_path_is_preserved_without_authenticating_its_configuration() {
    let fixture = Fixture::new();
    let venv = fixture.context.cwd.join("venv");
    fs::create_dir_all(venv.join("bin")).unwrap();
    fs::write(venv.join("pyvenv.cfg"), b"owned inert config").unwrap();
    let selected_path = venv.join("bin/python");
    symlink(fixture.root.join("bin/python named"), &selected_path).unwrap();
    let proof = selected::select(
        &fixture.context.cwd,
        &fixture.context.environment,
        selected_path.as_os_str(),
    )
    .unwrap();
    assert_eq!(proof.selected_path().unwrap(), selected_path);
    assert_eq!(
        proof.validate().unwrap(),
        fixture.root.join("bin/python named")
    );
    fs::write(
        venv.join("pyvenv.cfg"),
        b"different config, outside the executable proof",
    )
    .unwrap();
    assert_eq!(proof.selected_path().unwrap(), selected_path);
    proof.close().unwrap();
}
