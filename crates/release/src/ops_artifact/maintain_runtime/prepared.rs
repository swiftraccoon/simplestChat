//! Fixed cold-boot handoff of one fully settled VM's original files.

use super::*;
use sha2::Sha256;

const INPUTS: [(&str, u64); 9] = [
    ("base.qcow2", IMAGE_MAX),
    ("client", 16384),
    ("client.pub", 16384),
    ("host", 16384),
    ("host.pub", 16384),
    ("user-data", 16384),
    ("meta-data", 16384),
    ("known_hosts", 16384),
    ("seed.img", 16 * 1024 * 1024),
];

fn binding(a: &Metadata, b: &Metadata) -> bool {
    (a.dev(), a.ino(), a.mode(), a.uid(), a.gid(), a.nlink())
        == (b.dev(), b.ino(), b.mode(), b.uid(), b.gid(), b.nlink())
}

fn digest(file: &File, path: &Path, control: &mut Control) -> Result<(Metadata, String)> {
    control.check()?;
    let before = file.metadata().map_err(|_| Error::Io)?;
    if !before.is_file()
        || before.nlink() != 1
        || before.len() == 0
        || before.len() > 17 * GIB
        || before.uid() != rustix::process::getuid().as_raw()
    {
        return Err(Error::Changed);
    }
    let mut hash = Sha256::new();
    let mut offset = 0u64;
    let mut block = [0; 65536];
    loop {
        control.check()?;
        let count = match file.read_at(&mut block, offset) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            value => value.map_err(|_| Error::Io)?,
        };
        control.check()?;
        if count == 0 {
            break;
        }
        offset = offset.checked_add(count as u64).ok_or(Error::Refused)?;
        if offset > before.len() {
            return Err(Error::Changed);
        }
        hash.update(&block[..count]);
    }
    let after = file.metadata().map_err(|_| Error::Io)?;
    let visible = fs::symlink_metadata(path).map_err(|_| Error::Changed)?;
    if offset != before.len() || !files::same(&before, &after) || !files::same(&before, &visible) {
        return Err(Error::Changed);
    }
    control.check()?;
    Ok((before, files::hex(&hash.finalize())))
}

struct Input {
    before: Metadata,
    sha256: String,
    bytes: u64,
}

pub(super) struct Prepared {
    root: Option<File>,
    path: PathBuf,
    before: Metadata,
    port: u16,
    run_id: String,
    end: Instant,
    inputs: Vec<Input>,
    overlay: Metadata,
    sealed: Option<(Metadata, String)>,
}

impl Prepared {
    pub(super) fn capture(vm: &Vm, control: &mut Control) -> Result<Self> {
        control.check()?;
        if vm.originals.len() != INPUTS.len()
            || vm.restricted
            || vm.end != control.end()
            || vm
                .work
                .join("qmp-restricted.sock")
                .as_os_str()
                .as_encoded_bytes()
                .len()
                > 100
        {
            return Err(Error::Refused);
        }
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NONBLOCK)
                    .bits() as i32,
            )
            .open(&vm.work)
            .map_err(|_| Error::Io)?;
        let admitted = (|| {
            let before = root.metadata().map_err(|_| Error::Io)?;
            let mut inputs = Vec::new();
            for (name, _) in INPUTS {
                let original = vm
                    .originals
                    .iter()
                    .find(|input| input.path == vm.work.join(name))
                    .ok_or(Error::Changed)?;
                original.check(control)?;
                inputs.push(Input {
                    before: original.file()?.metadata().map_err(|_| Error::Io)?,
                    sha256: original.sha256.clone(),
                    bytes: original.bytes,
                });
            }
            let disk = vm.overlay.as_ref().ok_or(Error::Changed)?;
            disk.check(control)?;
            let overlay = disk
                .file
                .as_ref()
                .ok_or(Error::Close)?
                .metadata()
                .map_err(|_| Error::Io)?;
            Ok((before, inputs, overlay))
        })();
        let (before, inputs, overlay) = match admitted {
            Ok(value) => value,
            Err(error) => {
                control.record(error);
                let _ = files::close(root, control);
                return Err(error);
            }
        };
        let mut value = Self {
            root: Some(root),
            path: vm.work.clone(),
            before,
            port: vm.port,
            run_id: vm.run_id.clone(),
            end: vm.end,
            inputs,
            overlay,
            sealed: None,
        };
        if let Err(error) = value.namespace(control) {
            control.record(error);
            let _ = value.finish(control);
            return Err(error);
        }
        Ok(value)
    }
    fn namespace(&self, control: &mut Control) -> Result<()> {
        control.check()?;
        let visible = fs::symlink_metadata(&self.path).map_err(|_| Error::Changed)?;
        if !visible.is_dir()
            || visible.mode() & 0o7777 != 0o700
            || visible.uid() != rustix::process::getuid().as_raw()
            || !binding(&self.before, &visible)
            || fs::canonicalize(&self.path).map_err(|_| Error::Changed)? != self.path
        {
            return Err(Error::Changed);
        }
        if let Some(root) = &self.root
            && !binding(&self.before, &root.metadata().map_err(|_| Error::Io)?)
        {
            return Err(Error::Changed);
        }
        control.check()
    }
    fn identity(&self, vm: &Vm, control: &mut Control) -> Result<()> {
        self.namespace(control)?;
        if vm.work != self.path
            || vm.port != self.port
            || vm.run_id != self.run_id
            || vm.end != self.end
            || vm.end != control.end()
        {
            return Err(Error::Changed);
        }
        Ok(())
    }
    pub(super) fn seal(&mut self, vm: &Vm, control: &mut Control) -> Result<()> {
        self.identity(vm, control)?;
        if self.sealed.is_some() || vm.originals.len() != INPUTS.len() {
            return Err(Error::Changed);
        }
        for ((name, _), expected) in INPUTS.iter().zip(&self.inputs) {
            let original = vm
                .originals
                .iter()
                .find(|input| input.path == self.path.join(name))
                .ok_or(Error::Changed)?;
            original.check(control)?;
            if !files::same(
                &expected.before,
                &original.file()?.metadata().map_err(|_| Error::Io)?,
            ) || original.sha256 != expected.sha256
                || original.bytes != expected.bytes
            {
                return Err(Error::Changed);
            }
        }
        let disk = vm.overlay.as_ref().ok_or(Error::Changed)?;
        disk.check(control)?;
        let file = disk.file.as_ref().ok_or(Error::Close)?;
        if !binding(&self.overlay, &file.metadata().map_err(|_| Error::Io)?) {
            return Err(Error::Changed);
        }
        self.sealed = Some(digest(file, &disk.path, control)?);
        self.namespace(control)
    }
    pub(super) fn admit(&mut self, vm: &mut Vm, control: &mut Control) -> Result<()> {
        self.identity(vm, control)?;
        if !vm.restricted
            || !vm.originals.is_empty()
            || vm.overlay.is_some()
            || self.sealed.is_none()
        {
            return Err(Error::Refused);
        }
        match fs::symlink_metadata(self.path.join("qmp-restricted.sock")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(Error::Changed),
        }
        for ((name, maximum), expected) in INPUTS.iter().zip(&self.inputs) {
            vm.hold(name, *maximum, control)?;
            let original = vm.originals.last().ok_or(Error::Changed)?;
            if !files::same(
                &expected.before,
                &original.file()?.metadata().map_err(|_| Error::Io)?,
            ) || original.sha256 != expected.sha256
                || original.bytes != expected.bytes
            {
                return Err(Error::Changed);
            }
        }
        vm.overlay = Some(MutableDisk::open(&self.path.join("guest.qcow2"), control)?);
        self.verify_ready(vm, control)
    }
    pub(super) fn verify_ready(&self, vm: &Vm, control: &mut Control) -> Result<()> {
        self.identity(vm, control)?;
        let expected = self.sealed.as_ref().ok_or(Error::Refused)?;
        let disk = vm.overlay.as_ref().ok_or(Error::Changed)?;
        let (stamp, sha256) = digest(disk.file.as_ref().ok_or(Error::Close)?, &disk.path, control)?;
        if !files::same(&expected.0, &stamp) || expected.1 != sha256 {
            return Err(Error::Changed);
        }
        self.namespace(control)
    }
    pub(super) fn finish(&mut self, control: &mut Control) -> Result<()> {
        let checked = self.namespace(control);
        if let Err(error) = checked {
            control.record(error);
        }
        let closed = self
            .root
            .take()
            .map(|root| files::close(root, control))
            .unwrap_or(Ok(()));
        checked.and(closed).and(control.check())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    struct Fixture {
        vm: Vm,
        control: Control,
        roots: Vec<PathBuf>,
    }
    impl Fixture {
        fn new() -> Self {
            let root = Path::new("/tmp").canonicalize().unwrap().join(format!(
                "maintain-prepared-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst),
            ));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let mut control = Control::new(Instant::now()).unwrap();
            let mut vm = Vm::epoch(root.clone(), 2222, control.end(), "a".repeat(32), false);
            for (name, maximum) in INPUTS {
                fs::write(root.join(name), format!("inert original {name}")).unwrap();
                fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o600)).unwrap();
                vm.hold(name, maximum, &mut control).unwrap();
            }
            fs::write(root.join("guest.qcow2"), b"inert mutable disk").unwrap();
            fs::set_permissions(root.join("guest.qcow2"), fs::Permissions::from_mode(0o600))
                .unwrap();
            vm.overlay = Some(MutableDisk::open(&root.join("guest.qcow2"), &mut control).unwrap());
            Self {
                vm,
                control,
                roots: vec![root],
            }
        }
        fn sealed(&mut self) -> Prepared {
            let mut prepared = Prepared::capture(&self.vm, &mut self.control).unwrap();
            // The first writer may change the same overlay before settlement.
            fs::write(self.vm.work.join("guest.qcow2"), b"settled inert disk").unwrap();
            prepared.seal(&self.vm, &mut self.control).unwrap();
            for original in &mut self.vm.originals {
                original.finish(&mut self.control).unwrap();
            }
            self.vm
                .overlay
                .as_mut()
                .unwrap()
                .finish(&mut self.control)
                .unwrap();
            self.vm = Vm::epoch(
                self.vm.work.clone(),
                self.vm.port,
                self.vm.end,
                self.vm.run_id.clone(),
                true,
            );
            prepared
        }
        fn finish(mut self, mut prepared: Prepared) {
            self.control.settle();
            let _ = prepared.finish(&mut self.control);
            for original in &mut self.vm.originals {
                let _ = original.finish(&mut self.control);
            }
            if let Some(disk) = &mut self.vm.overlay {
                disk.finish(&mut self.control).unwrap();
            }
            assert!(prepared.root.is_none());
            assert!(
                self.vm
                    .overlay
                    .as_ref()
                    .is_none_or(|disk| disk.file.is_none())
            );
            assert!(!self.control.cleanup_failed);
            let _ = self.control.finish();
            for root in self.roots {
                fs::remove_dir_all(root).unwrap();
            }
        }
    }

    #[test]
    fn cold_handoff_reopens_exact_originals_and_sealed_disk_without_new_clock() {
        let mut fixture = Fixture::new();
        let start = fixture.control.start;
        let end = fixture.control.end();
        let mut prepared = fixture.sealed();
        prepared
            .admit(&mut fixture.vm, &mut fixture.control)
            .unwrap();
        assert_eq!(fixture.vm.originals.len(), INPUTS.len());
        prepared.finish(&mut fixture.control).unwrap();
        prepared
            .verify_ready(&fixture.vm, &mut fixture.control)
            .unwrap();
        assert_eq!(
            (fixture.control.start, fixture.control.end(), fixture.vm.end),
            (start, end, end)
        );
        // The last frozen witness remains effective after its namespace FD closes.
        fs::write(fixture.vm.work.join("guest.qcow2"), b"tampered inert dis").unwrap();
        assert_eq!(
            prepared.verify_ready(&fixture.vm, &mut fixture.control),
            Err(Error::Changed)
        );
        fixture.finish(prepared);
    }

    #[test]
    fn cold_handoff_refuses_replaced_overlay_immutable_input_and_foreign_socket() {
        for role in ["guest.qcow2", "client", "seed.img", "qmp-restricted.sock"] {
            let mut fixture = Fixture::new();
            let mut prepared = fixture.sealed();
            let path = fixture.vm.work.join(role);
            if role != "qmp-restricted.sock" {
                let bytes = fs::read(&path).unwrap();
                fs::remove_file(&path).unwrap();
                fs::write(&path, bytes).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            } else {
                fs::write(&path, b"foreign entry").unwrap();
            }
            assert!(
                prepared
                    .admit(&mut fixture.vm, &mut fixture.control)
                    .is_err(),
                "{role}"
            );
            fixture.finish(prepared);
        }
    }

    #[test]
    fn cold_handoff_refuses_wrong_epoch_identity_unsealed_state_and_replaced_namespace() {
        let mut fixture = Fixture::new();
        let mut prepared = Prepared::capture(&fixture.vm, &mut fixture.control).unwrap();
        assert_eq!(
            prepared.admit(&mut fixture.vm, &mut fixture.control),
            Err(Error::Refused)
        );
        prepared.finish(&mut fixture.control).unwrap();
        fixture.finish(prepared);
        for changed in 0..4 {
            let mut fixture = Fixture::new();
            let mut prepared = fixture.sealed();
            match changed {
                0 => fixture.vm.port += 1,
                1 => fixture.vm.run_id = "b".repeat(32),
                2 => fixture.vm.end += Duration::from_secs(1),
                _ => {
                    let moved = fixture.vm.work.with_extension("moved");
                    fs::rename(&fixture.vm.work, &moved).unwrap();
                    fixture.roots.push(moved);
                    fs::create_dir(&fixture.vm.work).unwrap();
                    fs::set_permissions(&fixture.vm.work, fs::Permissions::from_mode(0o700))
                        .unwrap();
                }
            }
            assert!(
                prepared
                    .admit(&mut fixture.vm, &mut fixture.control)
                    .is_err()
            );
            fixture.finish(prepared);
        }
    }
}
