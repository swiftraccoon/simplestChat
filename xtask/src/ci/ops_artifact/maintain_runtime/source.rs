//! Clean exact-head source bundle and fixed three-member distribution transport.

use super::{
    Error, Result,
    auth::{self, AuthenticatedOps},
    control::Control,
    files::{self, Original},
    options::Options,
    process::Runner,
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{self, Read, Seek, Write},
    os::unix::fs::{FileExt, MetadataExt},
    time::Duration,
};

pub(super) struct Bundle {
    pub original: Original,
    pub tree: String,
}
pub(super) async fn bundle(
    runner: &mut Runner,
    control: &mut Control,
    options: &Options,
) -> Result<Bundle> {
    let path = runner.output.join("maintain-source.bundle");
    let mut tree = String::new();
    for boundary in ["before", "after"] {
        for (role, suffix) in [
            ("head", vec!["rev-parse", "HEAD"]),
            ("tree", vec!["rev-parse", "HEAD^{tree}"]),
            (
                "clean",
                vec!["status", "--porcelain", "--untracked-files=all"],
            ),
        ] {
            let mut argv = vec![OsString::from("-C"), runner.root.as_os_str().into()];
            argv.extend(suffix.iter().map(OsString::from));
            let observed = runner
                .run(
                    control,
                    &format!("source-{role}-{boundary}"),
                    "git",
                    argv,
                    Duration::from_secs(30),
                    &[0],
                )
                .await?;
            match role {
                "head" if observed.stdout != format!("{}\n", options.revision).as_bytes() => {
                    return Err(Error::Changed);
                }
                "tree" => {
                    let value = std::str::from_utf8(&observed.stdout).map_err(|_| Error::Input)?;
                    let value = value.strip_suffix('\n').ok_or(Error::Refused)?;
                    if !super::options::digest(value, 40) || (!tree.is_empty() && tree != value) {
                        return Err(Error::Changed);
                    }
                    tree = value.into();
                }
                "clean" if !observed.stdout.is_empty() => return Err(Error::Changed),
                _ => {}
            }
        }
        runner.integrity(control)?;
        if boundary == "before" {
            if path.symlink_metadata().is_ok() {
                return Err(Error::Refused);
            }
            let argv = vec![
                "-C".into(),
                runner.root.as_os_str().into(),
                "bundle".into(),
                "create".into(),
                path.as_os_str().into(),
                "HEAD".into(),
            ];
            runner
                .run(
                    control,
                    "source-bundle",
                    "git",
                    argv,
                    Duration::from_secs(120),
                    &[0],
                )
                .await?;
        }
    }
    let mut original = Original::open(&path, 320 * 1024 * 1024, control)?;
    let admitted = (|| {
        input_properties(original.file()?, false)?;
        original.check(control)
    })();
    if let Err(error) = admitted {
        control.record(error);
        let _ = original.finish(control);
        return Err(error);
    }
    Ok(Bundle { original, tree })
}

pub(super) struct Transfer {
    pub original: Original,
    pub argv: Vec<OsString>,
}
struct Bytes<'a> {
    original: &'a Original,
    control: &'a mut Control,
    at: u64,
}
impl Read for Bytes<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let failed = || io::Error::other("retained transport refused");
        if let Err(error) = self
            .control
            .check()
            .and_then(|()| self.original.metadata(self.control))
        {
            self.control.record(error);
            return Err(failed());
        }
        let length = bytes.len().min(65536);
        let count = match self
            .original
            .file()
            .map_err(|_| failed())?
            .read_at(&mut bytes[..length], self.at)
        {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
            Err(error) => {
                self.control.record(Error::Io);
                return Err(error);
            }
        };
        self.at += count as u64;
        if let Err(error) = self
            .original
            .metadata(self.control)
            .and_then(|()| self.control.check())
        {
            self.control.record(error);
            return Err(failed());
        }
        Ok(count)
    }
}
pub(super) async fn transfer(
    runner: &mut Runner,
    control: &mut Control,
    ops: &AuthenticatedOps,
    options: &Options,
    run_id: &str,
) -> Result<Transfer> {
    ops.check(control)?;
    let selection_path = runner.output.join("ops-selection.json");
    let selected = serde_json::to_vec(&ops.selection).map_err(|_| Error::Input)?;
    files::write(&selection_path, &selected, control)?;
    let selection = Original::open(&selection_path, 16384, control)?;
    runner
        .sources
        .insert("generated/ops-selection.json".into(), selection);
    let selected = &runner.sources["generated/ops-selection.json"];
    let pins = [
        ops.pin("ops-release.json")?,
        selected,
        ops.pin("simplestchat-ops")?,
    ];
    let names = ["ops-release.json", "ops-selection.json", "simplestchat-ops"];
    let path = runner.output.join("ops-transfer.tar");
    let mut file = files::private_file(&path, control)?;
    let body = (|| {
        let mut builder = tar::Builder::new(&mut file);
        for (name, original) in names.iter().zip(pins) {
            original.check(control)?;
            let mut header = tar::Header::new_ustar();
            header.set_path(name).map_err(|_| Error::Input)?;
            header.set_mode(0o600);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(original.bytes);
            header.set_cksum();
            let reader = Bytes {
                original,
                control,
                at: 0,
            };
            builder.append(&header, reader).map_err(|_| Error::Io)?;
            original.check(control)?;
        }
        builder.finish().map_err(|_| Error::Io)?;
        drop(builder);
        let length = file.metadata().map_err(|_| Error::Io)?.len();
        let padding = (10240 - length % 10240) % 10240;
        file.write_all(&vec![
            0;
            usize::try_from(padding).map_err(|_| Error::Input)?
        ])
        .map_err(|_| Error::Io)?;
        file.flush().map_err(|_| Error::Io)?;
        file.sync_all().map_err(|_| Error::Io)?;
        control.check()
    })();
    if let Err(error) = body {
        control.record(error);
    }
    let closed = files::close(file, control);
    body.and(closed)?;
    let mut original = Original::open(&path, 256 * 1024 * 1024 + 65536 + 16384 + 16384, control)?;
    let result = async {
        let pins = [
            ops.pin("ops-release.json")?,
            &runner.sources["generated/ops-selection.json"],
            ops.pin("simplestchat-ops")?,
        ];
        input_properties(original.file()?, true)?;
        admit_archive(&original, pins, control)?;
        let mut requested = serde_json::Map::new();
        requested.insert("schemaVersion".into(), 1.into());
        for ((key, name), pin) in ["manifest", "selection", "binary"]
            .iter()
            .zip(names)
            .zip(pins)
        {
            requested.insert(
                (*key).into(),
                json!({"path":format!("/root/simplestchat-vm-fixture/ops-input/{name}"),
                "sha256":pin.sha256,"bytes":pin.bytes}),
            );
        }
        let install = serde_json::to_vec(&Value::Object(requested)).map_err(|_| Error::Input)?;
        let selection = serde_json::to_vec(&ops.selection).map_err(|_| Error::Input)?;
        let observed = runner
            .run(
                control,
                "inspect-musl",
                "ops",
                vec![
                    "distribution".into(),
                    "inspect-executable".into(),
                    "--path".into(),
                    ops.pin("simplestchat-ops")?.path.as_os_str().into(),
                ],
                Duration::from_secs(120),
                &[0],
            )
            .await?;
        if observed.stdout.len() > 4096 {
            return Err(Error::Refused);
        }
        let inspected = super::decode(&observed.stdout)?;
        let pin = ops.pin("simplestchat-ops")?;
        // The GNU controller inspects held musl bytes; it never executes them on the controller.
        super::projection::executable(&inspected, &pin.sha256, pin.bytes)?;
        let mut argv = auth::args(&["--run-id", run_id, "--selection-json"]);
        argv.extend([
            String::from_utf8(selection)
                .map_err(|_| Error::Input)?
                .into(),
            "--artifact-id".into(),
            options.artifact.to_string().into(),
            "--transfer-sha256".into(),
            original.sha256.clone().into(),
            "--transfer-bytes".into(),
            original.bytes.to_string().into(),
            "--install-request-json".into(),
            String::from_utf8(install.clone())
                .map_err(|_| Error::Input)?
                .into(),
            "--install-request-sha256".into(),
            files::hex(&sha2::Sha256::digest(&install)).into(),
            "--executable-json".into(),
            serde_json::to_string(&inspected)
                .map_err(|_| Error::Input)?
                .into(),
        ]);
        for ((prefix, _), pin) in ["manifest", "selection", "binary"].iter().zip(names).zip([
            ops.pin("ops-release.json")?,
            &runner.sources["generated/ops-selection.json"],
            ops.pin("simplestchat-ops")?,
        ]) {
            argv.extend([
                format!("--{prefix}-sha256").into(),
                pin.sha256.clone().into(),
                format!("--{prefix}-bytes").into(),
                pin.bytes.to_string().into(),
            ]);
        }
        original.check(control)?;
        ops.check(control)?;
        Ok(argv)
    }
    .await;
    match result {
        Ok(argv) => Ok(Transfer { original, argv }),
        Err(error) => {
            control.record(error);
            let _ = original.finish(control);
            Err(error)
        }
    }
}
use sha2::Digest;

fn input_properties(file: &std::fs::File, transfer: bool) -> Result<()> {
    let stamp = file.metadata().map_err(|_| Error::Io)?;
    if !stamp.is_file()
        || stamp.nlink() != 1
        || stamp.mode() & 0o7777 != 0o600
        || stamp.uid() != rustix::process::getuid().as_raw()
    {
        return Err(Error::Refused);
    }
    if transfer {
        let flags = rustix::fs::fcntl_getfl(file).map_err(|_| Error::Io)?;
        let mut borrowed = file;
        if stamp.gid() != rustix::process::getgid().as_raw()
            || flags & rustix::fs::OFlags::ACCMODE != rustix::fs::OFlags::RDONLY
            || borrowed.stream_position().map_err(|_| Error::Io)? != 0
        {
            return Err(Error::Refused);
        }
    }
    Ok(())
}
fn admit_archive(original: &Original, pins: [&Original; 3], control: &mut Control) -> Result<()> {
    original.check(control)?;
    let reader = Bytes {
        original,
        control,
        at: 0,
    };
    let mut archive = tar::Archive::new(reader);
    let mut offset = 0u64;
    let mut count = 0usize;
    for entry in archive.entries().map_err(|_| Error::Refused)? {
        let mut entry = entry.map_err(|_| Error::Refused)?;
        let names = ["ops-release.json", "ops-selection.json", "simplestchat-ops"];
        if count >= 3 {
            return Err(Error::Refused);
        }
        let pin = pins[count];
        let header = entry.header();
        let bytes = header.as_bytes();
        if entry.path_bytes().as_ref() != names[count].as_bytes()
            || entry.link_name_bytes().is_some()
            || bytes[156] != b'0'
            || &bytes[257..263] != b"ustar\0"
            || header.uid().map_err(|_| Error::Refused)? != 0
            || header.gid().map_err(|_| Error::Refused)? != 0
            || header.mode().map_err(|_| Error::Refused)? != 0o600
            || header.size().map_err(|_| Error::Refused)? != pin.bytes
            || entry.raw_header_position() != offset
            || entry.raw_file_position() != offset + 512
        {
            return Err(Error::Refused);
        }
        let mut hash = sha2::Sha256::new();
        let mut total = 0u64;
        let mut block = [0u8; 65536];
        loop {
            let read = entry.read(&mut block).map_err(|_| Error::Io)?;
            if read == 0 {
                break;
            }
            total = total.checked_add(read as u64).ok_or(Error::Refused)?;
            if total > pin.bytes {
                return Err(Error::Refused);
            }
            hash.update(&block[..read]);
        }
        if total != pin.bytes || files::hex(&hash.finalize()) != pin.sha256 {
            return Err(Error::Changed);
        }
        offset = offset
            .checked_add(512)
            .and_then(|value| value.checked_add(pin.bytes.div_ceil(512) * 512))
            .ok_or(Error::Refused)?;
        count += 1;
    }
    let reader = archive.into_inner();
    let control = reader.control;
    if count != 3 || original.bytes < offset + 1024 || original.bytes > offset + 10240 {
        return Err(Error::Refused);
    }
    let mut at = offset;
    let mut block = [0u8; 65536];
    while at < original.bytes {
        control.check()?;
        let limit = (original.bytes - at).min(block.len() as u64) as usize;
        let read = original
            .file()?
            .read_at(&mut block[..limit], at)
            .map_err(|_| Error::Io)?;
        control.check()?;
        if read == 0 || block[..read].iter().any(|byte| *byte != 0) {
            return Err(Error::Refused);
        }
        at += read as u64;
    }
    original.check(control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::{self, OpenOptions},
        os::unix::fs::OpenOptionsExt,
        sync::atomic::{AtomicU64, Ordering},
        time::Instant,
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn work() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "maintain-input-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }
    #[test]
    fn fixed_bundle_and_transfer_keep_nlink_mode_and_readonly_offset_admission() {
        let path = work();
        let leaf = path.join("input");
        let mut write = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&leaf)
            .unwrap();
        write.write_all(b"original").unwrap();
        let writable = input_properties(&write, true);
        let closed = nix::unistd::close(write);
        let mut read = std::fs::File::open(&leaf).unwrap();
        let good = input_properties(&read, true);
        let _ = read.seek(std::io::SeekFrom::Start(1)).unwrap();
        let offset = input_properties(&read, true);
        let _ = read.seek(std::io::SeekFrom::Start(0)).unwrap();
        fs::hard_link(&leaf, path.join("alias")).unwrap();
        let linked = input_properties(&read, false);
        fs::remove_file(path.join("alias")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o644)).unwrap();
        let public_mode = input_properties(&read, false);
        let read_closed = nix::unistd::close(read);
        fs::remove_dir_all(path).unwrap();
        assert_eq!(writable, Err(Error::Refused));
        assert!(closed.is_ok());
        assert!(good.is_ok());
        assert_eq!(offset, Err(Error::Refused));
        assert_eq!(linked, Err(Error::Refused));
        assert_eq!(public_mode, Err(Error::Refused));
        assert!(read_closed.is_ok());
    }
    #[test]
    fn independent_fixed_ustar_admission_rejects_changed_member_bytes_before_vm() {
        let path = work();
        let mut control = Control::new(Instant::now()).unwrap();
        let mut pins = Vec::new();
        for (name, data) in [
            ("ops-release.json", b"manifest".as_slice()),
            ("ops-selection.json", b"selection"),
            ("simplestchat-ops", b"binary"),
        ] {
            let leaf = path.join(name);
            files::write(&leaf, data, &mut control).unwrap();
            pins.push(Original::open(&leaf, 20, &mut control).unwrap());
        }
        let tar_path = path.join("transfer.tar");
        let mut file = files::private_file(&tar_path, &mut control).unwrap();
        {
            let mut writer = tar::Builder::new(&mut file);
            for original in &pins {
                let mut header = tar::Header::new_ustar();
                header.set_mode(0o600);
                header.set_uid(0);
                header.set_gid(0);
                header.set_size(original.bytes);
                header.set_entry_type(tar::EntryType::Regular);
                header.set_path(original.path.file_name().unwrap()).unwrap();
                header.set_cksum();
                writer
                    .append(&header, original.data(20, &mut control).unwrap().as_slice())
                    .unwrap();
            }
            writer.finish().unwrap();
        }
        file.sync_all().unwrap();
        let writer_closed = nix::unistd::close(file);
        let mut original = Original::open(&tar_path, 65536, &mut control).unwrap();
        let good = admit_archive(&original, [&pins[0], &pins[1], &pins[2]], &mut control);
        let first_closed = original.finish(&mut control);
        let mut mutable = OpenOptions::new().write(true).open(&tar_path).unwrap();
        mutable.seek(std::io::SeekFrom::Start(512)).unwrap();
        mutable.write_all(b"M").unwrap();
        let mutation_closed = nix::unistd::close(mutable);
        let mut original = Original::open(&tar_path, 65536, &mut control).unwrap();
        let bad = admit_archive(&original, [&pins[0], &pins[1], &pins[2]], &mut control);
        if let Err(error) = bad {
            control.record(error);
        }
        let _ = original.finish(&mut control);
        for pin in &mut pins {
            let _ = pin.finish(&mut control);
        }
        control.settle();
        let _ = control.finish();
        fs::remove_dir_all(path).unwrap();
        assert!(writer_closed.is_ok() && mutation_closed.is_ok() && first_closed.is_ok());
        assert!(good.is_ok());
        assert_eq!(bad, Err(Error::Changed));
    }
}
