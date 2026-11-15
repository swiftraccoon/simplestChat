//! The vendored directory's bounds, each at its edge. What the reader
//! makes of links, pipes and directories is held by the recorded
//! verifications.

use super::files;
use crate::{
    scratch::Scratch,
    vendor::{Bounds, Problem},
};

#[test]
fn a_vendored_directory_is_held_to_every_bound_at_its_edge() {
    const REVIEWED: Bounds = Bounds::REVIEWED;
    let scratch = Scratch::new("vendor-tree");
    let root = scratch.0.join("vendor");
    std::fs::create_dir_all(root.join("d/e")).expect("directories");
    for (path, content) in [("b", "bb"), ("a", "aa"), ("d/c", "c")] {
        std::fs::write(root.join(path), content).expect("a file");
    }
    let read = |bounds: Bounds| {
        let read = files(&root, &bounds);
        read.map(|files| files.into_keys().collect::<Vec<String>>())
    };
    let refused = |words: String| Err(Problem(words));
    let all = || Ok(["a", "b", "d/c"].map(str::to_owned).to_vec());
    assert_eq!(read(REVIEWED), all());
    // Three entries in the directory, one below, and one below that.
    let entries = 5;
    assert_eq!(
        read(Bounds {
            entries,
            ..REVIEWED
        }),
        all()
    );
    let entries = entries - 1;
    let many = refused("Too many vendor filesystem entries".to_owned());
    assert_eq!(
        read(Bounds {
            entries,
            ..REVIEWED
        }),
        many
    );
    let member = 2;
    assert_eq!(read(Bounds { member, ..REVIEWED }), all());
    let member = member - 1;
    let large = format!("Not a bounded regular file: {}/a", root.display());
    assert_eq!(read(Bounds { member, ..REVIEWED }), refused(large));
    let content = 5;
    assert_eq!(
        read(Bounds {
            content,
            ..REVIEWED
        }),
        all()
    );
    let content = content - 1;
    let budget = refused("Vendor content budget exceeded".to_owned());
    assert_eq!(
        read(Bounds {
            content,
            ..REVIEWED
        }),
        budget
    );
}

#[test]
fn a_file_that_grows_while_it_is_read_is_refused() {
    use std::{
        io::Write as _,
        sync::atomic::{AtomicBool, Ordering},
    };
    let scratch = Scratch::new("vendor-grows");
    let source = scratch.0.join("source");
    std::fs::write(&source, vec![b'x'; 4 * 1024 * 1024]).expect("a file");
    let (stop, started) = (AtomicBool::new(false), AtomicBool::new(false));
    let mut refusals = Vec::new();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut options = std::fs::OpenOptions::new();
            let mut growing = options.append(true).open(&source).expect("the file");
            while !stop.load(Ordering::SeqCst) {
                growing.write_all(b"y").expect("one more byte");
                started.store(true, Ordering::SeqCst);
            }
        });
        while !started.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        for _ in 0..20 {
            refusals.push(super::read_regular(&source, u64::MAX / 2).err());
        }
        stop.store(true, Ordering::SeqCst);
    });
    let words = format!("File changed or exceeded its limit: {}", source.display());
    let changed = Some(Problem(words));
    assert!(refusals.contains(&changed), "{refusals:?}");
    let alike = |refusal: &Option<Problem>| refusal.is_none() || *refusal == changed;
    assert!(refusals.iter().all(alike), "{refusals:?}");
}
