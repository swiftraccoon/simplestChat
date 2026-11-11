//! Backup receipts: private metadata and the current archive bytes, verified
//! without following links. A digest is rehashed unless an earlier check of
//! the very same settled file is on record and less than a day old.

use crate::host::failure::{Failure, Result, require};
use crate::host::state;
use crate::json::Json;
use chrono::{DateTime, NaiveDate, NaiveDateTime};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_RECEIPT_BYTES: u64 = 16384;
const PRIVATE_MODE: u32 = 0o600;
const CACHE_SECONDS: f64 = 86400.0;
const CACHE_SCHEMA: i128 = 2;
/// A same-size rewrite can share a fresh filesystem timestamp, so only a file
/// settled for longer than a coarse clock tick is remembered.
const TIMESTAMP_MARGIN_SECONDS: f64 = 2.0;

/// The identity of one checked archive: where it is, what it must hash to and
/// the inode state that hash was taken from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Signature {
    pub path: String,
    pub digest: String,
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub modified: i128,
    pub changed: i128,
}

/// One remembered check.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub signature: Signature,
    pub checked_at: f64,
}

impl Entry {
    /// Read a remembered check; any other shape is no memory at all.
    pub(crate) fn from_json(value: &Json) -> Option<Self> {
        let whole = |value: &Json| match value {
            Json::Integer(text) => text.parse::<i128>().ok(),
            _ => None,
        };
        if whole(value.get("schemaVersion")?)? != CACHE_SCHEMA {
            return None;
        }
        let checked_at = match value.get("checkedAt")? {
            number @ (Json::Integer(_) | Json::Float(_)) => number.number()?,
            _ => return None,
        };
        let [path, digest, device, inode, size, modified, changed] =
            value.get("signature")?.array()?
        else {
            return None;
        };
        let signature = Signature {
            path: path.string()?.to_owned(),
            digest: digest.string()?.to_owned(),
            device: u64::try_from(whole(device)?).ok()?,
            inode: u64::try_from(whole(inode)?).ok()?,
            size: u64::try_from(whole(size)?).ok()?,
            modified: whole(modified)?,
            changed: whole(changed)?,
        };
        Some(Self {
            signature,
            checked_at,
        })
    }
}

impl serde::Serialize for Entry {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let signature = &self.signature;
        let fields = (
            &signature.path,
            &signature.digest,
            signature.device,
            signature.inode,
            signature.size,
            signature.modified,
            signature.changed,
        );
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("schemaVersion", &CACHE_SCHEMA)?;
        map.serialize_entry("signature", &fields)?;
        map.serialize_entry("checkedAt", &self.checked_at)?;
        map.end()
    }
}

/// Nanoseconds since the epoch from a seconds and nanoseconds pair.
fn nanoseconds(seconds: i64, fraction: i64) -> i128 {
    i128::from(seconds) * 1_000_000_000 + i128::from(fraction)
}

/// Open one file for reading without following a final link or blocking.
fn open(path: &Path) -> Result<std::fs::File> {
    use nix::fcntl::OFlag;
    let flags = (OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK).bits();
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)?)
}

/// Whether the opened file is an ordinary private file with one name.
fn private(metadata: &std::fs::Metadata, owner: u32) -> bool {
    metadata.is_file()
        && metadata.uid() == owner
        && metadata.mode() & 0o7777 == PRIVATE_MODE
        && metadata.nlink() == 1
}

/// The archive names a receipt may point at, beside itself.
fn archive_name(name: &str) -> bool {
    let nightly = |name: &str| {
        let bytes = name.as_bytes();
        bytes.len() == 16
            && bytes[..8].iter().all(u8::is_ascii_digit)
            && bytes[8] == b'T'
            && bytes[9..15].iter().all(u8::is_ascii_digit)
            && bytes[15] == b'Z'
    };
    name.strip_suffix(".dump")
        .is_some_and(|stem| stem == "database-before" || nightly(stem))
}

/// Seconds since the epoch of a completion time that carries its own offset.
fn completion(text: &str) -> Result<f64> {
    if let Ok(instant) = DateTime::parse_from_rfc3339(text) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "microseconds of this era fit a double"
        )]
        return Ok(instant.timestamp_micros() as f64 / 1_000_000.0);
    }
    // A time without an offset is a well-formed value that names no instant.
    let local = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
        || NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok();
    if local {
        return Err(Failure::Refused("Invalid backup completion time"));
    }
    Err(Failure::Value)
}

/// Verify one receipt and the archive it names, and return when the backup
/// completed. `now` is seconds since the epoch. With `cache`, a settled
/// archive's check is remembered and a remembered check of the identical file
/// is trusted for a day; any other memory is forgotten.
/// A receipt whose archive is present, private and has the recorded digest.
pub(crate) struct Validated {
    /// The archive the receipt names, beside it.
    pub dump: PathBuf,
    /// The receipt itself.
    pub record: Json,
    /// When the archive was completed, in seconds since the epoch.
    pub completed: f64,
}

/// The completion time of a validated receipt: see [`inspected`].
pub(crate) fn validated(
    receipt: &Path,
    owner: u32,
    now: f64,
    cache: Option<&mut Option<Entry>>,
) -> Result<f64> {
    inspected(receipt, owner, now, cache).map(|validated| validated.completed)
}

pub(crate) fn inspected(
    receipt: &Path,
    owner: u32,
    now: f64,
    cache: Option<&mut Option<Entry>>,
) -> Result<Validated> {
    let parent = receipt.parent().ok_or(Failure::Os)?;
    state::protected(parent, owner, true, &[0o700], None)?;
    state::protected(
        receipt,
        owner,
        false,
        &[PRIVATE_MODE],
        Some(MAX_RECEIPT_BYTES),
    )?;
    let mut source = open(receipt)?;
    let metadata = source.metadata()?;
    let bounded = private(&metadata, owner) && metadata.len() <= MAX_RECEIPT_BYTES;
    require(bounded, "Unsafe backup receipt")?;
    let mut bytes = Vec::new();
    source
        .by_ref()
        .take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    let record = state::decode(&bytes)?;
    record.object().ok_or(Failure::Value)?;
    let whole = |name: &str| match record.get(name) {
        Some(Json::Integer(text)) => text.parse::<i128>().ok(),
        _ => None,
    };
    require(
        whole("schemaVersion") == Some(1),
        "Invalid backup receipt schema",
    )?;
    let name = record
        .get("dump")
        .and_then(Json::string)
        .filter(|name| archive_name(name));
    require(name.is_some(), "Invalid backup filename")?;
    let size = whole("bytes").filter(|size| *size > 0);
    require(size.is_some(), "Invalid backup size")?;
    let hexadecimal = |digest: &&str| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    };
    let digest = record
        .get("sha256")
        .and_then(Json::string)
        .filter(hexadecimal);
    require(digest.is_some(), "Invalid backup digest")?;
    let completed = record
        .get("completedAt")
        .and_then(Json::string)
        .ok_or(Failure::Value)?;
    let completed = completion(completed)?;
    let reached = 0.0 < completed && completed <= now + 300.0;
    require(reached, "Invalid backup completion time")?;
    let (name, digest) = (name.unwrap_or_default(), digest.unwrap_or_default());
    let backup = receipt.with_file_name(name);
    state::protected(&backup, owner, false, &[PRIVATE_MODE], None)?;
    let mut source = open(&backup)?;
    let before = source.metadata()?;
    let recorded = u64::try_from(size.unwrap_or_default()).ok() == Some(before.len());
    require(
        private(&before, owner) && recorded,
        "Backup size or permissions differ",
    )?;
    let modified = nanoseconds(before.mtime(), before.mtime_nsec());
    let changed = nanoseconds(before.ctime(), before.ctime_nsec());
    let signature = Signature {
        path: backup.to_str().ok_or(Failure::Value)?.to_owned(),
        digest: digest.to_owned(),
        device: before.dev(),
        inode: before.ino(),
        size: before.len(),
        modified,
        changed,
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "nanosecond stamps of this era fit a double"
    )]
    let stable_after = modified.max(changed) as f64 / 1_000_000_000.0 + TIMESTAMP_MARGIN_SECONDS;
    let remembered = cache
        .as_ref()
        .and_then(|cache| cache.as_ref())
        .is_some_and(|entry| {
            entry.signature == signature
                && stable_after < entry.checked_at
                && (0.0..CACHE_SECONDS).contains(&(now - entry.checked_at))
        });
    let actual = if remembered {
        digest.to_owned()
    } else {
        let mut hasher = Sha256::new();
        let mut chunk = vec![0_u8; 1024 * 1024];
        loop {
            let count = source.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            hasher.update(&chunk[..count]);
        }
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    let after = source.metadata()?;
    let steady = (before.len(), modified, changed)
        == (
            after.len(),
            nanoseconds(after.mtime(), after.mtime_nsec()),
            nanoseconds(after.ctime(), after.ctime_nsec()),
        );
    if let Some(cache) = cache
        && !remembered
    {
        // Forgotten before any refusal below, exactly as it is replaced after.
        *cache = None;
        require(steady, "Backup changed during validation")?;
        require(actual == digest, "Backup digest differs")?;
        if stable_after < now {
            *cache = Some(Entry {
                signature,
                checked_at: now,
            });
        }
        return Ok(Validated {
            dump: backup,
            record,
            completed,
        });
    }
    require(steady, "Backup changed during validation")?;
    require(actual == digest, "Backup digest differs")?;
    Ok(Validated {
        dump: backup,
        record,
        completed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::state::tests::Scratch;
    use serde_json::Value;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn mode(text: &Value) -> std::fs::Permissions {
        std::fs::Permissions::from_mode(u32::from_str_radix(text.as_str().unwrap(), 8).unwrap())
    }

    /// Build one recorded state and return its receipt.
    fn build(case: &Value, scratch: &Path) -> std::path::PathBuf {
        let parent = scratch.join("release.fixture");
        std::fs::create_dir(&parent).unwrap();
        let receipt = parent.join("database-before.receipt.json");
        let text = match case["receiptRaw"].as_str() {
            Some(raw) => raw.to_owned(),
            None => case["receipt"].to_string(),
        };
        let place = |target: &Path, text: &str, kind: &str, bits: &Value, spare: &str| match kind {
            "symlink" => {
                let real = parent.join(format!("real.{spare}"));
                std::fs::write(&real, text).unwrap();
                std::fs::set_permissions(&real, mode(&serde_json::json!("600"))).unwrap();
                symlink(&real, target).unwrap();
            }
            "missing" => {}
            kind => {
                std::fs::write(target, text).unwrap();
                std::fs::set_permissions(target, mode(bits)).unwrap();
                if kind == "hardlink" {
                    std::fs::hard_link(target, parent.join(format!("second.{spare}"))).unwrap();
                }
            }
        };
        let kind = case["receiptKind"].as_str().unwrap();
        place(&receipt, &text, kind, &case["receiptMode"], "json");
        let named = case["receipt"]["dump"]
            .as_str()
            .filter(|name| !name.contains('/'));
        let dump = parent.join(named.unwrap_or("database-before.dump"));
        let kind = case["dumpKind"].as_str().unwrap();
        place(
            &dump,
            case["dump"].as_str().unwrap(),
            kind,
            &case["dumpMode"],
            "dump",
        );
        std::fs::set_permissions(&parent, mode(&case["parentMode"])).unwrap();
        receipt
    }

    /// Every recorded state of receipt and archive is accepted with the
    /// recorded completion time or fails as recorded, and the digest memory is
    /// filled, kept or forgotten exactly as the recorded sequence says.
    #[test]
    fn receipts_and_the_digest_memory_answer_as_recorded() {
        let recorded: Value = serde_json::from_str(include_str!("recorded_backup.json")).unwrap();
        let cases = recorded["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 48);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let scratch = Scratch::new();
            let receipt = build(case, scratch.path());
            let owner = std::fs::metadata(scratch.path()).unwrap().uid();
            let base = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64();
            let mut memory: Option<Entry> = None;
            let calls = case["calls"].as_array().unwrap();
            for (call, expected) in calls.iter().zip(case["results"].as_array().unwrap()) {
                let now = base + call["offset"].as_f64().unwrap();
                match call["cache"].as_str() {
                    // An earlier schema or a time that is not a number reads as no memory.
                    Some("empty" | "schema1" | "text-time") => memory = None,
                    Some("other-signature") => memory.as_mut().unwrap().signature.inode += 1,
                    _ => {}
                }
                let before = memory.clone();
                let given = call["cache"].as_str().map(|_| &mut memory);
                let answer = validated(&receipt, owner, now, given);
                match &answer {
                    Ok(completed) => {
                        assert_eq!(Some(*completed), expected["completed"].as_f64(), "{name}");
                    }
                    Err(Failure::Refused(message)) => {
                        assert_eq!(Some(*message), expected["refused"].as_str(), "{name}");
                    }
                    Err(Failure::Os) => assert_eq!(expected["error"], "OSError", "{name}"),
                    Err(_) => assert_eq!(expected["error"], "ValueError", "{name}"),
                }
                let state = match (call["cache"].as_str(), &memory) {
                    (None, _) => "none",
                    (_, None) => "empty",
                    (_, entry) if *entry == before => "unchanged",
                    _ => "filled",
                };
                assert_eq!(state, expected["cache"].as_str().unwrap(), "{name}");
                if let Some(entry) = &memory {
                    let offset = (entry.checked_at - base).round();
                    assert_eq!(Some(offset), expected["checkedOffset"].as_f64(), "{name}");
                }
            }
            let private = mode(&serde_json::json!("700"));
            std::fs::set_permissions(scratch.path().join("release.fixture"), private).unwrap();
        }
    }

    /// A remembered check is written and read back in the shape earlier
    /// collectors wrote, and anything else reads as no memory.
    #[test]
    fn a_remembered_check_round_trips_and_other_shapes_are_no_memory() {
        let signature = Signature {
            path: "/srv/x/database-before.dump".into(),
            digest: "ab".repeat(32),
            device: 2049,
            inode: 77,
            size: 112,
            modified: 1_794_272_523_000_000_001,
            changed: 1_794_272_523_000_000_002,
        };
        let entry = Entry {
            signature,
            checked_at: 1_794_272_600.5,
        };
        let text = serde_json::to_string(&entry).unwrap();
        assert_eq!(
            text,
            format!(
                "{{\"schemaVersion\":2,\"signature\":[\"/srv/x/database-before.dump\",\"{}\",2049,77,\
                 112,1794272523000000001,1794272523000000002],\"checkedAt\":1794272600.5}}",
                "ab".repeat(32)
            )
        );
        let read = Entry::from_json(&state::decode(text.as_bytes()).unwrap());
        assert_eq!(read, Some(entry));
        for other in [
            "{}",
            r#"{"schemaVersion":1,"signature":[],"checkedAt":1}"#,
            r#"{"schemaVersion":2,"signature":["a","b",1,2,3,4],"checkedAt":1}"#,
            r#"{"schemaVersion":2,"signature":["a","b",1,2,3,4,"5"],"checkedAt":1}"#,
            r#"{"schemaVersion":2,"signature":["a","b",1,2,3,4,5],"checkedAt":"1"}"#,
            r#"{"schemaVersion":2,"signature":["a","b",1,2,3,4,5],"checkedAt":true}"#,
            r#"{"schemaVersion":true,"signature":["a","b",1,2,3,4,5],"checkedAt":1}"#,
        ] {
            assert!(Entry::from_json(&state::decode(other.as_bytes()).unwrap()).is_none());
        }
        let whole = r#"{"schemaVersion":2,"signature":["a","b",1,2,3,4,5],"checkedAt":9}"#;
        assert!(Entry::from_json(&state::decode(whole.as_bytes()).unwrap()).is_some());
    }
}
