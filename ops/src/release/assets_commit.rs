//! Ordered index then pointer transaction. Rollback stops at pointer visibility;
//! every removal requires a witness retained by this same guarded operation.
use super::{
    Result, fail,
    files::{Dir, Entry, Fs, ScopedFs},
    model::{self, Capture, Generation},
    scope::Scope,
    store::{self, State},
};
use std::borrow::BorrowMut;

#[cfg(test)]
pub(super) fn nonce() -> Result<String> {
    nonce_scoped(&Scope::standalone())
}
pub(super) fn nonce_bytes(input: &mut impl std::io::Read, scope: &Scope<'_>) -> Result<[u8; 16]> {
    let mut bytes = [0u8; 16];
    let mut offset = 0;
    while offset < bytes.len() {
        scope.check()?;
        let read = match input.read(&mut bytes[offset..]) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            value => scope.observed_io(value)?,
        };
        if read == 0 {
            return scope.observed_io(Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)));
        }
        offset += read;
    }
    scope.result(Ok(bytes))
}
pub(super) fn nonce_scoped(scope: &Scope<'_>) -> Result<String> {
    let mut random = scope.admit_io(|| std::fs::File::open("/dev/urandom"))?;
    let read = nonce_bytes(&mut random, scope);
    scope.observed(&read);
    let closed = scope.closed(super::files::Access::default().close(random));
    let mut bytes = read?;
    closed?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    scope.result(Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect()))
}

pub(super) fn run(
    fs: &mut Fs,
    prior: State,
    selected: &[Generation],
    captures: &[(String, Capture)],
) -> Result<()> {
    run_scoped(
        &mut ScopedFs::new(fs, &Scope::standalone()),
        prior,
        selected,
        captures,
    )
}
pub(super) fn run_scoped<T: BorrowMut<Fs>>(
    fs: &mut ScopedFs<'_, '_, T>,
    prior: State,
    selected: &[Generation],
    captures: &[(String, Capture)],
) -> Result<()> {
    let scope = fs.scope();
    scope.check()?;
    let identities = model::identities(selected)?;
    let bytes = model::index(selected)?;
    if prior.original.as_deref() == Some(bytes.as_slice()) {
        return fs.verify();
    }
    let root = prior.root;
    let pending = fs.mkdir(root, &format!(".pending-{}", nonce_scoped(scope)?), 0o755)?;
    let mut added = Vec::<Dir>::new();
    let mut public_created = None;
    let mut index = None;
    let mut switched = false;
    let result = (|| {
        let mut sources = prior.directories.clone();
        for (revision, capture) in captures {
            if prior
                .generations
                .iter()
                .any(|entry| entry.revision == *revision)
            {
                continue;
            }
            let directory = fs.mkdir(pending, revision, 0o755)?;
            for (name, payload) in &capture.0 {
                fs.write(directory, name, payload, 0o644)?;
            }
            fs.sync(directory)?;
            // Track before rename: an uncertain post-rename failure must not
            // lose the only retained owner of a newly visible generation.
            added.push(directory);
            fs.rename_dir(directory, root, revision)?;
            sources.push((revision.clone(), directory));
        }
        let union_name = format!("union-{}", model::sha_scoped(&bytes, scope)?);
        let union = fs.mkdir(pending, &union_name, 0o755)?;
        for (name, revision, _) in &identities {
            let source = sources
                .iter()
                .find(|item| item.0 == *revision)
                .ok_or_else(|| fail("asset_operation_failed"))?
                .1;
            let entry = fs.metadata(source, name)?;
            fs.hardlink(entry, union, name)?;
        }
        fs.sync(union)?;
        added.push(union);
        fs.rename_dir(union, root, &union_name)?;
        let public = match prior.public {
            Some(public) => public,
            None => {
                let public = fs.mkdir(root, "public", 0o755)?;
                public_created = Some(public);
                public
            }
        };
        let pointer = fs.symlink(pending, "assets", &format!("../{union_name}"))?;
        let staged = fs.write(pending, "index.json", &bytes, 0o644)?;
        index = Some(staged);
        fs.rename_entry(staged, root, "index.json", prior.index)?;
        fs.rename_link(pointer, public, "assets", prior.pointer, &mut switched)?;
        fs.sync(public)?;
        fs.sync(root)
    })();
    // A rename that became visible is irreversible here even when its final
    // probe or fsync failed. Leave that evidence for a subsequent refusal.
    scope.observed(&result);
    let (rollback, pending_cleanup) = {
        let _recovery = scope.recovery_if(result.is_err());
        let rollback = if result.is_err() && !switched {
            rollback(fs, &prior, pending, index, &added, public_created)
        } else {
            Ok(())
        };
        scope.observed(&rollback);
        let pending_cleanup = fs.remove_dir(pending, 0);
        (rollback, pending_cleanup)
    };
    result.and(rollback).and(pending_cleanup)?;
    for (revision, directory) in &prior.directories {
        if !selected.iter().any(|entry| entry.revision == *revision) {
            fs.remove_dir(*directory, 0)?;
        }
    }
    if let Some(union) = prior.union {
        fs.remove_dir(union, 0)?;
    }
    fs.sync(root)?;
    // This also fences unexpected additions and validates every newly selected
    // generation, the exact original index bytes and the visible union pointer.
    store::read_scoped(fs, root)?;
    fs.verify()
}
fn rollback<T: BorrowMut<Fs>>(
    fs: &mut ScopedFs<'_, '_, T>,
    prior: &State,
    pending: Dir,
    index: Option<Entry>,
    added: &[Dir],
    public_created: Option<Dir>,
) -> Result<()> {
    let restored = match index.filter(|entry| fs.entry_at(*entry, prior.root, "index.json")) {
        Some(index) => match &prior.original {
            Some(previous) => {
                fs.write(pending, "restore.json", previous, 0o644)
                    .and_then(|restore| {
                        fs.rename_entry(restore, prior.root, "index.json", Some(index))
                    })
            }
            None => fs.unlink(index),
        },
        None => Ok(()),
    };
    let mut result = restored;
    for directory in added.iter().rev() {
        result = result.and(fs.remove_dir(*directory, 0));
    }
    if let Some(public) = public_created {
        result = result.and(fs.remove_dir(public, 0));
    }
    result
}
