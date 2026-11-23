//! The PostgreSQL edges of the launcher's native window.
//!
//! The PostgreSQL container runs its health probe as a Docker execution every
//! five seconds, with a three-second timeout, independently of the launcher.
//! The window's history check admits only whole executions, so each cut of the
//! window first waits, for a bounded time, until no probe the collector has
//! received is unfinished. The check itself is unchanged: a probe that a cut
//! still divides is refused. [`diagnose`] then describes every unexpected or
//! missing PostgreSQL event in a closed vocabulary that carries no identifier,
//! name, command text, image, path or environment value.

use super::super::{Control, host::events::PgReason, native};
use super::{Context, Error, Fixture, Result, policy};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

/// The longest either cut waits for an unfinished health probe. Docker ends a
/// probe after its three-second timeout, so only a daemon that stops reporting
/// keeps a history unquiet this long.
pub(super) const QUIET: Duration = Duration::from_secs(10);
/// The pause between two collector reads while a cut waits.
const POLL: Duration = Duration::from_millis(10);
/// What one read and one cut take after the opening deadline has passed.
const CUT: Duration = Duration::from_secs(1);
/// The described events of one record; further ones are only counted.
const SHOWN: usize = 8;
/// The collector's row bound, at which every count of a record saturates.
const ROWS: usize = 2048;
const SCOPE: &str = "MaintainLauncherPgHistoryDiagnostic";
const PHASES: [&str; 3] = ["exec_create", "exec_start", "exec_die"];

/// The Docker container actions a described event names; any other is `other`.
pub(super) const ACTIONS: [&str; 28] = [
    "attach",
    "commit",
    "copy",
    "create",
    "destroy",
    "detach",
    "die",
    "exec_create",
    "exec_detach",
    "exec_die",
    "exec_start",
    "export",
    "health_status",
    "kill",
    "oom",
    "pause",
    "rename",
    "resize",
    "restart",
    "start",
    "stop",
    "top",
    "unpause",
    "update",
    "prune",
    "archive-path",
    "extract-to-dir",
    "other",
];
/// What a described execution runs, by [`Command`].
pub(super) const COMMANDS: [&str; 3] = ["health", "launcher", "unlisted"];
/// The actor classes of a described event, by [`Actor`].
pub(super) const ACTORS: [&str; 3] = ["postgres", "fixture", "unknown"];
/// Where a described event was received, by [`Window`].
pub(super) const WINDOWS: [&str; 4] = ["before", "inside", "after", "unobserved"];
/// Why the check does not admit a described event, by [`Fault`].
pub(super) const FAULTS: [&str; 7] = [
    "missing",
    "unadmitted",
    "unlisted-command",
    "duplicate",
    "out-of-order",
    "nonzero-exit",
    "surplus",
];

/// What a described execution runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    /// The container's own inspected health probe.
    Health,
    /// A command the launcher's source makes the check expect.
    Launcher,
    /// Neither.
    Unlisted,
}
impl Command {
    fn name(self) -> &'static str {
        COMMANDS[self as usize]
    }
}

/// The actor of a described event, as the daemon observation classifies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Actor {
    Postgres,
    /// Another container the fixture created or joined.
    Fixture,
    Unknown,
}
impl Actor {
    fn name(self) -> &'static str {
        ACTORS[self as usize]
    }
}

/// Where the collector received a described event relative to the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Window {
    Before,
    Inside,
    After,
    /// Not received at all by the time of the record.
    Unobserved,
}
impl Window {
    fn name(self) -> &'static str {
        WINDOWS[self as usize]
    }
}

/// Why the check does not admit a described event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// An event an execution inside the window needs is not inside it.
    Missing,
    /// An event the check never admits on the PostgreSQL container.
    Unadmitted,
    /// The creation of an execution that is neither the probe nor expected.
    UnlistedCommand,
    /// A second creation of one execution.
    Duplicate,
    /// A creation, start or death out of its execution's order.
    OutOfOrder,
    /// A death whose exit code is not `"0"`.
    NonzeroExit,
    /// One more execution of an expected command than expected.
    Surplus,
}
impl Fault {
    fn name(self) -> &'static str {
        FAULTS[self as usize]
    }
}

/// One described event. `action` is always an element of [`ACTIONS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Finding {
    action: &'static str,
    command: Option<Command>,
    actor: Actor,
    window: Window,
    fault: Fault,
    /// The exit code of a [`Fault::NonzeroExit`] death when it is 1 to 255.
    exit: Option<u8>,
}
impl Finding {
    fn inside(action: &'static str, command: Option<Command>, fault: Fault) -> Self {
        Self {
            action,
            command,
            actor: Actor::Postgres,
            window: Window::Inside,
            fault,
            exit: None,
        }
    }
    fn value(self) -> Value {
        json!({"type":"container","action":self.action,
            "command":self.command.map(Command::name),"actor":self.actor.name(),
            "window":self.window.name(),"fault":self.fault.name(),"exitCode":self.exit})
    }
}

/// One execution as the window shows it.
struct Execution<'a> {
    /// The command text of its first start or creation inside the window.
    text: Option<&'a str>,
    command: Option<Command>,
    /// Creation, start and death received inside the window.
    phases: [bool; 3],
    /// Already described by a fault that the rest of its events follow from.
    refused: bool,
}

/// The rows around one native window and the identities the check joined.
pub(super) struct Edges<'a> {
    /// Every row the collector received before the opening cut.
    pub prefix: &'a [Value],
    /// The rows the check examined.
    pub window: &'a [Value],
    /// The rows the collector has received since the closing cut.
    pub suffix: &'a [Value],
    pub pg: &'a str,
    /// The inspected health probe's exact command text.
    pub health: &'a str,
    /// The containers the fixture created or joined.
    pub fixture: &'a BTreeSet<String>,
}

fn postgres(row: &Value, pg: &str) -> bool {
    row["Type"] == "container" && row["Actor"]["ID"] == pg
}
fn verb(action: &str) -> &str {
    action.split_once(": ").map_or(action, |(verb, _)| verb)
}
fn action_name(action: &str) -> &'static str {
    let verb = verb(action);
    ACTIONS
        .iter()
        .copied()
        .find(|name| *name == verb)
        .unwrap_or("other")
}
fn exit_code(value: &Value) -> Option<u8> {
    value
        .as_str()
        .and_then(|code| code.parse::<u8>().ok())
        .filter(|code| *code != 0)
}

/// Whether every health probe of `pg` created in `rows` has its `exec_die` in
/// `rows` too.
pub(super) fn quiet(rows: &[Value], pg: &str, health: &str) -> bool {
    let create = format!("exec_create: {health}");
    let mut open = BTreeSet::new();
    for row in rows.iter().filter(|row| postgres(row, pg)) {
        let Some(id) = row["Actor"]["Attributes"]["execID"].as_str() else {
            continue;
        };
        if row["Action"] == create.as_str() {
            open.insert(id);
        } else if row["Action"] == "exec_die" {
            open.remove(id);
        }
    }
    open.is_empty()
}

/// The latest opening cut: [`QUIET`] after `now`, and early enough that the
/// native run still finds [`native::RESERVED`] before `body_end`. A body
/// without that slack is cut at once.
pub(super) fn opening_deadline(now: Instant, body_end: Instant) -> Instant {
    let slack = body_end
        .saturating_duration_since(now)
        .saturating_sub(native::RESERVED + CUT);
    now + slack.min(QUIET)
}

/// Keep `record()` as the receipt's window record only when `result` is the
/// receipt's first failure: nothing failed earlier in this observation
/// (`earlier`) or since (`current`), and no record is kept yet. A record never
/// changes `result`.
pub(super) fn retain(
    slot: &mut Option<Value>,
    result: &Result<()>,
    earlier: Option<Error>,
    current: Option<Error>,
    record: impl FnOnce() -> Option<Value>,
) {
    if result.is_err() && earlier.is_none() && current.is_none() && slot.is_none() {
        *slot = record();
    }
}

impl Edges<'_> {
    fn command(&self, text: Option<&str>, commands: &BTreeMap<String, usize>) -> Command {
        match text {
            Some(text) if commands.contains_key(text) => Command::Launcher,
            Some(text) if text == self.health => Command::Health,
            _ => Command::Unlisted,
        }
    }
    fn actor(&self, row: &Value) -> Actor {
        let id = row["Actor"]["ID"].as_str();
        if id == Some(self.pg) {
            Actor::Postgres
        } else if id.is_some_and(|id| self.fixture.contains(id)) {
            Actor::Fixture
        } else {
            Actor::Unknown
        }
    }
    /// The first container row outside the window that `matches`, and where.
    fn locate(&self, matches: impl Fn(&Value) -> bool) -> (Window, Option<&Value>) {
        let matches = |row: &&Value| row["Type"] == "container" && matches(row);
        if let Some(row) = self.prefix.iter().find(matches) {
            (Window::Before, Some(row))
        } else if let Some(row) = self.suffix.iter().find(matches) {
            (Window::After, Some(row))
        } else {
            (Window::Unobserved, None)
        }
    }
    /// The described absence of one phase of execution `id` from the window.
    fn missing(
        &self,
        id: &str,
        phase: usize,
        execution: &Execution<'_>,
        commands: &BTreeMap<String, usize>,
    ) -> Finding {
        let action = PHASES[phase];
        let (window, row) = self.locate(|row| {
            row["Actor"]["Attributes"]["execID"] == id
                && row["Action"]
                    .as_str()
                    .is_some_and(|text| verb(text) == action)
        });
        let creation = || {
            let (_, row) = self.locate(|row| {
                row["Actor"]["Attributes"]["execID"] == id
                    && row["Action"]
                        .as_str()
                        .is_some_and(|text| verb(text) == PHASES[0])
            });
            row.map(|row| {
                self.command(
                    row["Action"]
                        .as_str()
                        .and_then(|text| text.strip_prefix("exec_create: ")),
                    commands,
                )
            })
        };
        Finding {
            action,
            command: execution.command.or_else(creation),
            actor: row.map_or(Actor::Postgres, |row| self.actor(row)),
            window,
            fault: Fault::Missing,
            exit: None,
        }
    }
}

/// Every unexpected or missing PostgreSQL event of the window, in the order
/// the check meets them, and how many PostgreSQL events the window holds.
fn findings(edges: &Edges<'_>, commands: &BTreeMap<String, usize>) -> (Vec<Finding>, usize) {
    let mut found = Vec::new();
    let mut executions: BTreeMap<&str, Execution<'_>> = BTreeMap::new();
    let mut order = Vec::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut events = 0;
    for row in edges.window.iter().filter(|row| postgres(row, edges.pg)) {
        events += 1;
        let Some(action) = row["Action"].as_str() else {
            found.push(Finding::inside("other", None, Fault::Unadmitted));
            continue;
        };
        if action == "health_status: healthy" {
            continue;
        }
        let name = action_name(action);
        let attributes = &row["Actor"]["Attributes"];
        let Some(id) = attributes["execID"]
            .as_str()
            .filter(|id| policy::digest(id))
        else {
            found.push(Finding::inside(name, None, Fault::Unadmitted));
            continue;
        };
        match name {
            "exec_create" => {
                let text = action.strip_prefix("exec_create: ");
                let command = edges.command(text, commands);
                if let Some(execution) = executions.get_mut(id) {
                    let fault = if execution.phases[0] {
                        Fault::Duplicate
                    } else {
                        Fault::OutOfOrder
                    };
                    execution.refused = true;
                    found.push(Finding::inside(name, Some(command), fault));
                    continue;
                }
                let fault = match (command, text) {
                    (Command::Launcher, Some(text)) => {
                        let count = counts.entry(text).or_default();
                        *count += 1;
                        (commands.get(text).is_none_or(|expected| *count > *expected))
                            .then_some(Fault::Surplus)
                    }
                    (Command::Health, _) => None,
                    _ => Some(Fault::UnlistedCommand),
                };
                if let Some(fault) = fault {
                    found.push(Finding::inside(name, Some(command), fault));
                }
                order.push(id);
                executions.insert(
                    id,
                    Execution {
                        text,
                        command: Some(command),
                        phases: [true, false, false],
                        refused: fault.is_some(),
                    },
                );
            }
            "exec_start" | "exec_die" => {
                let execution = executions.entry(id).or_insert_with(|| {
                    order.push(id);
                    Execution {
                        text: None,
                        command: None,
                        phases: [false; 3],
                        refused: false,
                    }
                });
                if execution.refused {
                    continue;
                }
                if name == "exec_start" {
                    let text = action.strip_prefix("exec_start: ");
                    if execution.command.is_none() {
                        let command = edges.command(text, commands);
                        if let (Command::Launcher, Some(text)) = (command, text) {
                            *counts.entry(text).or_default() += 1;
                        }
                        execution.command = Some(command);
                        execution.text = text;
                    }
                    let fault = if text != execution.text {
                        Some(Fault::Unadmitted)
                    } else if execution.phases[1] || execution.phases[2] {
                        Some(Fault::OutOfOrder)
                    } else {
                        None
                    };
                    if let Some(fault) = fault {
                        execution.refused = true;
                        found.push(Finding::inside(name, execution.command, fault));
                        continue;
                    }
                    execution.phases[1] = true;
                } else {
                    if execution.phases[2] || (execution.phases[0] && !execution.phases[1]) {
                        execution.refused = true;
                        found.push(Finding::inside(name, execution.command, Fault::OutOfOrder));
                        continue;
                    }
                    execution.phases[2] = true;
                    if attributes["exitCode"] != "0" {
                        let mut finding =
                            Finding::inside(name, execution.command, Fault::NonzeroExit);
                        finding.exit = exit_code(&attributes["exitCode"]);
                        found.push(finding);
                    }
                }
            }
            _ => found.push(Finding::inside(name, None, Fault::Unadmitted)),
        }
    }
    for id in order {
        let Some(execution) = executions.get(id).filter(|execution| !execution.refused) else {
            continue;
        };
        if let Some(phase) = (0..PHASES.len()).find(|phase| !execution.phases[*phase]) {
            found.push(edges.missing(id, phase, execution, commands));
        }
    }
    for (text, expected) in commands {
        let observed = counts.get(text.as_str()).copied().unwrap_or(0);
        let create = format!("exec_create: {text}");
        for _ in observed..*expected {
            let (window, row) = edges.locate(|row| row["Action"] == create.as_str());
            found.push(Finding {
                action: PHASES[0],
                command: Some(Command::Launcher),
                actor: row.map_or(Actor::Postgres, |row| edges.actor(row)),
                window,
                fault: Fault::Missing,
                exit: None,
            });
        }
    }
    (found, events)
}

/// The closed record of one refused window history.
///
/// `reason` is the check's own first refusal. `commands` is the check's
/// expected launcher commands, or `None` when their derivation was what
/// refused; no event is then described. Events are listed in the order the
/// check meets them, then every missing one; the first eight are described and
/// the rest counted. Counts saturate at the collector's 2,048-row bound.
pub(super) fn diagnose(
    edges: &Edges<'_>,
    commands: Option<&BTreeMap<String, usize>>,
    reason: Option<PgReason>,
) -> Value {
    let (found, events) = match commands {
        Some(commands) => findings(edges, commands),
        None => (
            Vec::new(),
            edges
                .window
                .iter()
                .filter(|row| postgres(row, edges.pg))
                .count(),
        ),
    };
    let shown = found
        .iter()
        .take(SHOWN)
        .map(|finding| finding.value())
        .collect::<Vec<_>>();
    json!({"schemaVersion":1,"scope":SCOPE,"reason":reason.map(PgReason::name),
        "postgresEvents":events.min(ROWS),"events":shown,
        "withheldEvents":found.len().saturating_sub(SHOWN).min(ROWS)})
}

impl Fixture {
    /// Open the launcher's native window by [`opening_deadline`]
    /// ([`Fixture::open_window`]). Daemon effects stay uncertain until the
    /// window's own observation clears them.
    pub async fn open_native_window(&mut self, ctx: &mut Context) -> Result<()> {
        let end = opening_deadline(Instant::now(), ctx.control.body_end());
        self.open_window(&mut ctx.control, end).await?;
        self.unknown_effect = true;
        Ok(())
    }
    /// Cut the collected history after every row received so far, once no
    /// health probe is unfinished, or at `end`. Reads the collector every ten
    /// milliseconds; a collector or clock failure ends the wait with that
    /// failure and no cut.
    pub(super) async fn open_window(&mut self, control: &mut Control, end: Instant) -> Result<()> {
        let pg = self.pg.clone().ok_or(Error::Refused)?;
        let health = self.pg_health.clone();
        loop {
            let opened = {
                let mut monitors = self.monitors.lock().map_err(|_| Error::Io)?;
                monitors.check(control)?;
                monitors
                    .collector
                    .as_mut()
                    .ok_or(Error::Refused)?
                    .boundary_when(control, |rows| {
                        Instant::now() >= end || quiet(rows, &pg, &health)
                    })?
            };
            if opened {
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
            control.check()?;
        }
    }
    /// Close the launcher's native window within [`QUIET`]
    /// ([`Fixture::close_window`]).
    pub(super) async fn close_native_window(&mut self, control: &mut Control) -> Result<()> {
        self.close_window(control, Instant::now() + QUIET).await
    }
    /// Close the native window: retain the rows received since the opening
    /// cut once no health probe created among them is unfinished, or at
    /// `end`. Reads the collector every ten milliseconds; a collector or clock
    /// failure ends the wait with that failure and retains nothing.
    pub(super) async fn close_window(&mut self, control: &mut Control, end: Instant) -> Result<()> {
        let pg = self.pg.clone().ok_or(Error::Refused)?;
        let health = self.pg_health.clone();
        loop {
            let closed = {
                let mut monitors = self.monitors.lock().map_err(|_| Error::Io)?;
                monitors.check(control)?;
                let collector = monitors.collector.as_ref().ok_or(Error::Refused)?;
                let window = collector
                    .rows
                    .get(collector.native_start..)
                    .ok_or(Error::Refused)?;
                (Instant::now() >= end || quiet(window, &pg, &health)).then(|| window.to_vec())
            };
            if let Some(rows) = closed {
                self.native_rows = rows;
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
            control.check()?;
        }
    }
    /// The record of a refused window history ([`diagnose`]), or `None` when
    /// the collector no longer holds the rows around the window.
    pub(super) fn pg_refusal(
        &self,
        commands: Option<&BTreeMap<String, usize>>,
        reason: Option<PgReason>,
    ) -> Option<Value> {
        let pg = self.pg.as_deref()?;
        let monitors = self.monitors.lock().ok()?;
        let collector = monitors.collector.as_ref()?;
        let start = collector.native_start;
        let prefix = collector.rows.get(..start)?;
        let suffix = collector
            .rows
            .get(start.checked_add(self.native_rows.len())?..)?;
        let fixture = self
            .actors
            .iter()
            .chain(&self.app)
            .chain(&self.proxy)
            .cloned()
            .collect::<BTreeSet<_>>();
        let edges = Edges {
            prefix,
            window: &self.native_rows,
            suffix,
            pg,
            health: &self.pg_health,
            fixture: &fixture,
        };
        Some(diagnose(&edges, commands, reason))
    }
}

#[cfg(test)]
#[path = "pg_window_tests.rs"]
mod tests;
