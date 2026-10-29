//! Original current/previous payload admission and fixed canonical release copies.
//! Archive graphs are passive observations; signatures remain the controller's duty.

use super::*;
use crate::release::{archive::VerifiedArchive, manifest::Manifest};
use std::{fs::File, os::unix::fs::MetadataExt};

pub(in super::super) struct Admitted {
    pub manifest: Manifest,
    pub archive: VerifiedArchive,
    pub manifest_index: usize,
}
pub(super) struct Payloads {
    pub current: Admitted,
    pub previous: Admitted,
}
fn matches_pin(original: &Original, pin: &super::super::protocol::Pin) -> Result<()> {
    require(original.sha256 == pin.sha256 && original.bytes == pin.bytes)
}
fn manifest_for(bytes: &[u8], revision: &str, archive_sha: &str) -> Result<Manifest> {
    let manifest = Manifest::decode(bytes).map_err(|_| Error::Refused)?;
    require(manifest.fields().revision == revision)?;
    require(manifest.fields().archive_sha256 == archive_sha)?;
    Ok(manifest)
}
fn same_migrations(current: &Manifest, previous: &Manifest) -> Result<()> {
    require(
        !current.fields().migrations.is_empty()
            && current.fields().migrations == previous.fields().migrations,
    )
}
fn retain(ctx: &mut Context, path: &Path, maximum: u64) -> Result<usize> {
    let original = Original::open(path, maximum, 0, 0o600, &mut ctx.control)?;
    let index = ctx.originals.len();
    ctx.originals.push(original);
    ctx.originals[index].require_parent(0, 0o700, &mut ctx.control)?;
    Ok(index)
}
fn graph(ctx: &mut Context, archive_index: usize, manifest: &Manifest) -> Result<VerifiedArchive> {
    ctx.originals[archive_index].check(&mut ctx.control)?;
    let mut duplicate = ctx.originals[archive_index]
        .file()?
        .try_clone()
        .map_err(|_| Error::Io)?;
    let mut first = None;
    let verified =
        crate::release::archive::verify_retained_archive(&mut duplicate, manifest, || {
            if let Err(error) = ctx.control.check() {
                first.get_or_insert(error);
                false
            } else {
                true
            }
        })
        .map_err(|_| first.unwrap_or(Error::Refused));
    if let Err(error) = &verified {
        ctx.control.record(*error);
    }
    let closed = super::super::files::close(duplicate, &mut ctx.control);
    let verified = verified.and_then(|value| closed.map(|()| value))?;
    ctx.originals[archive_index].check(&mut ctx.control)?;
    Ok(verified)
}
fn absent(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(Error::Refused),
    }
}
fn directory_binding(metadata: &std::fs::Metadata) -> (u64, u64, u32, u32, u32) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.mode(),
        metadata.uid(),
        metadata.gid(),
    )
}
fn release_directory(ctx: &mut Context, revision: &str) -> Result<PathBuf> {
    ctx.control.check()?;
    require(super::super::protocol::digest(revision, 40))?;
    let parent = Path::new(PUBLIC).join("releases");
    require(std::fs::canonicalize(&parent).map_err(|_| Error::Io)? == parent)?;
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let held = rustix::fs::open(&parent, flags, rustix::fs::Mode::empty())
        .map(File::from)
        .map_err(|_| Error::Io)?;
    let mut child = None;
    let path = parent.join(revision);
    let result = (|| {
        let before = held.metadata().map_err(|_| Error::Io)?;
        require(
            before.is_dir()
                && before.uid() == 0
                && before.gid() == 0
                && before.mode() & 0o7777 == 0o700,
        )?;
        ctx.control.check()?;
        rustix::fs::mkdirat(&held, revision, rustix::fs::Mode::from_raw_mode(0o700))
            .map_err(|_| Error::Io)?;
        child = Some(
            rustix::fs::openat(&held, revision, flags, rustix::fs::Mode::empty())
                .map(File::from)
                .map_err(|_| Error::Io)?,
        );
        let created = child
            .as_ref()
            .ok_or(Error::Close)?
            .metadata()
            .map_err(|_| Error::Io)?;
        let visible = std::fs::symlink_metadata(&path).map_err(|_| Error::Changed)?;
        require(
            created.is_dir()
                && visible.is_dir()
                && created.uid() == 0
                && created.gid() == 0
                && created.mode() & 0o7777 == 0o700
                && directory_binding(&created) == directory_binding(&visible),
        )?;
        // Creating the fixed child changes parent timestamps and link count.
        require(
            directory_binding(&before)
                == directory_binding(&held.metadata().map_err(|_| Error::Io)?),
        )?;
        let visible_parent = std::fs::symlink_metadata(&parent).map_err(|_| Error::Changed)?;
        require(directory_binding(&before) == directory_binding(&visible_parent))?;
        absent(&path.join("staged.json"))?;
        ctx.control.check()
    })();
    if let Err(error) = result {
        ctx.control.record(error);
    }
    let mut first = None;
    for file in child.into_iter().chain([held]) {
        if let Err(error) = super::super::files::close(file, &mut ctx.control) {
            first.get_or_insert(error);
        }
    }
    result.and(first.map_or(Ok(()), Err))?;
    Ok(path)
}
fn admit(
    ctx: &mut Context,
    input: &Path,
    payload: &protocol::Payload,
    revision: &str,
) -> Result<Admitted> {
    let manifest_index = retain(ctx, &input.join("release.json"), 1024 * 1024)?;
    matches_pin(&ctx.originals[manifest_index], &payload.manifest)?;
    let bytes = ctx.originals[manifest_index].data(1024 * 1024, &mut ctx.control)?;
    let manifest = manifest_for(&bytes, revision, &payload.archive.sha256)?;
    let archive_index = retain(ctx, &input.join("image.tar"), 2 * 1024 * 1024 * 1024)?;
    matches_pin(&ctx.originals[archive_index], &payload.archive)?;
    let archive = graph(ctx, archive_index, &manifest)?;
    let release = release_directory(ctx, revision)?;
    let mut copied = Vec::new();
    for (index, name) in [
        (manifest_index, "release.json"),
        (archive_index, "image.tar"),
    ] {
        let original =
            super::files::copy(&ctx.originals[index], &release.join(name), &mut ctx.control)?;
        copied.push(ctx.originals.len());
        ctx.originals.push(original);
    }
    absent(&release.join("staged.json"))?;
    ctx.control.check()?;
    Ok(Admitted {
        manifest,
        archive,
        manifest_index: copied[0],
    })
}
pub(super) fn prepare(ctx: &mut Context, request: &protocol::Request) -> Result<Payloads> {
    let current = admit(
        ctx,
        &Path::new(PRIVATE).join("server-input"),
        &request.current,
        &request.revision,
    )?;
    let previous = admit(
        ctx,
        &Path::new(ROOT).join("deploy-runtime/previous-input"),
        &request.previous,
        &request.previous_revision,
    )?;
    same_migrations(&current.manifest, &previous.manifest)?;
    for original in &ctx.originals {
        original.check(&mut ctx.control)?;
    }
    ctx.control.check()?;
    Ok(Payloads { current, previous })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn manifest(revision: &str, checksum: char) -> Vec<u8> {
        serde_json::to_vec(
            &json!({"schemaVersion":1, "revision":revision, "platform":"linux/amd64",
            "archiveSha256":"c".repeat(64),
            "imageTag":format!("simplestchat-release/production:{revision}"),
            "migrations":{"1":checksum.to_string().repeat(96)},
            "createdAt":"2026-10-29T00:00:00Z"}),
        )
        .unwrap()
    }
    #[test]
    fn explicit_revisions_archive_join_and_exact_packaged_schema_cannot_be_substituted() {
        let current = manifest_for(
            &manifest(&"a".repeat(40), 'd'),
            &"a".repeat(40),
            &"c".repeat(64),
        )
        .unwrap();
        let previous = manifest_for(
            &manifest(&"b".repeat(40), 'd'),
            &"b".repeat(40),
            &"c".repeat(64),
        )
        .unwrap();
        assert!(same_migrations(&current, &previous).is_ok());
        assert!(
            manifest_for(
                &manifest(&"a".repeat(40), 'd'),
                &"b".repeat(40),
                &"c".repeat(64)
            )
            .is_err()
        );
        assert!(
            manifest_for(
                &manifest(&"a".repeat(40), 'd'),
                &"a".repeat(40),
                &"e".repeat(64)
            )
            .is_err()
        );
        let changed = manifest_for(
            &manifest(&"b".repeat(40), 'e'),
            &"b".repeat(40),
            &"c".repeat(64),
        )
        .unwrap();
        assert!(same_migrations(&current, &changed).is_err());
        let mut empty: serde_json::Value =
            serde_json::from_slice(&manifest(&"a".repeat(40), 'd')).unwrap();
        empty["migrations"] = json!({});
        assert!(
            manifest_for(
                &serde_json::to_vec(&empty).unwrap(),
                &"a".repeat(40),
                &"c".repeat(64)
            )
            .is_err()
        );
    }
    #[test]
    fn original_pin_checks_require_both_byte_count_and_digest() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            sync::atomic::{AtomicU64, Ordering},
            time::Instant,
        };
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "deploy-payload-pin-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let file = path.join("release.json");
        fs::write(&file, b"retained fixture").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let mut control = super::super::super::control::Control::new(Instant::now()).unwrap();
        let mut original = Original::fixture(&file, 100, 0o600, &mut control).unwrap();
        let pin = super::super::super::protocol::Pin {
            sha256: original.sha256.clone(),
            bytes: original.bytes,
        };
        let matched = matches_pin(&original, &pin);
        let wrong_size = matches_pin(
            &original,
            &super::super::super::protocol::Pin {
                sha256: pin.sha256.clone(),
                bytes: pin.bytes + 1,
            },
        );
        let wrong_hash = matches_pin(
            &original,
            &super::super::super::protocol::Pin {
                sha256: "f".repeat(64),
                bytes: pin.bytes,
            },
        );
        let closed = original.finish(&mut control);
        let finished = control.finish();
        fs::remove_file(file).unwrap();
        fs::remove_dir(path).unwrap();
        assert!(matched.is_ok());
        assert!(wrong_size.is_err() && wrong_hash.is_err());
        assert!(closed.is_ok() && finished.is_ok());
    }
}
