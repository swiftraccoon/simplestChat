//! The disposable Debian guest's checks of a provisioned deployment, as the
//! scheduled infrastructure job runs them after each playbook: the three
//! guest actions, the receipt each one prints and the controller admits, and
//! every decision the actions take over what they read.
//!
//! A receipt carries only aggregate success fields or four fixed labels of a
//! failed check; command output, paths, values and secrets stay in the guest.
//! The guest itself runs as `release infrastructure-guest` of the
//! distributed artifact (feature `maintain-guest`); everything in this module
//! is pure and available to every build.

mod model;

#[cfg(feature = "maintain-guest")]
mod guest;
#[cfg(feature = "maintain-guest")]
mod policy;
#[cfg(feature = "maintain-guest")]
pub use guest::run;

#[cfg(test)]
mod model_tests;
#[cfg(all(test, feature = "maintain-guest"))]
mod policy_tests;

use crate::json::{self, Json};
use serde_json::{Value, json};

/// The most bytes a receipt may have.
pub const MAX_RECEIPT: usize = 16384;

/// One guest action, run after the playbook that prepares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Compare protected configuration and policy after each `public.yml`.
    Snapshot,
    /// Migrate a new database with the real image and start the application.
    Database,
    /// Take a real backup through the installed units and restore it isolated.
    BackupRestore,
}

impl Action {
    /// The action as the command line and the receipt spell it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Database => "database",
            Self::BackupRestore => "backup-restore",
        }
    }

    /// The action `name` spells, if any.
    pub fn named(name: &str) -> Option<Self> {
        [Self::Snapshot, Self::Database, Self::BackupRestore]
            .into_iter()
            .find(|action| action.name() == name)
    }

    /// The success fields the action's receipt carries besides the common four.
    fn fields(self) -> &'static [&'static str] {
        match self {
            Self::Snapshot => &["configurationFiles", "idempotent"],
            Self::Database => &["counts", "migrations", "ready"],
            Self::BackupRestore => &[
                "archiveValidated",
                "cleanupPassed",
                "counts",
                "restoreVerified",
            ],
        }
    }
}

/// Where a failed check ran.
pub const PHASES: [&str; 9] = [
    "guard",
    "snapshot",
    "snapshot_files",
    "snapshot_units",
    "snapshot_compose",
    "database",
    "backup-restore",
    "report",
    "unknown",
];

/// The reviewed file, unit or service a failed check was about.
pub const CONTEXTS: [&str; 32] = [
    "unspecified",
    "marker",
    "selection",
    "helper",
    "docker_socket",
    "fixture_directory",
    "library_directory",
    "simplestchat-public",
    "results",
    "backups",
    "postgres",
    "postgres-socket",
    "caddy-data",
    "caddy-config",
    "secrets.json",
    "compose.base.yml",
    "Caddyfile",
    "compose.public.yml",
    "app.env",
    "migration.env",
    "proxy.env",
    "postgres-admin-password",
    "pg_hba.conf",
    "init-database.sql",
    "runtime-grants.sql",
    "images.json",
    "browser-assets-installed.json",
    "simplestchat-benchmark.service",
    "simplestchat-image-build.service",
    "simplestchat",
    "migrate",
    "caddy",
];

/// Each refusal message the guest gives a code for, with that code. Any other
/// refusal is `unclassified`.
pub const CODES: [(&str, &str); 39] = [
    (
        "Fixture file ownership, type, size or mode differs",
        "file_metadata",
    ),
    ("Fixture file changed while reading", "file_changed"),
    (
        "Fixture directory ownership or mode differs",
        "directory_metadata",
    ),
    ("Fixture selection schema or owner differs", "selection"),
    ("Fixture revision is invalid", "revision"),
    ("Fixture requires root", "root"),
    ("Fixture requires Linux x86_64", "platform"),
    ("Invalid fixture ownership token", "run_id"),
    ("Fixture requires Debian 13", "distribution"),
    ("Fixture requires QEMU", "hypervisor"),
    ("Fixture marker differs", "marker"),
    ("Fixture helper origin differs", "helper_origin"),
    ("Fixture Docker socket differs", "docker_socket"),
    ("Unexpected systemd property", "systemd_property"),
    ("Incomplete systemd properties", "systemd_properties"),
    ("Fixture unit identity differs", "unit_identity"),
    ("Fixture workload unit is enabled or active", "unit_state"),
    ("Fixture runtime directory differs", "runtime_directory"),
    (
        "Unexpected image build runtime directory",
        "image_build_directory",
    ),
    ("Fixture Compose user differs", "compose_user"),
    ("Fixture Compose isolation differs", "compose_isolation"),
    (
        "Fixture Compose added capabilities differ",
        "compose_capabilities",
    ),
    (
        "Fixture Compose privilege boundary differs",
        "compose_privileges",
    ),
    (
        "Fixture Compose resource limits differ",
        "compose_resources",
    ),
    ("Fixture Compose CPU limit differs", "compose_cpus"),
    ("Fixture isolated network differs", "compose_network"),
    ("Fixture Compose init differs", "compose_init"),
    (
        "Fixture HTTP publication is not loopback-only",
        "compose_http",
    ),
    ("Fixture Compose project differs", "compose_project"),
    ("Fixture Compose services differ", "compose_services"),
    (
        "Unexpected public configuration files",
        "configuration_files",
    ),
    ("Fixture deployment secret schema differs", "secret_schema"),
    ("Fixture secrets are not independent", "secret_independence"),
    (
        "Fixture application configuration differs",
        "application_environment",
    ),
    (
        "Fixture migration environment differs",
        "migration_environment",
    ),
    ("Fixture proxy environment differs", "proxy_environment"),
    ("Fixture database secret differs", "database_secret"),
    (
        "Fixture public project is already present",
        "project_present",
    ),
    (
        "Public playbook changed protected configuration",
        "configuration_changed",
    ),
];

/// The classes of failure a receipt names; `unexpected_error` is reserved
/// for a failure of no other class and no guest check produces it.
pub const CLASSES: [&str; 6] = [
    "release_error",
    "os_error",
    "value_error",
    "key_error",
    "subprocess_error",
    "unexpected_error",
];

/// The fixed labels of one failed guest check. Every label is one of this
/// module's reviewed constants, never text the guest read or printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    phase: &'static str,
    context: &'static str,
    code: &'static str,
    class: &'static str,
}

impl Diagnostic {
    /// The labels, each replaced by its fallback unless it is reviewed.
    pub(crate) fn labelled(phase: &str, context: &str, code: &str, class: &str) -> Self {
        let known = |list: &[&'static str], value: &str, fallback: &'static str| {
            list.iter()
                .copied()
                .find(|item| *item == value)
                .unwrap_or(fallback)
        };
        let codes: Vec<&'static str> = CODES.iter().map(|(_, code)| *code).collect();
        Self {
            phase: known(&PHASES, phase, "unknown"),
            context: known(&CONTEXTS, context, "unspecified"),
            code: known(&codes, code, "unclassified"),
            class: known(&CLASSES, class, "unexpected_error"),
        }
    }

    /// The phase, context, code and class, in that order.
    pub fn labels(&self) -> [&'static str; 4] {
        [self.phase, self.context, self.code, self.class]
    }

    /// The labels as a receipt and a summary record them.
    pub fn to_json(&self) -> Value {
        json!({"phase":self.phase,"context":self.context,"code":self.code,
            "exceptionClass":self.class})
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [phase, context, code, class] = self.labels();
        write!(formatter, "{phase}/{context}: {code} ({class})")
    }
}

impl std::error::Error for Diagnostic {}

/// What a well-formed receipt says.
#[derive(Clone, Debug, PartialEq)]
pub enum Receipt {
    /// The action passed; its aggregate fields, with `action` and `passed`.
    Passed(Value),
    /// The guest reported a failed check.
    Failed(Diagnostic),
}

/// A receipt that is not one the guest can print, by its fixed code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed(&'static str);

impl Malformed {
    /// The fixed code.
    pub fn code(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for Malformed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for Malformed {}

fn check(condition: bool, code: &'static str) -> Result<(), Malformed> {
    if condition {
        Ok(())
    } else {
        Err(Malformed(code))
    }
}

/// Whether a decoded JSON object has exactly `names` as its keys.
fn keys(value: &Json, names: &[&str]) -> bool {
    value.object().is_some_and(|object| {
        object.len() == names.len() && names.iter().all(|name| object.contains_key(*name))
    })
}

/// Whether `value` is a JSON integer (never a boolean) in `low..=high`.
fn integer(value: Option<&Json>, low: i64, high: i64) -> bool {
    value
        .and_then(Json::integer)
        .is_some_and(|number| (low..=high).contains(&number))
}

/// Admit what action `action` of run `run` printed on standard output with
/// exit status `status`: at most [`MAX_RECEIPT`] bytes of one duplicate-free
/// JSON object bound to the run and the action. Status 1 must carry exactly
/// the four reviewed failure labels; status 0 must say `passed` with exactly
/// the action's own bounded fields. Anything else is [`Malformed`].
pub fn admit(status: i32, output: &[u8], run: &str, action: Action) -> Result<Receipt, Malformed> {
    check(output.len() <= MAX_RECEIPT, "vm_guest_receipt_size")?;
    let value = json::decode_limited(output, MAX_RECEIPT)
        .ok()
        .filter(|value| value.object().is_some())
        .ok_or(Malformed("invalid_vm_guest_receipt_json"))?;
    let bound = matches!(value.get("schemaVersion"), Some(Json::Integer(text)) if text == "1")
        && value.get("runId").and_then(Json::string) == Some(run)
        && value.get("action").and_then(Json::string) == Some(action.name());
    check(bound, "invalid_vm_guest_receipt")?;
    let common = ["schemaVersion", "runId", "action", "passed"];
    if status == 1 {
        let labels = ["phase", "context", "code", "exceptionClass"];
        let names: Vec<&str> = common.iter().chain(&labels).copied().collect();
        let codes: Vec<&str> = CODES.iter().map(|(_, code)| *code).collect();
        let lists: [&[&str]; 4] = [&PHASES, &CONTEXTS, &codes, &CLASSES];
        let known = |label: &str, list: &[&str]| {
            value.get(label).and_then(Json::string).is_some_and(|text| {
                list.contains(&text) || (label == "code" && text == "unclassified")
            })
        };
        let exact = matches!(value.get("passed"), Some(Json::Bool(false)))
            && keys(&value, &names)
            && labels
                .iter()
                .zip(lists)
                .all(|(label, list)| known(label, list));
        check(exact, "invalid_vm_guest_failure_receipt")?;
        let text = |label: &str| value.get(label).and_then(Json::string).unwrap_or_default();
        return Ok(Receipt::Failed(Diagnostic::labelled(
            text("phase"),
            text("context"),
            text("code"),
            text("exceptionClass"),
        )));
    }
    check(
        status == 0 && matches!(value.get("passed"), Some(Json::Bool(true))),
        "invalid_vm_guest_receipt",
    )?;
    let names: Vec<&str> = common.iter().chain(action.fields()).copied().collect();
    check(keys(&value, &names), "invalid_vm_guest_receipt_fields")?;
    let mut projection = json!({"action":action.name(),"passed":true});
    if action == Action::Snapshot {
        let valid = integer(value.get("configurationFiles"), 1, 64)
            && matches!(value.get("idempotent"), Some(Json::Bool(_)));
        check(valid, "invalid_vm_snapshot_receipt")?;
        projection["configurationFiles"] = value
            .get("configurationFiles")
            .and_then(Json::integer)
            .unwrap_or_default()
            .into();
        projection["idempotent"] = value
            .get("idempotent")
            .and_then(Json::boolean)
            .unwrap_or_default()
            .into();
        return Ok(Receipt::Passed(projection));
    }
    let counts = value
        .get("counts")
        .ok_or(Malformed("invalid_tool_record"))?;
    check(counts.object().is_some(), "invalid_tool_record")?;
    check(
        keys(counts, &model::COUNT_NAMES),
        "invalid_tool_record_fields",
    )?;
    let bounded = model::COUNT_NAMES
        .iter()
        .all(|name| integer(counts.get(name), 0, 16));
    check(bounded, "invalid_vm_fixture_counts")?;
    let mut published = serde_json::Map::new();
    for name in model::COUNT_NAMES {
        let number = counts.get(name).and_then(Json::integer).unwrap_or_default();
        published.insert(name.into(), number.into());
    }
    projection["counts"] = published.into();
    if action == Action::Database {
        let valid = integer(value.get("migrations"), 1, 10000)
            && matches!(value.get("ready"), Some(Json::Bool(true)));
        check(valid, "invalid_vm_database_receipt")?;
        projection["migrations"] = value
            .get("migrations")
            .and_then(Json::integer)
            .unwrap_or_default()
            .into();
        projection["ready"] = true.into();
    } else {
        let names = ["archiveValidated", "restoreVerified", "cleanupPassed"];
        let all = names
            .iter()
            .all(|name| matches!(value.get(name), Some(Json::Bool(true))));
        check(all, "invalid_vm_restore_receipt")?;
        for name in names {
            projection[name] = true.into();
        }
    }
    Ok(Receipt::Passed(projection))
}

/// The receipt the guest prints for `action` of `run`: the passed fields, or
/// the four labels of a failed check.
#[cfg(feature = "maintain-guest")]
pub(crate) fn receipt(run: &str, action: Action, outcome: &Result<Value, Diagnostic>) -> Value {
    let mut value = json!({"schemaVersion":1,"runId":run,"action":action.name()});
    match outcome {
        Ok(fields) => {
            value["passed"] = true.into();
            if let Some(fields) = fields.as_object() {
                for (name, field) in fields {
                    value[name] = field.clone();
                }
            }
        }
        Err(diagnostic) => {
            value["passed"] = false.into();
            if let Some(labels) = diagnostic.to_json().as_object() {
                for (name, label) in labels {
                    value[name] = label.clone();
                }
            }
        }
    }
    value
}

/// Check the Compose model `document` (`config --format json` of the public
/// project with its maintenance profile) as a snapshot does: the project, its
/// four services, and each service's user, isolation, capabilities, privilege
/// boundary, finite resources, network and init. Returns the number of
/// services, or the labels of the first failed check.
pub fn compose_model(document: &[u8]) -> Result<usize, Diagnostic> {
    let placed = |fault: model::Fault| fault.placed("snapshot_compose", "unspecified");
    let value = model::object(document).map_err(|failure| placed(failure.into()))?;
    model::compose(&value)
        .map(|(services, _)| services)
        .map_err(placed)
}
