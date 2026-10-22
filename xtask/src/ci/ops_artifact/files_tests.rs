//! Owned filesystem regressions; no compiler or candidate executes.

use super::{fixture::*, namespace::*, *};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn fresh_output_refuses_existing_and_has_private_mode() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    assert!(OwnedRun::fixture(&fixture.output, &budget).is_err());
    run.record("producer-outcome.json", b"private bytes")
        .unwrap();
    assert_eq!(
        fs::metadata(fixture.output.join("producer-outcome.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

#[test]
fn checked_path_join_has_no_split_or_ambient_directory_fallback() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let run = OwnedRun::fixture(&fixture.root.join("colon:run"), &budget).unwrap();
    assert!(recipe::Recipe::environment(&run).is_err());
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
#[test]
fn outer_shared_directory_authority_and_empty_writable_cargo_are_refused() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let chain = Chain::open(&fixture.root, &budget).unwrap();
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(chain.controlled().is_err());
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700)).unwrap();
    chain.finish().unwrap();
    let chain = Chain::open(&fixture.root, &budget).unwrap();
    fs::create_dir(fixture.root.join(".cargo")).unwrap();
    fs::set_permissions(
        fixture.root.join(".cargo"),
        fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    assert!(chain.configuration_absent().is_err());
    fs::set_permissions(
        fixture.root.join(".cargo"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(chain.configuration_absent().is_ok());
    fs::write(fixture.root.join(".cargo/config"), b"[env]\n").unwrap();
    assert!(chain.configuration_absent().is_err());
    chain.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
#[test]
fn ancestor_sibling_creation_does_not_break_selected_namespace() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let chain = Chain::open(&fixture.source, &budget).unwrap();
    fs::write(fixture.root.join("unrelated-sibling"), b"inert").unwrap();
    chain.verify().unwrap();
    chain.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
#[test]
fn namespace_and_private_owner_changes_do_not_get_adopted() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    fs::set_permissions(&fixture.output, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(run.check().is_err());
    assert!(run.finish().is_err());
    assert_eq!(budget.count(), 0);
    fs::remove_dir(&fixture.output).unwrap();
    symlink(&fixture.source, &fixture.output).unwrap();
    assert!(OwnedRun::fixture(&fixture.output, &budget).is_err());
    assert_eq!(budget.count(), 0);
}
#[tokio::test]
async fn material_symlinks_and_special_aliases_are_refused_without_spawn_or_leaks() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut executor = Noop;
    symlink("Cargo.toml", fixture.source.join("alias")).unwrap();
    assert!(
        materials::measure(&fixture.source, &budget, &mut executor, &deadline)
            .await
            .is_err()
    );
    assert_eq!(budget.count(), 0);
    fs::remove_file(fixture.source.join("alias")).unwrap();
    fs::hard_link(
        fixture.source.join("Cargo.toml"),
        fixture.source.join("alias"),
    )
    .unwrap();
    assert!(
        materials::measure(&fixture.source, &budget, &mut executor, &deadline)
            .await
            .is_err()
    );
    assert_eq!(budget.count(), 0);
}
#[tokio::test]
async fn measured_source_and_staged_changes_are_rejected_before_receipt() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let request = fixture.request(&budget).await;
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut executor = Noop;
    let materials = materials::Materials::stage(&request, &run, &mut executor, &deadline)
        .await
        .unwrap();
    fs::write(
        run.path.join("source/ops/src/main.rs"),
        b"// different bytes\n",
    )
    .unwrap();
    assert!(
        materials
            .verify(&run, &mut executor, &deadline)
            .await
            .is_err()
    );
    materials.finish().unwrap();
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
#[tokio::test]
async fn executable_admission_uses_measured_stamps_not_new_baseline() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let request = fixture.request(&budget).await;
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut executor = Noop;
    let materials = materials::Materials::stage(&request, &run, &mut executor, &deadline)
        .await
        .unwrap();
    let cargo = run.path.join("kit/toolchain/bin/cargo");
    let mut bytes = fs::read(&cargo).unwrap();
    bytes[0] = b'X';
    fs::write(&cargo, bytes).unwrap();
    assert!(recipe::Tools::open(&run, &materials).is_err());
    materials.finish().unwrap();
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn equal_length_destination_edit_between_hash_and_stamp_admission_is_refused() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let run = OwnedRun::fixture(&fixture.output, &budget).unwrap();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut executor = Noop;
    let mut changed = false;
    let mut after = |name: &str, file: &std::fs::File| {
        if name == "ops/src/main.rs" {
            let mut file = file;
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(b"XX").unwrap();
            file.sync_all().unwrap();
            changed = true;
        }
    };
    assert!(
        materials::copy_after(&fixture.source, &run, &mut executor, &deadline, &mut after)
            .await
            .is_err()
    );
    assert!(changed);
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
