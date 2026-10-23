//! Plain exact-ID retirement. No force, volumes, foreign names or unproved absence.
use super::{
    failure, files,
    io::Role,
    journal::{Journal, Owned},
    plan, policy,
};
use crate::{
    ci::{
        container_engine::{self, Clock, Core},
        process::Executor,
    },
    tooling::{Failure, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
pub(super) fn first(slot: &mut Option<Failure>, result: Result<()>) {
    if slot.is_none()
        && let Err(e) = result
    {
        *slot = Some(failure(e.code));
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Inspection {
    id: String,
    name: String,
    image: String,
    configured_image: String,
    run: String,
    role: String,
    network: String,
    state: State,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "PascalCase")]
struct State {
    running: bool,
    restarting: bool,
    #[serde(rename = "OOMKilled")]
    oom_killed: bool,
    exit_code: i64,
    error_empty: bool,
    pid: u32,
    started_at: String,
}
pub(super) fn inspect(
    bytes: &[u8],
    j: &Journal,
    role: Role,
    id: &str,
    server: Option<&str>,
) -> Result<Value> {
    let parsed: Inspection = serde_json::from_slice(bytes).map_err(|_| failure(1))?;
    if parsed.state.started_at.len() > 128
        || parsed.state.started_at.chars().any(char::is_control)
        || chrono::DateTime::parse_from_rfc3339(&parsed.state.started_at).is_err()
    {
        return Err(failure(1));
    }
    let v = serde_json::to_value(parsed).map_err(|_| failure(1))?;
    let network = if role == Role::Server {
        "none".into()
    } else {
        format!("container:{}", server.ok_or_else(|| failure(1))?)
    };
    if !policy::ownership(&v, id, j.image(role), &j.run_id, role.name())
        || v.get("name").and_then(Value::as_str)
            != Some(&format!("/{}", plan::name(&j.run_id, role)))
        || v.get("configuredImage").and_then(Value::as_str) != Some(j.image(role))
        || v.get("network").and_then(Value::as_str) != Some(&network)
    {
        return Err(failure(1));
    }
    Ok(v)
}
pub(super) async fn observed<E: Executor, C: Clock>(
    core: &mut Core<'_, E, C>,
    j: &Journal,
    owned: &Owned,
    server: Option<&str>,
    cleanup: bool,
) -> Result<Value> {
    let id = owned.id()?;
    let b = core
        .run(plan::command(
            core.endpoint,
            &["inspect", "--format", plan::INSPECT, id],
            cleanup,
        ))
        .await?;
    inspect(&b, j, owned.role, id, server)
}
fn state(v: &Value) -> Result<&Value> {
    v.get("state")
        .filter(|v| v.is_object())
        .ok_or_else(|| failure(1))
}
fn terminal(v: &Value) -> bool {
    state(v).is_ok_and(|s| {
        s.get("Running") == Some(&Value::Bool(false))
            && s.get("Restarting") == Some(&Value::Bool(false))
    })
}
pub(super) fn exited(v: &Value) -> Result<()> {
    let s = state(v)?;
    if s.get("ExitCode").and_then(Value::as_i64).is_none() {
        return Err(failure(1));
    }
    let selected = serde_json::json!({"Running":s.get("Running"),"Restarting":s.get("Restarting"),
        "OOMKilled":s.get("OOMKilled"),"ExitCode":s.get("ExitCode"),
        "Error":if s.get("ErrorEmpty")==Some(&Value::Bool(true)){Value::String(String::new())}
            else{Value::Null}});
    if policy::exit_passed(&selected) {
        Ok(())
    } else {
        Err(failure(1))
    }
}
async fn listed<E: Executor, C: Clock>(
    core: &mut Core<'_, E, C>,
    filter: &str,
) -> Result<Option<String>> {
    let b = core
        .run(plan::command(
            core.endpoint,
            &[
                "container",
                "ls",
                "--all",
                "--no-trunc",
                "--filter",
                filter,
                "--format",
                "{{.ID}}",
            ],
            true,
        ))
        .await?;
    if b.is_empty() {
        return Ok(None);
    }
    let id = container_engine::line(&b, container_engine::Scope::Benchmark)?;
    if !container_engine::container_id(id) {
        return Err(failure(1));
    }
    Ok(Some(id.into()))
}
pub(super) struct Evidence<'a> {
    pub output: &'a files::Output,
    pub spool: &'a crate::ci::release_controller::files::Directory,
    pub budget: &'a super::io::Budget,
    pub token: &'a str,
}
pub(super) async fn retire<E: Executor, C: Clock>(
    core: &mut Core<'_, E, C>,
    j: &Journal,
    evidence: &Evidence<'_>,
    owned: &mut Owned,
    server: Option<&str>,
) -> Result<()> {
    let output = evidence.output;
    let spool = evidence.spool;
    let budget = evidence.budget;
    let token = evidence.token;
    if !owned.attempted {
        return Ok(());
    }
    let name = plan::name(&j.run_id, owned.role);
    let by_name = listed(core, &format!("name=^/{name}$")).await?;
    let by_id = match &owned.id {
        Some(id) => listed(core, &format!("id={id}")).await?,
        None => None,
    };
    let Some(id) = by_name else {
        if by_id.is_none() && owned.removed && owned.id.is_some() {
            return Ok(());
        }
        return Err(failure(1)); // lost create response is not absence
    };
    if owned.id.as_ref().is_some_and(|old| old != &id)
        || (owned.id.is_some() && by_id.as_deref() != Some(id.as_str()))
    {
        return Err(failure(1));
    }
    owned.id = Some(id.clone());
    // Recovered exact identity is recorded only after complete authoritative inspection.
    let before = observed(core, j, owned, server, true).await?;
    let mut error = None;
    first(
        &mut error,
        output.bytes(
            &format!("{}.cid", owned.role.name()),
            format!("{id}\n").as_bytes(),
        ),
    );
    if !terminal(&before) {
        let (grace, body) = if owned.role == Role::Server {
            ("30", 33)
        } else {
            ("5", 8)
        };
        let mut stop = plan::command(core.endpoint, &["stop", "--time", grace, &id], true);
        stop.timeout = Duration::from_secs(body);
        let stopped = core.run(stop).await;
        if let Err(e) = stopped {
            first(&mut error, Err(e));
            let kill = core
                .run(plan::command(
                    core.endpoint,
                    &["kill", "--signal", "KILL", &id],
                    true,
                ))
                .await;
            first(&mut error, kill.map(|_| ()));
        }
    }
    let logs = files::logs(core, spool, budget, &id, token).await;
    match logs {
        Ok(bytes) => first(
            &mut error,
            output.bytes(&format!("{}.log", owned.role.name()), &bytes),
        ),
        Err(e) => first(&mut error, Err(e)),
    }
    let after = observed(core, j, owned, server, true).await;
    let removable = after.as_ref().is_ok_and(terminal);
    match after {
        Ok(v) => first(
            &mut error,
            output.json(&format!("{}.final-state.json", owned.role.name()), &v),
        ),
        Err(e) => first(&mut error, Err(e)),
    }
    if !removable {
        first(&mut error, Err(failure(1)));
    }
    if removable {
        let remove = core
            .run(plan::command(core.endpoint, &["rm", &id], true))
            .await;
        if remove.is_ok() {
            owned.removed = true;
            first(
                &mut error,
                output.bytes(
                    &format!("{}.removed", owned.role.name()),
                    format!("{id}\n").as_bytes(),
                ),
            );
        }
        first(&mut error, remove.map(|_| ()));
        let id_absent = listed(core, &format!("id={id}")).await;
        first(
            &mut error,
            id_absent.and_then(|v| if v.is_none() { Ok(()) } else { Err(failure(1)) }),
        );
        let name_absent = listed(core, &format!("name=^/{name}$")).await;
        first(
            &mut error,
            name_absent.and_then(|v| if v.is_none() { Ok(()) } else { Err(failure(1)) }),
        );
    }
    error.map_or(Ok(()), Err)
}
