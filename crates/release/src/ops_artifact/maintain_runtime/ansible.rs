//! The one recap line `ansible-playbook` prints for the generated guest.

use serde_json::{Value, json};

/// The task counts of one playbook run on the single generated host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Recap {
    pub ok: u64,
    pub changed: u64,
    pub unreachable: u64,
    pub failed: u64,
    pub skipped: u64,
    pub rescued: u64,
    pub ignored: u64,
}

impl Recap {
    /// The counts of the one recap line of `vmfixture` in `output`. Missing,
    /// repeated or non-numeric recaps, and a recap of any other host, are
    /// refused by fixed code; nothing else of the output is kept.
    pub fn parse(output: &[u8]) -> Result<Self, &'static str> {
        let text = std::str::from_utf8(output).map_err(|_| "vm_ansible_recap_text")?;
        let line = regex::Regex::new(concat!(
            r"(?m)^vmfixture\s+:\s+ok=([0-9]+)\s+changed=([0-9]+)\s+unreachable=([0-9]+)",
            r"\s+failed=([0-9]+)\s+skipped=([0-9]+)\s+rescued=([0-9]+)\s+ignored=([0-9]+)\s*$"
        ))
        .map_err(|_| "vm_ansible_recap_missing_or_ambiguous")?;
        let found: Vec<_> = line.captures_iter(text).collect();
        let [counts] = found.as_slice() else {
            return Err("vm_ansible_recap_missing_or_ambiguous");
        };
        let hosts =
            regex::Regex::new(r"(?m)^(\S+)\s+:\s+ok=").map_err(|_| "vm_ansible_unexpected_host")?;
        let named: Vec<&str> = hosts
            .captures_iter(text)
            .filter_map(|row| row.get(1).map(|host| host.as_str()))
            .collect();
        if named != ["vmfixture"] {
            return Err("vm_ansible_unexpected_host");
        }
        let count = |index: usize| {
            counts[index]
                .parse::<u64>()
                .map_err(|_| "vm_ansible_recap_missing_or_ambiguous")
        };
        Ok(Self {
            ok: count(1)?,
            changed: count(2)?,
            unreachable: count(3)?,
            failed: count(4)?,
            skipped: count(5)?,
            rescued: count(6)?,
            ignored: count(7)?,
        })
    }

    /// A complete run did something and left nothing unreachable, failed,
    /// rescued or ignored; a repeated run (`unchanged`) changed nothing.
    pub fn verdict(&self, unchanged: bool) -> Result<(), &'static str> {
        let complete = self.ok > 0
            && self.unreachable == 0
            && self.failed == 0
            && self.rescued == 0
            && self.ignored == 0;
        if !complete {
            return Err("vm_ansible_incomplete");
        }
        if unchanged && self.changed != 0 {
            return Err("vm_ansible_not_idempotent");
        }
        Ok(())
    }

    /// The counts as a summary records them.
    pub fn to_json(self) -> Value {
        json!({"ok":self.ok,"changed":self.changed,"unreachable":self.unreachable,
            "failed":self.failed,"skipped":self.skipped,"rescued":self.rescued,
            "ignored":self.ignored})
    }
}
