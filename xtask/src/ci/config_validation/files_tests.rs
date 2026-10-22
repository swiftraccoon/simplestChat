//! Deterministic owned filesystem replacements; never host inputs or engines.

use super::*;
use crate::ci::config_validation::{plan::Kind, tests::Fixture};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};

#[test]
fn fixed_private_records_are_exclusive_and_symlinks_or_large_inputs_refuse() {
    let fixture = Fixture::new(Kind::Caddy);
    let artifacts = Artifacts::new(&fixture.context).unwrap();
    artifacts.write("ownership.json", b"{}\n").unwrap();
    assert!(artifacts.write("ownership.json", b"PRIVATE").is_err());
    assert!(artifacts.write("../PRIVATE", b"PRIVATE").is_err());
    let path = fixture.artifacts().join("ownership.json");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"{}\n");
    artifacts.finish().unwrap();
    let source = fixture.root.join("Caddyfile");
    std::fs::rename(&source, fixture.root.join("retired")).unwrap();
    symlink(fixture.root.join("retired"), &source).unwrap();
    assert!(Inputs::read(&fixture.context).is_err());
    std::fs::remove_file(&source).unwrap();
    std::fs::write(&source, vec![b'x'; INPUT_LIMIT as usize + 1]).unwrap();
    assert!(Inputs::read(&fixture.context).is_err());
}

#[test]
fn selected_source_mutation_and_one_close_failure_attempt_every_acquired_close() {
    let fixture = Fixture::new(Kind::Promtool);
    let inputs = Inputs::read(&fixture.context).unwrap();
    std::fs::write(
        fixture
            .root
            .join("ops/ansible/tests/monitoring-rules.test.yml"),
        "changed",
    )
    .unwrap();
    let count = Cell::new(0);
    assert!(
        inputs
            .finish_with(|file| {
                close(file)?;
                count.set(count.get() + 1);
                Err(failure())
            })
            .is_err()
    );
    assert_eq!(count.get(), 2);
}

#[test]
fn late_leaf_replacement_cannot_redirect_payload_to_the_foreign_namespace() {
    let fixture = Fixture::new(Kind::Caddy);
    let artifacts = Artifacts::new(&fixture.context).unwrap();
    let original = fixture.artifacts();
    let retired = fixture.root.join("retired-prefix");
    let result = artifacts.log_with(64, || {
        std::fs::rename(&original, &retired).unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&original)
            .unwrap();
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read_dir(&original).unwrap().count(), 0);
    assert!(artifacts.finish().is_err());
}

#[test]
fn bound_log_read_refuses_replacement_hardlinks_modes_size_and_unknown_metadata() {
    for bad in ["replace", "link", "mode", "size"] {
        let fixture = Fixture::new(Kind::Caddy);
        let artifacts = Artifacts::new(&fixture.context).unwrap();
        let (mut file, log) = artifacts.log(64).unwrap();
        file.write_all(b"original").unwrap();
        close(file).unwrap();
        let path = fixture.artifacts().join(&log.name);
        match bad {
            "replace" => {
                std::fs::rename(&path, fixture.artifacts().join("retired")).unwrap();
                std::fs::write(&path, b"PRIVATE_REPLACEMENT").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            "link" => std::fs::hard_link(&path, fixture.artifacts().join("linked")).unwrap(),
            "mode" => {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap()
            }
            "size" => std::fs::write(&path, vec![b'x'; 65]).unwrap(),
            _ => unreachable!(),
        }
        assert!(artifacts.read_log(&log).is_err(), "{bad}");
        artifacts.finish().unwrap();
    }
}

#[test]
fn current_owner_private_directory_and_log_count_bounds_gate_payload_admission() {
    let fixture = Fixture::new(Kind::Caddy);
    let artifacts = Artifacts::new(&fixture.context).unwrap();
    std::fs::set_permissions(fixture.artifacts(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(artifacts.write("ownership.json", b"PRIVATE").is_err());
    assert!(artifacts.log(64).is_err());
    assert!(artifacts.finish().is_err());
    let fixture = Fixture::new(Kind::Caddy);
    let artifacts = Artifacts::new(&fixture.context).unwrap();
    artifacts.next.set(FILE_LIMIT);
    assert!(artifacts.log(64).is_err());
    assert!(artifacts.log(LOG_LIMIT + 1).is_err());
    artifacts.finish().unwrap();
}
