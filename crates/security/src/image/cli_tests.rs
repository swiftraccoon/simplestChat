//! Tests of the two commands: what their command lines ask for, and how
//! they end before any evidence.

use super::{
    Asked, judged, parsed, prepare, prepared, preparing, private, readied, reported, scan,
    scanning, status,
};
use crate::{
    Fault, engine,
    image::{
        gate::{Options, Preparation},
        sandbox::Platform,
    },
    scratch::Scratch,
};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, atomic::AtomicBool},
};

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

/// The checkout this crate was built in.
fn repository() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().expect("the checkout")
}

#[test]
fn a_scan_names_its_image_and_every_option_once() {
    let line = "sha256:x --artifact-dir /a --output /o --tools-directory /t --engine podman \
        --platform linux/arm64 --database-cache /c";
    let asked = parsed(&words(line), true).expect("a scan");
    assert_eq!(
        asked,
        Asked {
            image: Some("sha256:x".to_owned()),
            artifact: Some("/a".into()),
            output: Some("/o".into()),
            tools: Some("/t".into()),
            engine: Some(engine::Name::Podman),
            platform: Some(Platform::Arm64),
            database_cache: Some("/c".into()),
        }
    );
    let (options, chosen) = scanning(asked).expect("a whole scan");
    assert_eq!(chosen, engine::Name::Podman);
    assert_eq!(options.image_id, "sha256:x");
    assert_eq!(options.artifact, PathBuf::from("/a"));
    assert_eq!(options.output, PathBuf::from("/o"));
    assert_eq!(options.tools, PathBuf::from("/t"));
    assert_eq!(options.platform, Platform::Arm64);
    assert_eq!(options.database_cache, Some(PathBuf::from("/c")));
    let line = "--output /o sha256:x --tools-directory /t --artifact-dir /a";
    let (options, chosen) = parsed(&words(line), true)
        .and_then(scanning)
        .expect("a scan");
    assert_eq!(chosen, engine::Name::Docker);
    assert_eq!(options.platform, Platform::Amd64);
    assert_eq!(options.database_cache, None);
}

#[test]
fn a_preparation_names_no_image_and_needs_its_cache() {
    let line = "--output /o --tools-directory /t --database-cache /c";
    let (preparation, chosen) = parsed(&words(line), false)
        .and_then(preparing)
        .expect("one");
    assert_eq!(chosen, engine::Name::Docker);
    assert_eq!(preparation.output, PathBuf::from("/o"));
    assert_eq!(preparation.tools, PathBuf::from("/t"));
    assert_eq!(preparation.platform, Platform::Amd64);
    assert_eq!(preparation.database_cache, Some(PathBuf::from("/c")));
    for line in [
        "--tools-directory /t --database-cache /c",
        "--output /o --database-cache /c",
        "--output /o --tools-directory /t",
    ] {
        assert!(
            parsed(&words(line), false).and_then(preparing).is_none(),
            "{line}"
        );
    }
    for line in [
        "sha256:x --output /o --tools-directory /t --database-cache /c",
        "--artifact-dir /a --output /o --tools-directory /t --database-cache /c",
    ] {
        assert!(parsed(&words(line), false).is_none(), "{line}");
    }
}

#[test]
fn a_command_line_that_is_none_of_theirs_is_refused() {
    let whole = "sha256:x --artifact-dir /a --output /o --tools-directory /t";
    assert!(parsed(&words(whole), true).and_then(scanning).is_some());
    for line in [
        "sha256:x sha256:y --artifact-dir /a --output /o --tools-directory /t",
        "sha256:x --artifact-dir /a --output /o --output /p --tools-directory /t",
        "sha256:x --artifact-dir /a --output /o --tools-directory /t --unknown /u",
        "sha256:x --artifact-dir /a --output -o --tools-directory /t",
        "sha256:x --artifact-dir /a --output /o --tools-directory",
        "sha256:x --artifact-dir /a --output /o --tools-directory /t --engine rkt",
        "sha256:x --artifact-dir /a --output /o --tools-directory /t --platform linux/riscv64",
        "sha256:x --artifact-dir /a --output /o --tools-directory /t --engine docker --engine podman",
        "sha256:x --artifact-dir /a --output /o --tools-directory /t --platform linux/amd64 --platform linux/arm64",
        "sha256:x --artifact-dir /a --artifact-dir /b --output /o --tools-directory /t",
        "sha256:x --artifact-dir /a --output /o --tools-directory /t --tools-directory /u",
        "sha256:x --artifact-dir /a --output /o --tools-directory /t --database-cache /c --database-cache /d",
        "-x --artifact-dir /a --output /o --tools-directory /t",
    ] {
        assert!(parsed(&words(line), true).is_none(), "{line}");
    }
    for line in [
        "--artifact-dir /a --output /o --tools-directory /t",
        "sha256:x --output /o --tools-directory /t",
        "sha256:x --artifact-dir /a --tools-directory /t",
        "sha256:x --artifact-dir /a --output /o",
    ] {
        assert!(
            parsed(&words(line), true).and_then(scanning).is_none(),
            "{line}"
        );
    }
}

#[test]
fn a_command_line_that_is_none_of_theirs_ends_with_the_usage() {
    let root = repository();
    assert_eq!(scan(&root, &words("--bogus")), ExitCode::from(2));
    assert_eq!(prepare(&root, &words("--output /o")), ExitCode::from(2));
}

#[test]
fn only_a_pass_whose_line_was_written_ends_well() {
    assert_eq!(status(true, true), ExitCode::SUCCESS);
    assert_eq!(status(true, false), ExitCode::from(1));
    assert_eq!(status(false, true), ExitCode::from(1));
    assert_eq!(status(false, false), ExitCode::from(1));
}

#[test]
fn a_scan_of_an_export_that_is_not_there_fails_before_any_evidence() {
    let scratch = Scratch::new("missing");
    let output = scratch.0.join("output");
    let options = Options {
        image_id: "sha256:x".to_owned(),
        artifact: scratch.0.join("absent"),
        output: output.clone(),
        tools: scratch.0.join("tools"),
        platform: Platform::Amd64,
        database_cache: None,
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let refused = judged(&repository(), &options, engine::Name::Docker, &cancelled);
    assert_eq!(refused, Err(Fault("tool_entry_missing")));
    assert!(!output.exists());
}

#[test]
fn a_preparation_of_relative_paths_fails_before_any_evidence() {
    let preparation = Preparation {
        output: "relative".into(),
        tools: "/t".into(),
        platform: Platform::Amd64,
        database_cache: Some("/c".into()),
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let refused = prepared(
        &repository(),
        &preparation,
        engine::Name::Docker,
        &cancelled,
    );
    assert_eq!(refused, Err(Fault("image_database_preparation_paths")));
    assert!(!Path::new("relative").exists());
}

#[test]
fn the_status_lines_say_whether_it_passed_and_where_the_evidence_is() {
    let output = Path::new("results/image");
    assert_eq!(
        reported(true, output),
        "Image security passed; evidence: results/image"
    );
    assert_eq!(
        reported(false, output),
        "Image security failed; evidence: results/image"
    );
    assert_eq!(readied(true), "Image database preparation passed");
    assert_eq!(readied(false), "Image database preparation failed");
}

#[test]
fn what_the_commands_create_is_this_accounts_alone() {
    let open = rustix::fs::Mode::from_raw_mode(0o022);
    let previous = rustix::process::umask(open);
    private();
    let made = rustix::process::umask(previous);
    assert_eq!(made, rustix::fs::Mode::from_raw_mode(0o077));
}
