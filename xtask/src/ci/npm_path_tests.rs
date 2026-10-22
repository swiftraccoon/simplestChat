use super::*;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    root: PathBuf,
    temporary: PathBuf,
    target: PathBuf,
    environment: tooling::Environment,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "simplestchat-npm-path-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let temporary = root.join("runner space é");
        fs::create_dir(&temporary).unwrap();
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(temporary.join(PREFIX).join("bin")).unwrap();
        let target = root.join("runner-path");
        fs::write(&target, b"prior/path\n").unwrap();
        let environment = [
            ("RUNNER_TEMP".into(), temporary.clone().into_os_string()),
            ("GITHUB_PATH".into(), target.clone().into_os_string()),
        ]
        .into_iter()
        .collect();
        Self {
            root,
            temporary,
            target,
            environment,
        }
    }
    fn unchanged(&self) {
        assert_eq!(fs::read(&self.target).unwrap(), b"prior/path\n");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn publishes_only_the_fixed_literal_directory_preserving_existing_file_and_mode() {
    let fixture = Fixture::new();
    let mode = fs::metadata(&fixture.target).unwrap().permissions().mode();
    publish(&fixture.environment).unwrap();
    assert_eq!(
        fs::read(&fixture.target).unwrap(),
        format!("prior/path\n{}/{PREFIX}/bin\n", fixture.temporary.display()).as_bytes()
    );
    assert_eq!(
        fs::metadata(&fixture.target).unwrap().permissions().mode(),
        mode
    );
    let installer = include_str!("../../../build/install-npm.mjs");
    assert!(installer.contains("export const NPM_VERSION = '12.2.0';"));
}

#[test]
fn refuses_missing_relative_and_line_injecting_inputs_before_writing() {
    let fixture = Fixture::new();
    for name in ["RUNNER_TEMP", "GITHUB_PATH"] {
        let mut env = fixture.environment.clone();
        env.remove(std::ffi::OsStr::new(name));
        assert!(publish(&env).is_err());
        env.insert(name.into(), "relative".into());
        assert!(publish(&env).is_err());
    }
    for suffix in ["\ninjected", "\r", "\0", "\t", "\u{85}"] {
        let mut env = fixture.environment.clone();
        env.insert(
            "RUNNER_TEMP".into(),
            format!("{}{suffix}", fixture.temporary.display()).into(),
        );
        assert!(publish(&env).is_err());
    }
    fixture.unchanged();
}

#[test]
fn never_creates_truncates_or_follows_special_runner_output() {
    let fixture = Fixture::new();
    for name in ["missing", "directory", "link", "fifo"] {
        let path = fixture.root.join(name);
        match name {
            "directory" => fs::create_dir(&path).unwrap(),
            "link" => symlink(&fixture.target, &path).unwrap(),
            "fifo" => assert!(
                std::process::Command::new("mkfifo")
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            ),
            _ => (),
        }
        let mut env = fixture.environment.clone();
        env.insert("GITHUB_PATH".into(), path.clone().into_os_string());
        assert!(publish(&env).is_err());
        if name == "missing" {
            assert!(!path.exists());
        }
        fixture.unchanged();
    }
}

#[test]
fn requires_real_owned_protected_runner_prefix_and_bin_directories() {
    for level in ["runner", "prefix", "bin"] {
        for kind in ["missing", "link", "file", "writable"] {
            let fixture = Fixture::new();
            let path = match level {
                "runner" => fixture.temporary.clone(),
                "prefix" => fixture.temporary.join(PREFIX),
                _ => fixture.temporary.join(PREFIX).join("bin"),
            };
            let saved = fixture.root.join("saved");
            fs::rename(&path, &saved).unwrap();
            match kind {
                "link" => symlink(&saved, &path).unwrap(),
                "file" => fs::write(&path, b"not directory").unwrap(),
                "writable" => {
                    fs::create_dir(&path).unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();
                }
                _ => (),
            }
            assert!(publish(&fixture.environment).is_err(), "{level} {kind}");
            fixture.unchanged();
        }
    }
}

#[test]
fn every_descriptor_is_closed_after_an_operation_or_close_failure() {
    let fixture = Fixture::new();
    for operation in [Ok(()), Err(())] {
        for failed_close in [None, Some(0), Some(2)] {
            let files = (0..4)
                .map(|_| File::open(&fixture.target).unwrap())
                .collect();
            let mut count = 0;
            let outcome = finish(operation, files, |file| {
                nix::unistd::close(file).unwrap();
                let failed = failed_close == Some(count);
                count += 1;
                if failed { Err(()) } else { Ok(()) }
            });
            assert_eq!(count, 4);
            assert_eq!(outcome.is_ok(), operation.is_ok() && failed_close.is_none());
        }
    }
}
