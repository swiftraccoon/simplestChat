//! Actual private work growth and foreign namespace refusal; no VM or commands.

use super::*;
use std::{
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Temporary(PathBuf);
impl Temporary {
    fn new() -> io::Result<Self> {
        let root = std::env::temp_dir().canonicalize()?.join(format!(
            "maintain-work-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        Ok(Self(root))
    }
    fn finish(self) -> io::Result<()> {
        fs::remove_dir_all(self.0)
    }
}
fn directory(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|_| Error::Io)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| Error::Io)
}
fn write(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| Error::Io)?;
    let body = file
        .write_all(data)
        .and_then(|()| file.flush())
        .map_err(|_| Error::Io);
    let closed = nix::unistd::close(file).map_err(|_| Error::Close);
    body.and(closed)
}
fn finish(work: &mut Option<Work>, control: &mut Control) -> (Result<()>, bool) {
    control.settle();
    let finished = work
        .as_mut()
        .map(|work| work.finish(control))
        .unwrap_or(Err(Error::Input));
    let empty = work
        .as_ref()
        .is_some_and(|work| work.parent.is_none() && work.child.is_none());
    if let Err(error) = finished {
        control.record(error);
    }
    (finished, empty)
}

#[test]
fn latched_body_io_does_not_fabricate_a_work_retirement_failure() {
    let temporary = Temporary::new().unwrap();
    let mut control = Control::new(Instant::now()).unwrap();
    let mut work = None;
    let body: Result<()> = (|| {
        work = Some(Work::create(&temporary.0, &mut control)?);
        write(
            &work.as_ref().ok_or(Error::Input)?.path.join("client"),
            b"OWNED_GENERATED_CLIENT",
        )
    })();
    if let Err(error) = body {
        control.record(error);
    }
    control.record(Error::Io);
    let original = (
        control.start,
        control.end(),
        control.whole_body_end(),
        control.body_end(),
    );
    // This is deliberately called before settle: known namespace retirement
    // may ignore the latched cause, but cannot borrow the final reserve.
    let retired = work
        .as_mut()
        .map(|work| work.finish(&mut control))
        .unwrap_or(Err(Error::Input));
    let slots_empty = work
        .as_ref()
        .is_some_and(|work| work.parent.is_none() && work.child.is_none());
    let absent = fs::symlink_metadata(temporary.0.join("work"))
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
    let observed = (
        control.start,
        control.end(),
        control.whole_body_end(),
        control.body_end(),
    );
    let primary = control.first;
    let setup = control.setup;
    let settling = control.settling;
    let cleanup_failed = control.cleanup_failed;
    control.settle();
    let finished = control.finish();
    let removed = temporary.finish();
    assert_eq!(body, Ok(()));
    assert_eq!(retired, Ok(()));
    assert!(slots_empty && absent);
    assert_eq!(original, observed);
    assert!(setup && !settling && !cleanup_failed);
    assert_eq!(primary, Some(Error::Io));
    assert_eq!(finished, Err(Error::Io));
    assert!(removed.is_ok());
}

#[test]
fn authorised_work_growth_retires_originals_without_following_tool_links() {
    let temporary = Temporary::new().unwrap();
    let mut control = Control::new(Instant::now()).unwrap();
    let mut work = None;
    let outside = temporary.0.join("outside-tool");
    let body: Result<()> = (|| {
        write(&outside, b"OWNED_OUTSIDE_TARGET")?;
        work = Some(Work::create(&temporary.0, &mut control)?);
        let owned = work.as_ref().ok_or(Error::Input)?;
        owned.check(&mut control)?;
        write(&owned.path.join("client"), b"private generated key fixture")?;
        write(&owned.path.join("base.qcow2"), b"inert image fixture")?;
        directory(&owned.path.join("native-tools"))?;
        symlink(&outside, owned.path.join("native-tools/gh")).map_err(|_| Error::Io)?;
        // The same original child accepts its own flat entries and one known
        // nested tool directory without treating changing nlink as replacement.
        owned.check(&mut control)
    })();
    if let Err(error) = body {
        control.record(error);
    }
    let (retired, slots_empty) = finish(&mut work, &mut control);
    let absent = fs::symlink_metadata(temporary.0.join("work"))
        .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
    let target = fs::read(&outside);
    let finished = control.finish();
    // Test-created outside data is measured before the test's separate owned
    // temporary tree cleanup, never attributed to Work's retirement.
    let removed = temporary.finish();
    assert_eq!(body, Ok(()));
    assert_eq!(retired, Ok(()));
    assert!(slots_empty && absent);
    assert_eq!(target.unwrap(), b"OWNED_OUTSIDE_TARGET");
    assert_eq!(finished, Ok(()));
    assert!(removed.is_ok());
}

#[derive(Clone, Copy, Debug)]
enum Substitution {
    ChildDirectory,
    ChildSymlink,
    ParentDirectory,
}

#[test]
fn substituted_namespace_refuses_and_consumes_both_original_owner_slots() {
    for substitution in [
        Substitution::ChildDirectory,
        Substitution::ChildSymlink,
        Substitution::ParentDirectory,
    ] {
        substituted_namespace_case(substitution);
    }
}

fn substituted_namespace_case(substitution: Substitution) {
    let temporary = Temporary::new().unwrap();
    let mut control = Control::new(Instant::now()).unwrap();
    let mut work = None;
    let output = temporary.0.join("output");
    let moved = temporary.0.join("retained-original");
    let foreign = temporary.0.join("foreign");
    let body: Result<()> = (|| {
        directory(&output)?;
        directory(&foreign)?;
        write(&foreign.join("marker"), b"FOREIGN_NAMESPACE")?;
        work = Some(Work::create(&output, &mut control)?);
        let owned = work.as_ref().ok_or(Error::Input)?;
        write(&owned.path.join("client"), b"ORIGINAL_NAMESPACE")?;
        match substitution {
            Substitution::ChildDirectory => {
                fs::rename(&owned.path, &moved).map_err(|_| Error::Io)?;
                fs::rename(&foreign, &owned.path).map_err(|_| Error::Io)?;
            }
            Substitution::ChildSymlink => {
                fs::rename(&owned.path, &moved).map_err(|_| Error::Io)?;
                symlink(&foreign, &owned.path).map_err(|_| Error::Io)?;
            }
            Substitution::ParentDirectory => {
                fs::rename(&output, &moved).map_err(|_| Error::Io)?;
                directory(&output)?;
                fs::rename(&foreign, output.join("work")).map_err(|_| Error::Io)?;
            }
        }
        owned.check(&mut control)
    })();
    if let Err(error) = body {
        control.record(error);
    }
    let (retired, slots_empty) = finish(&mut work, &mut control);
    let foreign_marker = match substitution {
        Substitution::ChildSymlink => foreign.join("marker"),
        _ => output.join("work/marker"),
    };
    let original_marker = match substitution {
        Substitution::ParentDirectory => moved.join("work/client"),
        _ => moved.join("client"),
    };
    let foreign_bytes = fs::read(&foreign_marker);
    let original_bytes = fs::read(&original_marker);
    let replacement = fs::symlink_metadata(output.join("work"));
    let finished = control.finish();
    let removed = temporary.finish();
    assert_eq!(body, Err(Error::Changed), "{substitution:?}");
    assert_eq!(retired, Err(Error::Changed), "{substitution:?}");
    assert!(slots_empty, "{substitution:?}");
    assert_eq!(foreign_bytes.unwrap(), b"FOREIGN_NAMESPACE");
    assert_eq!(original_bytes.unwrap(), b"ORIGINAL_NAMESPACE");
    let metadata = replacement.unwrap();
    assert_eq!(
        metadata.file_type().is_symlink(),
        matches!(substitution, Substitution::ChildSymlink)
    );
    assert_eq!(finished, Err(Error::Changed));
    assert!(removed.is_ok());
}

#[test]
fn unreviewed_nested_directory_is_retained_instead_of_recursively_deleted() {
    for nested_tools in [false, true] {
        unreviewed_nested_directory_case(nested_tools);
    }
}

fn unreviewed_nested_directory_case(nested_tools: bool) {
    let temporary = Temporary::new().unwrap();
    let mut control = Control::new(Instant::now()).unwrap();
    let mut work = None;
    let unreviewed = if nested_tools {
        temporary.0.join("work/native-tools/unreviewed")
    } else {
        temporary.0.join("work/unreviewed")
    };
    let body: Result<()> = (|| {
        work = Some(Work::create(&temporary.0, &mut control)?);
        if nested_tools {
            directory(&temporary.0.join("work/native-tools"))?;
        }
        directory(&unreviewed)?;
        write(&unreviewed.join("marker"), b"UNREVIEWED_NAMESPACE")?;
        work.as_ref().ok_or(Error::Input)?.check(&mut control)
    })();
    if let Err(error) = body {
        control.record(error);
    }
    let (retired, slots_empty) = finish(&mut work, &mut control);
    let marker = fs::read(unreviewed.join("marker"));
    let finished = control.finish();
    let removed = temporary.finish();
    assert_eq!(body, Ok(()));
    assert_eq!(retired, Err(Error::Refused));
    assert!(slots_empty);
    assert_eq!(marker.unwrap(), b"UNREVIEWED_NAMESPACE");
    assert_eq!(finished, Err(Error::Refused));
    assert!(removed.is_ok());
}
