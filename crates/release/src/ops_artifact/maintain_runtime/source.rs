//! Clean exact-head source bundle.

use super::{Error, Result, control::Control, files::Original, options::Options, process::Runner};
use std::{ffi::OsString, os::unix::fs::MetadataExt, time::Duration};

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
        input_properties(original.file()?)?;
        original.check(control)
    })();
    if let Err(error) = admitted {
        control.record(error);
        let _ = original.finish(control);
        return Err(error);
    }
    Ok(Bundle { original, tree })
}

/// Admits the bundle only as a private, single-link file of the invoking user.
fn input_properties(file: &std::fs::File) -> Result<()> {
    let stamp = file.metadata().map_err(|_| Error::Io)?;
    if !stamp.is_file()
        || stamp.nlink() != 1
        || stamp.mode() & 0o7777 != 0o600
        || stamp.uid() != rustix::process::getuid().as_raw()
    {
        return Err(Error::Refused);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };
    fn work() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("maintain-input-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }

    #[test]
    fn fixed_bundle_keeps_nlink_and_mode_admission() {
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
        let closed = nix::unistd::close(write);
        let read = std::fs::File::open(&leaf).unwrap();
        let good = input_properties(&read);
        fs::hard_link(&leaf, path.join("alias")).unwrap();
        let linked = input_properties(&read);
        fs::remove_file(path.join("alias")).unwrap();
        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o644)).unwrap();
        let public_mode = input_properties(&read);
        let read_closed = nix::unistd::close(read);
        fs::remove_dir_all(path).unwrap();
        assert!(closed.is_ok());
        assert!(good.is_ok());
        assert_eq!(linked, Err(Error::Refused));
        assert_eq!(public_mode, Err(Error::Refused));
        assert!(read_closed.is_ok());
    }
}
