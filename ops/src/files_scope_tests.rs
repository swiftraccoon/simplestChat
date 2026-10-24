//! Real retained-descriptor checkpoints; every original closes before assertions.

use super::*;
use crate::test_support::Fixture;
use std::{cell::Cell, fs};

#[test]
fn refused_entry_checkpoint_never_reaches_a_path_open_boundary() {
    let fixture = Fixture::new();
    let path = fixture.0.join("input");
    fs::write(&path, b"INERT").unwrap();
    let events = Cell::new(0_usize);
    let opened = Snapshot::open_checked(&path, 16, |event, value| {
        events.set(events.get() + 1);
        match event {
            SnapshotEvent::Check => Err(Error::Input),
            _ => value,
        }
    });
    let refused = match opened {
        Ok(snapshot) => {
            let _ = snapshot.finish();
            false
        }
        Err(error) => error == Error::Input,
    };
    drop(fixture);
    assert!(refused);
    assert_eq!(events.get(), 1);
}

#[test]
fn refusal_after_actual_root_open_still_closes_that_original_once() {
    let fixture = Fixture::new();
    let path = fixture.0.join("input");
    fs::write(&path, b"INERT").unwrap();
    let opens = Cell::new(0_usize);
    let closes = Cell::new(0_usize);
    let opened = Snapshot::open_scoped(
        &path,
        16,
        |file| {
            closes.set(closes.get() + 1);
            nix::unistd::close(file).map_err(|_| Error::Input)
        },
        |event, value| {
            if matches!(event, SnapshotEvent::Open) {
                opens.set(opens.get() + 1);
                if opens.get() == 2 {
                    return Err(Error::Input);
                }
            }
            value
        },
    );
    let refused = match opened {
        Ok(snapshot) => {
            let _ = snapshot.finish();
            false
        }
        Err(error) => error == Error::Input,
    };
    drop(fixture);
    assert!(refused);
    assert_eq!(opens.get(), 2);
    assert_eq!(closes.get(), 1);
}

#[test]
fn known_metadata_change_wins_over_late_checked_close_and_every_peer_closes() {
    let fixture = Fixture::new();
    let path = fixture.0.join("input");
    fs::write(&path, b"INERT").unwrap();
    let snapshot = Snapshot::open(&path, 16).unwrap();
    let expected = snapshot.directories.len() + 1;
    fs::write(&path, b"CHANGED").unwrap();
    let closes = Cell::new(0_usize);
    let first_observed = Cell::new(None);
    let result = snapshot.finish_checked_fault(
        |file| {
            closes.set(closes.get() + 1);
            let actual = nix::unistd::close(file).map_err(|_| Error::Input);
            actual.and(Err(Error::Input))
        },
        |_, value| {
            if let Err(error) = value
                && first_observed.get().is_none()
            {
                first_observed.set(Some(error));
            }
            value
        },
    );
    drop(fixture);
    assert_eq!(result, Err(Error::Changed));
    assert_eq!(first_observed.get(), Some(Error::Changed));
    assert_eq!(closes.get(), expected);
}

#[test]
fn late_close_checkpoint_refusal_never_skips_original_peer_closes() {
    let fixture = Fixture::new();
    let path = fixture.0.join("input");
    fs::write(&path, b"INERT").unwrap();
    let snapshot = Snapshot::open(&path, 16).unwrap();
    let expected = snapshot.directories.len() + 1;
    let closes = Cell::new(0_usize);
    let result = snapshot.finish_checked_fault(
        |file| {
            closes.set(closes.get() + 1);
            nix::unistd::close(file).map_err(|_| Error::Input)
        },
        |event, value| {
            if matches!(event, SnapshotEvent::Close) {
                Err(Error::Input)
            } else {
                value
            }
        },
    );
    drop(fixture);
    assert_eq!(result, Err(Error::Input));
    assert_eq!(closes.get(), expected);
}

#[test]
fn post_read_refusal_invalidates_bytes_from_the_actual_original_stream() {
    let fixture = Fixture::new();
    let path = fixture.0.join("input");
    fs::write(&path, b"INERT").unwrap();
    let mut snapshot = Snapshot::open(&path, 16).unwrap();
    let reads = Cell::new(0_usize);
    let mut bytes = [0_u8; 5];
    let read = snapshot.read_checked(&mut bytes, |event, value| {
        if matches!(event, SnapshotEvent::Read) {
            reads.set(reads.get() + 1);
            if reads.get() == 2 {
                return Err(Error::Input);
            }
        }
        value
    });
    let position = snapshot.file_mut().stream_position();
    let closed = snapshot.finish();
    drop(fixture);
    assert!(read.is_err());
    assert_eq!(reads.get(), 2);
    assert_eq!(&bytes, b"INERT");
    assert_eq!(position.unwrap(), 5);
    assert!(closed.is_ok());
}

#[test]
fn refused_seek_checkpoint_leaves_the_original_cursor_unchanged() {
    let fixture = Fixture::new();
    let path = fixture.0.join("input");
    fs::write(&path, b"INERT").unwrap();
    let mut snapshot = Snapshot::open(&path, 16).unwrap();
    let sought = snapshot.seek_checked(SeekFrom::End(0), |event, value| {
        if matches!(event, SnapshotEvent::Seek) {
            Err(Error::Input)
        } else {
            value
        }
    });
    let position = snapshot.file_mut().stream_position();
    let closed = snapshot.finish();
    drop(fixture);
    assert!(sought.is_err());
    assert_eq!(position.unwrap(), 0);
    assert!(closed.is_ok());
}
