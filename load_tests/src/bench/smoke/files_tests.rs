//! Owned private-file fault fixtures. No server or socket is opened.

use super::{
    files::{Output, Selected},
    fixtures::{Fixture, records},
};
use serde_json::json;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
};

fn recorder(fixture: &Fixture, bytes: &[u8]) {
    let path = fixture.config.output.join("server-diagnostics.jsonl");
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn recorder_must_exist_at_readiness_and_cannot_be_adopted_later() {
    let fixture = Fixture::new(json!({}), true);
    let mut output = Output::new(&fixture.config).unwrap();
    assert!(output.observe_recorder().is_err());
    recorder(&fixture, &records(&json!({})));
    assert!(output.take_recorder().is_err());
    output.finish().unwrap();
}

#[test]
fn original_recorder_may_grow_but_replacement_and_loosened_mode_refuse() {
    for replacement in [false, true] {
        let fixture = Fixture::new(json!({}), true);
        let mut output = Output::new(&fixture.config).unwrap();
        recorder(&fixture, b"");
        output.observe_recorder().unwrap();
        let path = fixture.config.output.join("server-diagnostics.jsonl");
        if replacement {
            fs::rename(&path, fixture.config.output.join("original.jsonl")).unwrap();
        }
        recorder(&fixture, &records(&json!({})));
        let result = output.take_recorder().unwrap().report(|| true);
        if replacement {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap()["coverage"]["complete"], true);
        }
        output.finish().unwrap();
    }
    let fixture = Fixture::new(json!({}), true);
    let mut output = Output::new(&fixture.config).unwrap();
    recorder(&fixture, &records(&json!({})));
    output.observe_recorder().unwrap();
    fs::set_permissions(
        fixture.config.output.join("server-diagnostics.jsonl"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(output.take_recorder().unwrap().report(|| true).is_err());
    output.finish().unwrap();
}

#[test]
fn recorder_admission_refuses_links_special_files_and_replaced_output() {
    for kind in ["symlink", "hardlink", "directory", "namespace"] {
        let fixture = Fixture::new(json!({}), true);
        let mut output = Output::new(&fixture.config).unwrap();
        let path = fixture.config.output.join("server-diagnostics.jsonl");
        match kind {
            "symlink" => std::os::unix::fs::symlink(&fixture.config.binary, &path).unwrap(),
            "hardlink" => {
                let original = fixture.root.join("private");
                fs::write(&original, b"PRIVATE").unwrap();
                fs::set_permissions(&original, fs::Permissions::from_mode(0o600)).unwrap();
                fs::hard_link(original, &path).unwrap();
            }
            "directory" => fs::create_dir(&path).unwrap(),
            _ => {
                fs::rename(&fixture.config.output, fixture.root.join("original")).unwrap();
                fs::create_dir(&fixture.config.output).unwrap();
                recorder(&fixture, b"PRIVATE");
            }
        }
        assert!(output.observe_recorder().is_err(), "{kind}");
        if kind == "namespace" {
            assert!(output.finish().is_err());
            assert_eq!(fs::read(path).unwrap(), b"PRIVATE");
        } else {
            output.finish().unwrap();
        }
    }
}

#[test]
fn selected_original_executable_and_ancestor_binding_survive_until_close() {
    let fixture = Fixture::new(json!({}), false);
    let selected = Selected::open(&fixture.config.binary).unwrap();
    let original = fixture.root.join("original-server");
    fs::rename(&fixture.config.binary, &original).unwrap();
    fs::copy(&original, &fixture.config.binary).unwrap();
    assert!(selected.finish().is_err());
    assert_eq!(fs::read(original).unwrap(), b"#!/bin/sh\nexit 99\n");
    let selected = Selected::open(&fixture.config.binary).unwrap();
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(selected.finish().is_err());
}

#[test]
fn owned_reader_closes_on_invalid_bound_and_checkpoint_and_clips_original_length() {
    let fixture = Fixture::new(json!({}), true);
    fs::create_dir(&fixture.config.output).unwrap();
    let path = fixture.config.output.join("server-diagnostics.jsonl");
    for bound in [0, crate::report::diagnostics::MAX_RECORDS + 1] {
        recorder(&fixture, &records(&json!({})));
        let file = File::open(&path).unwrap();
        let fd = file.as_raw_fd();
        assert!(crate::report::diagnostics::read_owned(file, bound, || true).is_err());
        assert!(fs::metadata(format!("/dev/fd/{fd}")).is_err());
    }
    let file = File::open(&path).unwrap();
    let fd = file.as_raw_fd();
    let report = crate::report::diagnostics::read_owned(file, 10, || false).unwrap();
    let value = serde_json::to_value(report).unwrap();
    assert_eq!(value["coverage"]["complete"], false);
    assert!(fs::metadata(format!("/dev/fd/{fd}")).is_err());
    let file = File::open(&path).unwrap();
    let mut appended = false;
    let report = crate::report::diagnostics::read_owned(file, 10, || {
        if !appended {
            appended = true;
            OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"PRIVATE_TRAILING_BYTES\n")
                .unwrap();
        }
        true
    })
    .unwrap();
    let value = serde_json::to_value(report).unwrap();
    assert_eq!(value["coverage"]["complete"], true);
    assert!(!value.to_string().contains("PRIVATE"));
}
