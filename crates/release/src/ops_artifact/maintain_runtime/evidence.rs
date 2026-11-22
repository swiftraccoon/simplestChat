//! Fixed public projections of what an owned guest said before SSH
//! authenticated it: the cloud-init status, the emulator's first standard
//! error lines and the serial boot milestones. Each keeps only fixed labels,
//! counts and digests, never the text it read.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const CLOUD_STAGES: [&str; 4] = ["init-local", "init", "modules-config", "modules-final"];
const CLOUD_RUNNING: [&str; 4] = ["not started", "running", "done", "disabled"];
const CLOUD_CLASSES: [(&str, &str); 6] = [
    (
        "schema_validation",
        r"schema|invalid cloud.config|failed validating",
    ),
    ("deprecated_config", r"deprecat"),
    ("hostname", r"hostname|host name"),
    ("apt", r"\bapt\b|deb822|sources\.list"),
    ("network", r"network|dhcp|dns|name resolution"),
    ("ssh", r"\bssh\b|ssh_|host key|authorized.key"),
];
const MAX_CLOUD_STATUS: usize = 65536;
const MAX_CLOUD_GROUPS: usize = 17;
const MAX_CLOUD_ERRORS: usize = 128;
const MAX_CLOUD_MESSAGE: usize = 4096;
const MAX_CLOUD_TOTAL: usize = 512;

fn require(condition: bool, code: &'static str) -> Result<(), &'static str> {
    if condition { Ok(()) } else { Err(code) }
}

/// The error counts and fixed classes of one cloud-init status record.
fn cloud_errors(record: &Value) -> Result<Value, &'static str> {
    let object = record.as_object().ok_or("vm_cloud_init_record")?;
    let errors = object.get("errors").and_then(Value::as_array);
    let recoverable = object.get("recoverable_errors").and_then(Value::as_object);
    let (Some(errors), Some(recoverable)) = (errors, recoverable) else {
        return Err("vm_cloud_init_errors");
    };
    let groups = 1 + recoverable.len();
    require(groups <= MAX_CLOUD_GROUPS, "vm_cloud_init_error_limit")?;
    let mut messages = Vec::new();
    for group in std::iter::once(&object["errors"]).chain(recoverable.values()) {
        let group = group
            .as_array()
            .filter(|group| group.len() <= MAX_CLOUD_ERRORS)
            .ok_or("vm_cloud_init_errors")?;
        for message in group {
            let message = message
                .as_str()
                .filter(|message| message.chars().count() <= MAX_CLOUD_MESSAGE)
                .ok_or("vm_cloud_init_message")?;
            messages.push(message);
        }
    }
    require(
        messages.len() <= MAX_CLOUD_TOTAL,
        "vm_cloud_init_error_limit",
    )?;
    let mut classes = BTreeSet::new();
    let mut unclassified = 0;
    for message in &messages {
        let lower = message.to_lowercase();
        let mut matched = false;
        for (name, pattern) in CLOUD_CLASSES {
            let expression = regex::Regex::new(pattern).map_err(|_| "vm_cloud_init_record")?;
            if expression.is_match(&lower) {
                classes.insert(name);
                matched = true;
            }
        }
        unclassified += usize::from(!matched);
    }
    Ok(
        json!({"errors":errors.len(),"recoverableErrors":messages.len() - errors.len(),
        "failureClasses":classes,"unclassifiedErrors":unclassified}),
    )
}

/// The public projection of `cloud-init status --wait --format=json` that
/// exited with `status`: fixed status and stage labels, each record's error
/// counts and classes, and whether initialization completed healthy (exit 0,
/// `done` twice, no active stage, all four stage records, no error of any
/// kind). A report that is not one is refused by fixed code.
pub(super) fn cloud_status(bytes: &[u8], status: i32) -> Result<Value, &'static str> {
    require(bytes.len() <= MAX_CLOUD_STATUS, "vm_cloud_init_report_size")?;
    let record = super::decode(bytes).map_err(|_| "vm_cloud_init_report_invalid")?;
    let object = record.as_object().ok_or("vm_cloud_init_record")?;
    require(object.contains_key("stage"), "vm_cloud_init_stage")?;
    let label = object.get("status").and_then(Value::as_str);
    let known = label.filter(|label| CLOUD_RUNNING.contains(label) || *label == "error");
    let label = known.ok_or("vm_cloud_init_status")?;
    let extended = object.get("extended_status").and_then(Value::as_str);
    let extended = extended
        .filter(|extended| {
            CLOUD_RUNNING.contains(extended)
                || matches!(*extended, "degraded done" | "degraded running")
                || CLOUD_RUNNING
                    .iter()
                    .any(|name| *extended == format!("error - {name}"))
        })
        .ok_or("vm_cloud_init_status")?;
    let stage = &object["stage"];
    let staged = stage.is_null()
        || stage
            .as_str()
            .is_some_and(|stage| CLOUD_STAGES.contains(&stage));
    require(staged, "vm_cloud_init_stage")?;
    let aggregate = cloud_errors(&record)?;
    let mut stages = serde_json::Map::new();
    for name in CLOUD_STAGES {
        if let Some(value) = object.get(name) {
            stages.insert(name.into(), cloud_errors(value)?);
        }
    }
    let healthy = std::iter::once(&aggregate)
        .chain(stages.values())
        .all(|item| item["errors"] == 0 && item["recoverableErrors"] == 0);
    let passed = status == 0
        && label == "done"
        && extended == "done"
        && stage.is_null()
        && stages.len() == CLOUD_STAGES.len()
        && healthy;
    Ok(
        json!({"exitStatus":status,"parsed":true,"status":label,"extendedStatus":extended,
        "stage":stage,"aggregate":aggregate,"stages":stages,"passed":passed}),
    )
}

const MAX_STARTUP_LINES: usize = 12;
const MAX_STARTUP_LINE: usize = 1024;
const STARTUP_ASSERTION: &str = r"\bassertion\b.*\bfailed\b|\bcode should not be reached\b";
const STARTUP_PREFIXES: [&str; 6] = [
    "qemu-system-x86_64:",
    "qemu:",
    "prlimit:",
    "timeout:",
    "could not access kvm kernel module:",
    "failed to initialize kvm:",
];
const STARTUP_ERRORS: [(&str, &str); 14] = [
    ("kvm", r"\bkvm\b|accelerator|accel=|hardware virtualization"),
    (
        "block_backend",
        r"backing|block node|blockdev|qcow2|image format|block driver",
    ),
    (
        "backing_format",
        r"backing.*format|format.*backing|image format.*specified|auto-detect.*format",
    ),
    (
        "image_access",
        r"could not (?:open|read)|failed to (?:open|lock)|read-only|read only",
    ),
    (
        "sandbox",
        r"seccomp|sandbox|privilege|operation not permitted",
    ),
    (
        "boot_device",
        r"boot(?:able)? device|bootindex|boot order|bios|firmware|romfile",
    ),
    (
        "device",
        r"device|machine type|machine.*support|bus .*found|driver.*found",
    ),
    (
        "memory",
        r"memory|mmap|address.space|allocation|allocate|ram size|pc\.ram",
    ),
    (
        "resource_limit",
        r"rlimit|prlimit|resource limit|too many open files|file size limit",
    ),
    (
        "thread_start",
        r"gthread|pthread|qemu_thread_create|creating thread|glib-error",
    ),
    ("assertion_failure", STARTUP_ASSERTION),
    (
        "network",
        r"network|netdev|host forwarding|hostfwd|address already in use|slirp",
    ),
    (
        "option",
        concat!(
            r"invalid (?:option|parameter|argument)|unknown option|unrecognized option",
            r"|expects|requires"
        ),
    ),
    (
        "execution",
        r"failed to (?:execute|run)|command not found|exec format",
    ),
];
const STARTUP_REASONS: [&str; 11] = [
    "permission denied",
    "operation not permitted",
    "cannot allocate memory",
    "invalid argument",
    "no such file or directory",
    "no such device",
    "not supported",
    "address already in use",
    "too many open files",
    "resource temporarily unavailable",
    "read-only",
];
const STARTUP_COMPONENTS: [&str; 16] = [
    "rootdisk",
    "seed",
    "guest.qcow2",
    "base.qcow2",
    "seed.img",
    "pc.ram",
    "virtio-blk-pci",
    "virtio-net-pci",
    "q35",
    "kvm",
    "qcow2",
    "seccomp",
    "prlimit",
    "glib",
    "gthread",
    "pthread",
];
const STARTUP_SOURCES: [&str; 4] = ["thread-pool.c", "qemu-thread-posix.c", "async.c", "gmem.c"];

fn matches(pattern: &str, line: &str) -> bool {
    regex::Regex::new(pattern).is_ok_and(|expression| expression.is_match(line))
}

/// Whether `line` names source file `name` with a line number: the name at
/// the start or after `/`, a space, a tab or `:`, then `:`, digits, and a
/// `:`, a space, a tab or the end.
fn source_component(line: &str, name: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(name).any(|(at, _)| {
        let led = at == 0 || matches!(bytes[at - 1], b'/' | b' ' | b'\t' | b':');
        let rest = &bytes[at + name.len()..];
        let Some(digits) = rest.strip_prefix(b":") else {
            return false;
        };
        let count = digits
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        let after = digits.get(count);
        led && count > 0 && after.is_none_or(|byte| matches!(byte, b':' | b' ' | b'\t'))
    })
}

/// The lines of `content` as `splitlines` divides bytes: at `\n`, `\r\n`
/// and `\r`.
fn lines(content: &[u8]) -> Vec<&[u8]> {
    let mut found = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at < content.len() {
        match content[at] {
            b'\n' => {
                found.push(&content[start..at]);
                start = at + 1;
            }
            b'\r' => {
                found.push(&content[start..at]);
                if content.get(at + 1) == Some(&b'\n') {
                    at += 1;
                }
                start = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    if start < content.len() {
        found.push(&content[start..]);
    }
    found
}

/// The emulator's first standard error lines projected onto fixed classes,
/// reasons and component names. At most twelve lines of at most 1 KiB of
/// printable ASCII are read; any other line, and every line naming no known
/// error, is only counted as withheld.
pub(super) fn startup_errors(content: &[u8]) -> Value {
    let lines = lines(content);
    let mut messages = Vec::new();
    let mut withheld = lines.len().saturating_sub(MAX_STARTUP_LINES);
    for raw in lines.iter().take(MAX_STARTUP_LINES) {
        let printable = raw
            .iter()
            .all(|byte| *byte == b'\t' || (32..127).contains(byte));
        if raw.len() > MAX_STARTUP_LINE || !printable {
            withheld += 1;
            continue;
        }
        let line = String::from_utf8_lossy(raw).to_ascii_lowercase();
        let failure = format!(
            r"\b(?:glib-error|gthread|pthread_create)\b|^(?:glib:)?error:.*(?:{STARTUP_ASSERTION})"
        );
        let emulator = STARTUP_PREFIXES
            .iter()
            .any(|prefix| line.starts_with(prefix));
        if !emulator && !matches(&failure, &line) {
            withheld += 1;
            continue;
        }
        let classes: Vec<&str> = STARTUP_ERRORS
            .iter()
            .filter(|(_, pattern)| matches(pattern, &line))
            .map(|(name, _)| *name)
            .collect();
        if classes.is_empty() {
            withheld += 1;
            continue;
        }
        let reasons: Vec<&str> = STARTUP_REASONS
            .iter()
            .filter(|reason| line.contains(**reason))
            .copied()
            .collect();
        let mut components: Vec<&str> = STARTUP_COMPONENTS
            .iter()
            .filter(|name| line.contains(**name))
            .copied()
            .collect();
        components.extend(
            STARTUP_SOURCES
                .iter()
                .filter(|name| source_component(&line, name)),
        );
        messages.push(json!({"classes":classes,"reasons":reasons,"components":components}));
    }
    json!({"messages":messages,"withheldLines":withheld})
}

const MILESTONES: [(&str, &str); 12] = [
    ("firmware", r"seabios|tianocore|uefi firmware"),
    ("boot_disk", r"booting from hard disk"),
    (
        "no_bootable_disk",
        r"no bootable device|no bootable disk|boot failed",
    ),
    ("grub", r"\bgrub\b|booting `debian gnu/linux'"),
    ("linux", r"linux version|booting linux"),
    ("kernel_panic", r"kernel panic|not syncing"),
    (
        "initramfs",
        r"\(initramfs\)|unable to mount root fs|dropping to a shell",
    ),
    (
        "disk_resize",
        r"growroot|growpart|resize2fs|partition resize",
    ),
    ("reboot", r"reboot: restarting system|rebooting"),
    ("poweroff", r"power down|powering off"),
    ("cloud_init", r"cloud-init"),
    ("ssh", r"started openssh|starting openssh|ssh.service"),
];
const OVERLAP: usize = 128;

/// The guest's serial output before SSH authenticated it, kept only as a
/// byte count, a digest and the fixed boot milestones it showed. A milestone
/// split between two reads is found through a 128-byte overlap.
#[derive(Default)]
pub(super) struct Serial {
    bytes: usize,
    digest: Sha256,
    tail: Vec<u8>,
    milestones: BTreeSet<&'static str>,
}

impl Serial {
    /// Observe the next `chunk` the guest printed.
    pub fn observe(&mut self, chunk: &[u8]) {
        self.bytes += chunk.len();
        self.digest.update(chunk);
        let mut window = std::mem::take(&mut self.tail);
        window.extend_from_slice(chunk);
        window.make_ascii_lowercase();
        for (name, pattern) in MILESTONES {
            let found = regex::bytes::Regex::new(&format!("(?-u){pattern}"))
                .is_ok_and(|expression| expression.is_match(&window));
            if found {
                self.milestones.insert(name);
            }
        }
        let keep = window.len().saturating_sub(OVERLAP);
        self.tail = window.split_off(keep);
    }

    /// The count, digest and milestones seen.
    pub fn to_json(&self) -> Value {
        let digest: String = self
            .digest
            .clone()
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        json!({"bytesObserved":self.bytes,"sha256":digest,"milestones":self.milestones})
    }
}
