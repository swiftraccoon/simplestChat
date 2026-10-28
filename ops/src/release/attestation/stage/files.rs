//! Stage-specific atomic records. Every newly created inode is journalled here
//! before the first metadata/read/write that can fail; failed evidence remains.
use super::super::{
    control::{self, Cause, Owner, Result, Window},
    files::{Dir, Retained},
};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    os::unix::fs::{FileExt, MetadataExt},
};

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    ImportJournalWrite,
    ValidateJournalWrite,
    ValidatedJournalWrite,
    FinalJournalWrite,
    ValidationNameWrite,
    StagedWrite,
    OutcomeWrite,
    StagedFileSync,
    StagedDirectorySync,
    OutcomeFileSync,
    OutcomeDirectorySync,
}
#[cfg(test)]
thread_local! {
    static CLOSES: std::cell::Cell<(usize,usize)> = const { std::cell::Cell::new((0,0)) };
}
#[cfg(test)]
pub(super) fn start_probe() {
    CLOSES.set((0, 0));
}
#[cfg(test)]
pub(super) fn take_probe() -> (usize, usize) {
    CLOSES.replace((0, 0))
}
struct Pending {
    file: Option<File>,
    parent: Dir,
    name: String,
    original: Option<Metadata>,
    digest: Option<String>,
}
#[derive(Default)]
pub(super) struct Records {
    files: Vec<Pending>,
    pub initial_journal: Option<Retained>,
    #[cfg(test)]
    pub fault: Option<Fault>,
}
fn checkpoint(owner: &mut Owner, final_work: bool) -> Result<()> {
    if final_work {
        owner.final_work()
    } else {
        owner.check(Window::Body)
    }
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
}
fn digest(file: &File, owner: &mut Owner, final_work: bool) -> Result<(u64, String)> {
    let mut bytes = vec![0; 65536];
    let mut offset = 0_u64;
    let mut hash = Sha256::new();
    loop {
        checkpoint(owner, final_work)?;
        let count = control::io(owner, file.read_at(&mut bytes, offset))?;
        checkpoint(owner, final_work)?;
        if count == 0 {
            break;
        }
        offset = offset
            .checked_add(count.try_into().map_err(|_| Cause::Bounds)?)
            .ok_or(Cause::Bounds)?;
        if offset > 2 * 1024 * 1024 {
            return control::failed(owner, Cause::Bounds);
        }
        hash.update(&bytes[..count]);
    }
    Ok((offset, super::super::files::hex(&hash.finalize())))
}
impl Pending {
    fn verify(&self, owner: &mut Owner, final_work: bool) -> Result<()> {
        checkpoint(owner, final_work)?;
        self.parent.final_namespace(owner)?;
        let file = self.file.as_ref().ok_or(Cause::Close)?;
        let current = control::io(owner, file.metadata())?;
        let stamp = self.original.as_ref().ok_or(Cause::Changed)?;
        if !binding(stamp, &current)
            || stamp.len() != current.len()
            || (
                stamp.mtime(),
                stamp.mtime_nsec(),
                stamp.ctime(),
                stamp.ctime_nsec(),
            ) != (
                current.mtime(),
                current.mtime_nsec(),
                current.ctime(),
                current.ctime_nsec(),
            )
        {
            return control::failed(owner, Cause::Changed);
        }
        let descriptor = rustix::fs::openat(
            self.parent.file(),
            self.name.as_str(),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| Cause::Io)?;
        let probe = File::from(descriptor);
        let metadata = probe.metadata().map_err(|_| Cause::Io);
        if metadata.is_err() {
            owner.record(Cause::Io);
        }
        let matches = metadata.as_ref().is_ok_and(|v| binding(stamp, v));
        if metadata.is_ok() && !matches {
            owner.record(Cause::Changed);
        }
        let closed = nix::unistd::close(probe).is_ok();
        if !closed {
            owner.record(Cause::Close);
        }
        metadata?;
        if !closed {
            return Err(Cause::Close);
        }
        if !matches {
            return Err(Cause::Changed);
        }
        let (count, actual) = digest(file, owner, final_work)?;
        if count != stamp.len() || self.digest.as_ref() != Some(&actual) {
            return control::failed(owner, Cause::Changed);
        }
        checkpoint(owner, final_work)
    }
    fn close(mut self, owner: &mut Owner) {
        if let Some(file) = self.file.take() {
            #[cfg(test)]
            CLOSES.with(|count| {
                let (closed, fallback) = count.get();
                count.set((closed + 1, fallback));
            });
            if nix::unistd::close(file).is_err() {
                owner.record(Cause::Close);
            }
        }
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        #[cfg(test)]
        if self.file.is_some() {
            CLOSES.with(|count| {
                let (closed, fallback) = count.get();
                count.set((closed, fallback + 1));
            });
        }
    }
}
impl Records {
    /// Compare candidate input identity with all still-owned atomic records,
    /// including a temporary generation not yet admitted for publication.
    pub fn input_aliases(&self, input: &File, owner: &mut Owner) -> Result<bool> {
        owner.check(Window::Body)?;
        let selected = control::io(owner, input.metadata())?;
        if let Some(original) = &self.initial_journal {
            let stamp = control::io(owner, original.file().metadata())?;
            if selected.dev() == stamp.dev() && selected.ino() == stamp.ino() {
                return Ok(true);
            }
        }
        for item in &self.files {
            owner.check(Window::Body)?;
            let stamp = control::io(owner, item.file.as_ref().ok_or(Cause::Close)?.metadata())?;
            if selected.dev() == stamp.dev() && selected.ino() == stamp.ino() {
                return Ok(true);
            }
        }
        owner.check(Window::Body)?;
        Ok(false)
    }
    /// Require the latest owned generation before an unlocked interval may
    /// resume its original transaction. Equal replacement bytes are not enough.
    pub fn check_owned(&self, parent: &Dir, name: &str, owner: &mut Owner) -> Result<()> {
        let mut matching = self
            .files
            .iter()
            .filter(|item| item.parent.path() == parent.path() && item.name == name);
        let item = matching.next().ok_or(Cause::Changed)?;
        if matching.next().is_some() {
            return control::failed(owner, Cause::Changed);
        }
        item.verify(owner, false)
    }
    /// Handoff boundaries retain the complete owned record inventory, including
    /// a freshly published staged marker that is not an original input file.
    pub fn check_all(&self, owner: &mut Owner) -> Result<()> {
        self.check_guard(owner)?;
        for item in &self.files {
            item.verify(owner, false)?;
        }
        owner.check(Window::Body)
    }
    #[cfg(test)]
    fn fault(&mut self, name: &str, bytes: &[u8], boundary: u8) -> bool {
        if self.fault.is_none() {
            return false;
        }
        let phase = serde_json::from_slice::<serde_json::Value>(bytes)
            .ok()
            .and_then(|v| v.get("phase").and_then(|p| p.as_str()).map(str::to_owned));
        let hit = match self.fault {
            Some(Fault::ImportJournalWrite) => {
                boundary == 0
                    && name == "release-state.json"
                    && phase.as_deref() == Some("import_image")
            }
            Some(Fault::ValidateJournalWrite) => {
                boundary == 0
                    && name == "release-state.json"
                    && phase.as_deref() == Some("validate_image")
            }
            Some(Fault::ValidatedJournalWrite) => {
                boundary == 0
                    && name == "release-state.json"
                    && phase.as_deref() == Some("image_validated")
            }
            Some(Fault::FinalJournalWrite) => {
                boundary == 0 && name == "release-state.json" && phase.as_deref() == Some("staged")
            }
            Some(Fault::ValidationNameWrite) => boundary == 0 && name == "validation-name.txt",
            Some(Fault::StagedWrite) => boundary == 0 && name == "staged.json",
            Some(Fault::OutcomeWrite) => boundary == 0 && name == "outcome.json",
            Some(Fault::StagedFileSync) => boundary == 1 && name == "staged.json",
            Some(Fault::StagedDirectorySync) => boundary == 2 && name == "staged.json",
            Some(Fault::OutcomeFileSync) => boundary == 1 && name == "outcome.json",
            Some(Fault::OutcomeDirectorySync) => boundary == 2 && name == "outcome.json",
            None => false,
        };
        if hit {
            self.fault = None;
        }
        hit
    }
    pub fn write(
        &mut self,
        parent: &Dir,
        name: &str,
        bytes: &[u8],
        owner: &mut Owner,
        final_work: bool,
    ) -> Result<()> {
        let numbered = name.len() == 10
            && name.as_bytes()[..3].iter().all(u8::is_ascii_digit)
            && (name.ends_with(".stdout") || name.ends_with(".stderr"));
        if ![
            "release-state.json",
            "staged.json",
            "outcome.json",
            "validation-name.txt",
        ]
        .contains(&name)
            && !numbered
        {
            return control::failed(owner, Cause::Input);
        }
        if bytes.len() > 2 * 1024 * 1024 || self.files.len() >= 64 {
            return control::failed(owner, Cause::Bounds);
        }
        checkpoint(owner, final_work)?;
        parent.final_namespace(owner)?;
        #[cfg(test)]
        if self.fault(name, bytes, 0) {
            return control::failed(owner, Cause::Io);
        }
        // Keep the previous owned generation alive through temporary writes;
        // its final namespace check occurs immediately before our replacement.
        let previous_index = self
            .files
            .iter()
            .position(|v| v.parent.path() == parent.path() && v.name == name);
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| Cause::Io)?;
        checkpoint(owner, final_work)?;
        let temporary = format!(".release-{}", super::super::files::hex(&nonce));
        let descriptor = rustix::fs::openat(
            parent.file(),
            temporary.as_str(),
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_bits_truncate(0o600),
        )
        .map_err(|_| Cause::Io)?;
        self.files.push(Pending {
            file: Some(File::from(descriptor)),
            parent: parent.clone(),
            name: temporary,
            original: None,
            digest: None,
        });
        let mut previous = previous_index.map(|index| self.files.remove(index));
        let mut prior = if name == "release-state.json" {
            self.initial_journal.take()
        } else {
            None
        };
        let index = self.files.len() - 1;
        #[cfg(test)]
        let fail_file_sync = self.fault(name, bytes, 1);
        #[cfg(test)]
        let fail_directory_sync = self.fault(name, bytes, 2);
        let result = (|| {
            if previous.is_none()
                && prior.is_none()
                && !final_work
                && parent.exists(name.as_ref(), owner)?
            {
                prior = Some(Retained::open(parent.clone(), name, owner)?);
            } else if final_work && previous.is_none() && prior.is_none() {
                match rustix::fs::statat(parent.file(), name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                {
                    Ok(_) => return control::failed(owner, Cause::Changed),
                    Err(rustix::io::Errno::NOENT) => {}
                    Err(_) => return control::failed(owner, Cause::Io),
                }
            }
            let item = &mut self.files[index];
            let file = item.file.as_ref().ok_or(Cause::Close)?;
            rustix::fs::fchmod(file, rustix::fs::Mode::from_bits_truncate(0o600))
                .map_err(|_| Cause::Io)?;
            let first = control::io(owner, file.metadata())?;
            if !first.is_file()
                || first.mode() & 0o7777 != 0o600
                || first.nlink() != 1
                || first.uid() != rustix::process::getuid().as_raw()
                || first.len() != 0
            {
                return control::failed(owner, Cause::Changed);
            }
            let mut offset = 0;
            while offset < bytes.len() {
                checkpoint(owner, final_work)?;
                let count = control::io(owner, file.write_at(&bytes[offset..], offset as u64))?;
                checkpoint(owner, final_work)?;
                if count == 0 {
                    return control::failed(owner, Cause::Io);
                }
                offset += count;
            }
            #[cfg(test)]
            if fail_file_sync {
                return control::failed(owner, Cause::Io);
            }
            control::io(owner, file.sync_all())?;
            checkpoint(owner, final_work)?;
            let grown = control::io(owner, file.metadata())?;
            if !binding(&first, &grown) || grown.len() != bytes.len() as u64 {
                return control::failed(owner, Cause::Changed);
            }
            item.original = Some(grown);
            item.digest = Some(super::super::files::hex(&Sha256::digest(bytes)));
            item.verify(owner, final_work)?;
            if let Some(original) = previous.take() {
                let valid = original.verify(owner, final_work);
                original.close(owner);
                valid?;
                checkpoint(owner, final_work)?;
            }
            if let Some(original) = prior.take() {
                let valid = original.check(owner, Window::Body);
                original.finish(owner);
                valid?;
                checkpoint(owner, final_work)?;
            }
            rustix::fs::renameat(parent.file(), item.name.as_str(), parent.file(), name)
                .map_err(|_| Cause::Io)?;
            // Record our successful namespace effect before any later fallible
            // metadata observation. Close still owns the original on refusal.
            item.name = name.into();
            let renamed = control::io(owner, file.metadata())?;
            if !binding(item.original.as_ref().ok_or(Cause::Changed)?, &renamed) {
                return control::failed(owner, Cause::Changed);
            }
            item.original = Some(renamed);
            item.verify(owner, final_work)?;
            #[cfg(test)]
            if fail_directory_sync {
                return control::failed(owner, Cause::Io);
            }
            control::io(owner, parent.file().sync_all())?;
            checkpoint(owner, final_work)
        })();
        if let Err(cause) = result {
            owner.record(cause);
        }
        if let Some(original) = prior.take() {
            original.finish(owner);
        }
        if let Some(original) = previous.take() {
            original.close(owner);
        }
        result
    }
    pub fn check_guard(&self, owner: &mut Owner) -> Result<()> {
        if let Some(original) = &self.initial_journal {
            original.check(owner, Window::Body)?;
        }
        Ok(())
    }
    pub fn finish(mut self, owner: &mut Owner) {
        if let Some(original) = self.initial_journal.take() {
            original.finish(owner);
        }
        for item in self.files.drain(..) {
            if owner.error().is_none() {
                let _ = item
                    .verify(owner, true)
                    .map_err(|cause| owner.record(cause));
            }
            item.close(owner);
        }
    }
}
