//! Owned-file mutations cannot become new CA or capture baselines.

use super::*;
use crate::ci::release_http_policy::fixture::Fixture;
use std::{
    fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn ca_symlink_wrong_digest_size_and_later_same_inode_rewrite_are_refused() {
    for kind in ["symlink", "digest", "size", "rewrite", "replacement"] {
        let mut f = Fixture::new();
        if kind == "symlink" {
            let other = f.root.join("other");
            fs::rename(&f.config.ca, &other).unwrap();
            symlink(other, &f.config.ca).unwrap();
        } else if kind == "digest" {
            f.config.sha256 = "d".repeat(64);
        } else if kind == "size" {
            fs::write(&f.config.ca, vec![b'x'; 65537]).unwrap();
        }
        if matches!(kind, "symlink" | "digest" | "size") {
            assert!(crate::ci::release_http_policy::ca::Ca::open(&f.config).is_err());
        } else {
            let (ca, _) = crate::ci::release_http_policy::ca::Ca::open(&f.config).unwrap();
            if kind == "rewrite" {
                fs::write(&f.config.ca, b"changed ascii!").unwrap();
            } else {
                fs::remove_file(&f.config.ca).unwrap();
                fs::write(&f.config.ca, b"inert ascii CA").unwrap();
            }
            assert!(ca.finish().is_err());
        }
    }
}
#[tokio::test]
async fn original_finished_capture_rejects_same_length_rewrite_and_special_mode_adoption() {
    for change_mode in [false, true] {
        let f = Fixture::new();
        let scratch = Scratch::open(&f.root).unwrap();
        let (writer, mut reader) = scratch.anonymous().unwrap();
        let mut alias = writer.try_clone().unwrap();
        let log = Log::start(writer, 64).unwrap();
        {
            let mut pipe = log.writer().unwrap();
            pipe.write_all(b"ORIGINAL").unwrap();
            close(pipe).unwrap();
        }
        let finished = log.finish().await.unwrap();
        assert!(finished.complete);
        let seal = Seal {
            sha256: finished.sha256,
            bytes: finished.bytes,
            metadata: finished.metadata,
        };
        if change_mode {
            alias
                .set_permissions(fs::Permissions::from_mode(0o4600))
                .unwrap();
        } else {
            alias.rewind().unwrap();
            alias.write_all(b"REPLACED").unwrap();
        }
        let read = reader.bytes(64, &seal);
        let alias = close(alias);
        let reader = reader.finish();
        let scratch = scratch.finish();
        assert!(read.is_err());
        alias.unwrap();
        reader.unwrap();
        scratch.unwrap();
    }
}

#[tokio::test]
async fn second_constructor_refusal_awaits_first_log_and_closes_its_reader_and_both_pipe_ends() {
    use std::os::fd::AsRawFd;
    let f = Fixture::new();
    let scratch = Scratch::open(&f.root).unwrap();
    let mut observations = Vec::new();
    let result = scratch
        .capture_with(|log, reader| {
            for descriptor in log.pipe_fds().into_iter().chain([reader.file.as_raw_fd()]) {
                let path = format!("/dev/fd/{descriptor}");
                observations.push((path.clone(), fs::metadata(path).unwrap()));
            }
            Err(fail(42, "fixed_second_constructor"))
        })
        .await;
    let closed = scratch.finish();
    assert_eq!(result.err().unwrap().code, 42);
    for (path, before) in observations {
        assert!(
            !fs::metadata(path)
                .is_ok_and(|after| before.dev() == after.dev() && before.ino() == after.ino())
        );
    }
    closed.unwrap();
}
