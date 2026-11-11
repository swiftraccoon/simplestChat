//! The scrape credential: the prepared metrics token alone, copied from the
//! deployment's private environment file into the file Prometheus reads. The
//! token never leaves the host and is never printed.

use crate::host::failure::{Failure, Result, require};
use crate::host::state::{self, Access};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

const NAME: &str = "METRICS_TOKEN";
const TOKEN_BYTES: u64 = 64;
const MAX_ENVIRONMENT_BYTES: usize = 1024 * 1024;

/// Copy the token when the credential is missing, differs, or is not exactly
/// the reader's file. Returns the line that says whether anything changed.
pub(super) fn credential(
    environment: &Path,
    destination: &Path,
    access: &Access,
) -> Result<&'static str> {
    let text = String::from_utf8(state::read(environment, MAX_ENVIRONMENT_BYTES)?);
    let text = text.map_err(|_| Failure::Value)?;
    // As a shell reads the file: the last assignment of a name is its value.
    // A comment line never spells the bare name before its first `=`.
    let mut values = text.lines().filter_map(|line| line.split_once('='));
    let token = values.rfind(|(name, _)| *name == NAME);
    let (_, token) = token.ok_or(Failure::Key)?;
    let hexadecimal = |byte: u8| matches!(byte, b'0'..=b'9' | b'a'..=b'f');
    let valid = token.len() as u64 == TOKEN_BYTES && token.bytes().all(hexadecimal);
    require(valid, "Invalid prepared metrics token")?;
    let current = match std::fs::symlink_metadata(destination) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(metadata) = current {
        require(metadata.is_file(), "Unexpected file type")?;
        let theirs = metadata.uid() == access.owner
            && metadata.gid() == access.group
            && metadata.permissions().mode() & 0o7777 == access.mode;
        let same = metadata.len() == TOKEN_BYTES && std::fs::read(destination)? == token.as_bytes();
        if theirs && same {
            return Ok("retained\n");
        }
    }
    state::atomic_for(destination, token.as_bytes(), access)?;
    Ok("changed\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::state::tests::Scratch;

    fn access() -> Access {
        Access {
            owner: rustix::process::geteuid().as_raw(),
            group: rustix::process::getegid().as_raw(),
            mode: 0o440,
        }
    }

    /// The last assignment wins, comments and other settings are ignored, and
    /// a repeat changes nothing; a rotated token, a widened mode and a wrong
    /// length are each replaced by exactly the token, with the reader's mode.
    #[test]
    fn credential_is_exactly_the_prepared_token_and_repeats_are_retained() {
        let scratch = Scratch::new();
        let environment = scratch.path().join("app.env");
        let destination = scratch.path().join("metrics-token");
        let (first, second) = ("a1".repeat(32), "b2".repeat(32));
        let text = format!(
            "# METRICS_TOKEN={}\nJWT=x=y\nMETRICS_TOKEN={}\r\nOTHER\nMETRICS_TOKEN={first}\n",
            "c".repeat(64),
            "d".repeat(64)
        );
        std::fs::write(&environment, text).unwrap();
        let copy = || credential(&environment, &destination, &access());
        let mode = || {
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        };
        assert_eq!(copy(), Ok("changed\n"));
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), first);
        assert_eq!(mode(), 0o440);
        assert_eq!(copy(), Ok("retained\n"));
        std::fs::write(&environment, format!("METRICS_TOKEN={second}\n")).unwrap();
        assert_eq!(copy(), Ok("changed\n"));
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), second);
        std::fs::set_permissions(&destination, PermissionsExt::from_mode(0o644)).unwrap();
        assert_eq!(copy(), Ok("changed\n"));
        assert_eq!(mode(), 0o440);
        std::fs::set_permissions(&destination, PermissionsExt::from_mode(0o600)).unwrap();
        std::fs::write(&destination, format!("{second}\n")).unwrap();
        assert_eq!(copy(), Ok("changed\n"));
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), second);
        assert_eq!(copy(), Ok("retained\n"));
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 2);
    }

    /// A missing, malformed or differently spelled token refuses and leaves
    /// the existing credential alone; so does anything that is not a file
    /// where the credential belongs.
    #[test]
    fn missing_or_malformed_tokens_and_unsafe_destinations_refuse() {
        let scratch = Scratch::new();
        let environment = scratch.path().join("app.env");
        let destination = scratch.path().join("metrics-token");
        let kept = "e".repeat(64);
        std::fs::write(&destination, &kept).unwrap();
        let copy = || credential(&environment, &destination, &access());
        assert_eq!(copy(), Err(Failure::Os));
        let invalid = || Err(Failure::Refused("Invalid prepared metrics token"));
        for (text, expected) in [
            (String::new(), Err(Failure::Key)),
            (format!("METRICS_TOKEN ={kept}\n"), Err(Failure::Key)),
            (format!(" METRICS_TOKEN={kept}\n"), Err(Failure::Key)),
            (format!("#METRICS_TOKEN={kept}\n"), Err(Failure::Key)),
            ("METRICS_TOKEN=\n".to_owned(), invalid()),
            (format!("METRICS_TOKEN={}\n", "E".repeat(64)), invalid()),
            (format!("METRICS_TOKEN={}\n", "e".repeat(63)), invalid()),
            (format!("METRICS_TOKEN={} \n", "e".repeat(64)), invalid()),
            (format!("METRICS_TOKEN=\"{}\"\n", "e".repeat(64)), invalid()),
        ] {
            std::fs::write(&environment, &text).unwrap();
            assert_eq!(copy(), expected, "{text:?}");
            assert_eq!(std::fs::read_to_string(&destination).unwrap(), kept);
        }
        std::fs::write(&environment, format!("METRICS_TOKEN={}\n", "f".repeat(64))).unwrap();
        std::fs::remove_file(&destination).unwrap();
        std::os::unix::fs::symlink(&environment, &destination).unwrap();
        assert_eq!(copy(), Err(Failure::Refused("Unexpected file type")));
        std::fs::remove_file(&destination).unwrap();
        std::fs::create_dir(&destination).unwrap();
        assert_eq!(copy(), Err(Failure::Refused("Unexpected file type")));
        assert!(
            std::fs::read_to_string(&environment)
                .unwrap()
                .contains(&"f".repeat(64))
        );
    }
}
