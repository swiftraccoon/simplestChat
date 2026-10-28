//! A separate finite event epoch for the complete prepared-host operation.
//! Destroyed transient actors are bound by their fixed command names, images,
//! labels and complete traces. Final service rows are independent inspections.

use super::super::{Error, Result, protocol::Request};
use super::{Baseline, require};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const PROJECT: &str = "simplestchat-public";
const LABEL: &str = "simplestchat.maintain-prerequisite";
const APP: &str = "simplestchat-public-simplestchat-1";
const PROXY: &str = "simplestchat-public-caddy-1";
const MIGRATE: &str = "simplestchat-public-migrate-1";
const LEDGER: &str = concat!(
    "SELECT version, success, encode(checksum, 'hex') ",
    "FROM public._sqlx_migrations ORDER BY version"
);
const NATIVE_DUMP: &str = concat!(
    "timeout --signal=TERM --kill-after=2s 45s pg_dump --host ",
    "/run/simplestchat-postgres --username postgres --dbname simplestchat --format custom"
);
const NATIVE_LIST: &str = "timeout --signal=TERM --kill-after=2s 15s pg_restore --list";

fn text(value: &Value) -> Result<&str> {
    value.as_str().ok_or(Error::Refused)
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn nonce_name(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|tail| {
        tail.len() == 32
            && tail
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}
fn signal(row: &Value) -> Result<&'static str> {
    match row["Config"]["StopSignal"].as_str() {
        None | Some("" | "SIGTERM" | "15") => Ok("15"),
        Some("SIGQUIT" | "3") => Ok("3"),
        _ => Err(Error::Refused),
    }
}
fn native_commands(success: bool) -> BTreeMap<String, usize> {
    let base = concat!(
        "psql --no-psqlrc --set ON_ERROR_STOP=on --host /run/simplestchat-postgres ",
        "--username postgres --dbname simplestchat"
    );
    BTreeMap::from([
        (
            format!("{base} --tuples-only --no-align --field-separator   --command {LEDGER}"),
            if success { 2 } else { 1 },
        ),
        (
            concat!(
                "psql -X -q -t -A -h /run/simplestchat-postgres -U postgres -d simplestchat ",
                "-c SELECT pg_database_size(current_database())"
            )
            .into(),
            1,
        ),
        (NATIVE_DUMP.into(), 1),
        (NATIVE_LIST.into(), 1),
    ])
}
fn completion(rows: &[Value], database: &str, command: &str) -> Result<usize> {
    let qualified = |row: &Value| row["Type"] == "container" && row["Actor"]["ID"] == database;
    let creates = rows
        .iter()
        .filter(|row| qualified(row) && row["Action"] == format!("exec_create: {command}"))
        .collect::<Vec<_>>();
    require(creates.len() == 1)?;
    let id = text(&creates[0]["Actor"]["Attributes"]["execID"])?;
    let deaths = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            qualified(row)
                && row["Action"] == "exec_die"
                && row["Actor"]["Attributes"]["execID"] == id
                && row["Actor"]["Attributes"]["exitCode"] == "0"
        })
        .map(|(at, _)| at)
        .collect::<Vec<_>>();
    require(deaths.len() == 1)?;
    Ok(deaths[0])
}
fn interruption_order(rows: &[Value], baseline: &Baseline) -> Result<()> {
    let first = |id: &str, actions: &[&str]| {
        rows.iter()
            .position(|row| {
                row["Type"] == "container"
                    && row["Actor"]["ID"] == id
                    && row["Action"]
                        .as_str()
                        .is_some_and(|action| actions.contains(&action))
            })
            .ok_or(Error::Refused)
    };
    let dump = completion(rows, &baseline.pg, NATIVE_DUMP)?;
    let list = completion(rows, &baseline.pg, NATIVE_LIST)?;
    let app_first = first(&baseline.app, &["kill", "die"])?;
    let app_die = first(&baseline.app, &["die"])?;
    let proxy_first = first(&baseline.proxy, &["kill", "die"])?;
    require(dump < list && list < app_first && app_die < proxy_first)
}
fn pg_history(
    rows: &[&Value],
    expected: &BTreeMap<String, usize>,
    health: &str,
    pending: &BTreeMap<String, u8>,
) -> Result<()> {
    require(
        pending.len() <= 1
            && pending
                .iter()
                .all(|(id, phase)| digest(id) && [1, 2].contains(phase)),
    )?;
    let mut executions: BTreeMap<String, (String, u8)> = pending
        .iter()
        .map(|(id, phase)| (id.clone(), (health.into(), *phase)))
        .collect();
    for row in rows {
        let action = text(&row["Action"])?;
        if action == "health_status: healthy" {
            continue;
        }
        let attrs = &row["Actor"]["Attributes"];
        let id = text(&attrs["execID"])?;
        require(digest(id))?;
        if let Some(command) = action.strip_prefix("exec_create: ") {
            require(
                !executions.contains_key(id)
                    && (expected.contains_key(command) || command == health),
            )?;
            executions.insert(id.into(), (command.into(), 1));
        } else {
            let (command, phase) = executions.get_mut(id).ok_or(Error::Refused)?;
            if action == format!("exec_start: {command}") {
                require(*phase == 1)?;
                *phase = 2;
            } else {
                require(action == "exec_die" && *phase == 2 && attrs["exitCode"] == "0")?;
                *phase = 3;
            }
        }
    }
    require(
        executions
            .values()
            .all(|(command, phase)| *phase == 3 || command == health && [1, 2].contains(phase))
            && executions.values().filter(|(_, phase)| *phase != 3).count() <= 1,
    )?;
    for (command, count) in expected {
        require(
            executions
                .values()
                .filter(|(actual, _)| actual == command)
                .count()
                == *count,
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    AppBefore,
    ProxyBefore,
    App,
    Proxy,
    Migration,
    NativeValidator,
    Asset,
    ShellValidator,
}
impl Kind {
    fn service(self) -> Option<&'static str> {
        match self {
            Self::AppBefore | Self::App => Some("simplestchat"),
            Self::ProxyBefore | Self::Proxy => Some("caddy"),
            Self::Migration => Some("migrate"),
            _ => None,
        }
    }
}
struct Actor {
    kind: Kind,
    name: String,
    created: String,
    phases: Vec<String>,
    killed: bool,
    stopped: bool,
    replaces: Option<String>,
}
impl Actor {
    fn baseline(kind: Kind, name: &str) -> Self {
        Self {
            kind,
            name: name.into(),
            created: name.into(),
            phases: Vec::new(),
            killed: false,
            stopped: false,
            replaces: None,
        }
    }
}
fn image(attrs: &Value, kind: Kind, baseline: &Baseline, request: &Request) -> Result<()> {
    require(
        if matches!(kind, Kind::ProxyBefore | Kind::Proxy | Kind::ShellValidator) {
            attrs["image"] == baseline.caddy_image || attrs["image"] == request.caddy_image
        } else {
            attrs["image"] == baseline.image
        },
    )
}
fn replacement(
    name: &str,
    canonical: &str,
    actors: &BTreeMap<String, Actor>,
    kind: Kind,
) -> Result<String> {
    let candidates = actors
        .iter()
        .filter(|(_, actor)| actor.kind.service() == kind.service())
        .filter(|(id, actor)| {
            name == format!("{}_{canonical}", &id[..12])
                || name == canonical && actor.phases.last().is_some_and(|phase| phase == "destroy")
        })
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    require(candidates.len() == 1)?;
    Ok(candidates[0].clone())
}
fn classify(
    attrs: &Value,
    baseline: &Baseline,
    request: &Request,
    actors: &BTreeMap<String, Actor>,
    success: bool,
) -> Result<Actor> {
    let name = text(&attrs["name"])?;
    let mut replaces = None;
    let kind = match attrs["com.docker.compose.service"].as_str() {
        Some("simplestchat") if success => {
            replaces = Some(replacement(name, APP, actors, Kind::App)?);
            Kind::App
        }
        Some("caddy") if success => {
            replaces = Some(replacement(name, PROXY, actors, Kind::Proxy)?);
            Kind::Proxy
        }
        Some("migrate") if success && name == MIGRATE => Kind::Migration,
        None if nonce_name(name, "scpub-release-validate-") => Kind::NativeValidator,
        None if nonce_name(name, "scpub-release-assets-") => Kind::Asset,
        None if success && baseline.shell_attempt.as_deref() == Some(name) => {
            let nonce = name
                .strip_prefix("scpub-")
                .and_then(|name| name.strip_suffix("-validate"))
                .ok_or(Error::Refused)?;
            require(
                nonce.len() == 32
                    && nonce
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    && attrs["simplestchat.public.validation"] == nonce,
            )?;
            Kind::ShellValidator
        }
        _ => return Err(Error::Refused),
    };
    if let Some(service) = kind.service() {
        require(
            attrs[LABEL] == request.run_id
                && attrs["com.docker.compose.project"] == PROJECT
                && attrs["com.docker.compose.service"] == service
                && attrs["com.docker.compose.container-number"] == "1",
        )?;
    } else {
        require(
            attrs["com.docker.compose.service"].is_null()
                && attrs["com.docker.compose.project"].is_null(),
        )?;
    }
    image(attrs, kind, baseline, request)?;
    Ok(Actor {
        kind,
        name: name.into(),
        created: name.into(),
        phases: Vec::new(),
        killed: false,
        stopped: false,
        replaces,
    })
}

pub(in super::super) fn validate(
    rows: &[Value],
    baseline: &Baseline,
    request: &Request,
    app: &Value,
    proxy: &Value,
) -> Result<()> {
    require(
        request.is_host()
            && rows.len() <= 2048
            && super::super::protocol::digest(&request.run_id, 32),
    )?;
    let success = request.scenario == "host-success";
    let final_app = text(&app["Id"])?;
    let final_proxy = text(&proxy["Id"])?;
    require(
        [
            baseline.pg.as_str(),
            baseline.app.as_str(),
            baseline.proxy.as_str(),
            baseline.network.as_str(),
            baseline.none_network.as_str(),
            final_app,
            final_proxy,
        ]
        .iter()
        .all(|id| digest(id)),
    )?;
    require(
        BTreeSet::from([&baseline.pg, &baseline.app, &baseline.proxy]).len() == 3
            && app["Image"] == baseline.image
            && proxy["Image"] == baseline.caddy_image
            && app["State"]["Running"] == success
            && proxy["State"]["Running"] == success,
    )?;
    if !success {
        require(final_app == baseline.app && final_proxy == baseline.proxy)?;
    }
    let mut actors = BTreeMap::from([
        (baseline.app.clone(), Actor::baseline(Kind::AppBefore, APP)),
        (
            baseline.proxy.clone(),
            Actor::baseline(Kind::ProxyBefore, PROXY),
        ),
    ]);
    let mut networks: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    let mut pg = Vec::new();
    let mut image_actions = BTreeSet::new();
    for row in rows {
        let action = text(&row["Action"])?;
        let id = text(&row["Actor"]["ID"])?;
        let attrs = &row["Actor"]["Attributes"];
        require(attrs.is_object())?;
        match text(&row["Type"])? {
            "image" => {
                require(
                    id == baseline.image
                        && ["load", "tag"].contains(&action)
                        && image_actions.insert(action.to_owned()),
                )?;
                if let Some(name) = attrs.get("name") {
                    require(
                        name == &baseline.image
                            || name
                                == &format!("simplestchat-release/production:{}", request.revision),
                    )?;
                }
            }
            "container" => {
                require(digest(id))?;
                if id == baseline.pg {
                    pg.push(row);
                    continue;
                }
                if !actors.contains_key(id) {
                    require(action == "create")?;
                    let actor = classify(attrs, baseline, request, &actors, success)?;
                    require(!actors.values().any(|known| known.created == actor.created))?;
                    actors.insert(id.into(), actor);
                }
                let actor = actors.get(id).ok_or(Error::Refused)?;
                image(attrs, actor.kind, baseline, request)?;
                if let Some(service) = actor.kind.service() {
                    require(
                        attrs[LABEL] == request.run_id
                            && attrs["com.docker.compose.project"] == PROJECT
                            && attrs["com.docker.compose.service"] == service
                            && attrs["com.docker.compose.container-number"] == "1",
                    )?;
                }
                if action == "rename" {
                    let canonical = match actor.kind {
                        Kind::App => APP,
                        Kind::Proxy => PROXY,
                        _ => return Err(Error::Refused),
                    };
                    require(
                        matches!(actor.kind, Kind::App | Kind::Proxy)
                            && actor.phases == ["create"]
                            && actor.created != canonical
                            && attrs["oldName"] == format!("/{}", actor.created)
                            && attrs["name"] == canonical
                            && actors
                                .get(actor.replaces.as_ref().ok_or(Error::Refused)?)
                                .is_some_and(|old| {
                                    old.phases.last().is_some_and(|phase| phase == "destroy")
                                }),
                    )?;
                    let actor = actors.get_mut(id).ok_or(Error::Refused)?;
                    actor.phases.push("rename".into());
                    actor.name = canonical.into();
                    continue;
                }
                require(attrs["name"] == actor.name)?;
                let expected_signal = if matches!(actor.kind, Kind::ProxyBefore | Kind::Proxy) {
                    signal(&baseline.proxy_row)?
                } else {
                    signal(&baseline.app_row)?
                };
                let actor = actors.get_mut(id).ok_or(Error::Refused)?;
                match action {
                    "create" => {
                        require(
                            actor.phases.is_empty()
                                && !matches!(actor.kind, Kind::AppBefore | Kind::ProxyBefore),
                        )?;
                        actor.phases.push(action.into());
                    }
                    "start" => {
                        let new = actor.phases == ["create"] && actor.created == actor.name
                            || actor.phases == ["create", "rename"];
                        let restarted = actor.kind == Kind::ProxyBefore && actor.phases == ["die"];
                        require(new || restarted)?;
                        actor.phases.push(action.into());
                    }
                    "kill" => {
                        require(
                            matches!(
                                actor.kind,
                                Kind::AppBefore
                                    | Kind::ProxyBefore
                                    | Kind::App
                                    | Kind::Proxy
                                    | Kind::Migration
                            ) && !actor.killed
                                && attrs["signal"] == expected_signal
                                && (actor.phases.last().is_some_and(|phase| phase == "start")
                                    || actor.phases.is_empty()),
                        )?;
                        actor.killed = true;
                    }
                    "die" => {
                        require(
                            (actor.phases.last().is_some_and(|phase| phase == "start")
                                || actor.phases.is_empty() && actor.killed)
                                && attrs["exitCode"] == "0",
                        )?;
                        actor.phases.push(action.into());
                    }
                    "stop" => {
                        require(
                            actor.killed
                                && !actor.stopped
                                && (actor
                                    .phases
                                    .last()
                                    .is_some_and(|phase| phase == "start" || phase == "die")
                                    || actor.phases.is_empty()),
                        )?;
                        actor.stopped = true;
                    }
                    "destroy" => {
                        require(
                            actor.phases.last().is_some_and(|phase| phase == "die")
                                || actor.kind == Kind::Asset && actor.phases == ["create"],
                        )?;
                        actor.phases.push(action.into());
                    }
                    _ => return Err(Error::Refused),
                }
            }
            "network" => {
                require(id == baseline.network || id == baseline.none_network)?;
                require(
                    attrs.as_object().is_some_and(|attrs| attrs.len() == 3)
                        && ["connect", "disconnect"].contains(&action),
                )?;
                let target = text(&attrs["container"])?;
                let actor = actors.get(target).ok_or(Error::Refused)?;
                if id == baseline.network {
                    require(
                        attrs["type"] == "bridge"
                            && attrs["name"] == format!("sc{}", &request.run_id[..12])
                            && matches!(
                                actor.kind,
                                Kind::AppBefore | Kind::ProxyBefore | Kind::App | Kind::Proxy
                            ),
                    )?;
                } else {
                    require(
                        attrs["type"] == "null"
                            && attrs["name"] == "none"
                            && matches!(
                                actor.kind,
                                Kind::NativeValidator
                                    | Kind::Asset
                                    | Kind::ShellValidator
                                    | Kind::Migration
                            ),
                    )?;
                }
                let edge = networks.entry((id.into(), target.into())).or_default();
                require(edge.len() < 2 && !edge.last().is_some_and(|previous| previous == action))?;
                edge.push(action.into());
            }
            _ => return Err(Error::Refused),
        }
    }
    let count = |kind| actors.values().filter(|actor| actor.kind == kind).count();
    if success {
        require(
            final_app != baseline.app
                && actors
                    .get(final_app)
                    .is_some_and(|actor| actor.kind == Kind::App)
                && actors
                    .get(final_proxy)
                    .is_some_and(|actor| matches!(actor.kind, Kind::ProxyBefore | Kind::Proxy)),
        )?;
    }
    require(
        count(Kind::NativeValidator) == 2
            && count(Kind::Asset) == 1
            && count(Kind::Migration) == usize::from(success)
            && count(Kind::ShellValidator) == usize::from(success)
            && count(Kind::App) == if success { 2 } else { 0 }
            && count(Kind::Proxy) == usize::from(success && final_proxy != baseline.proxy),
    )?;
    for (id, actor) in &actors {
        let phases = actor.phases.iter().map(String::as_str).collect::<Vec<_>>();
        let expected: &[&str] = match actor.kind {
            Kind::NativeValidator | Kind::ShellValidator | Kind::Migration => {
                &["create", "start", "die", "destroy"]
            }
            Kind::Asset => &["create", "destroy"],
            Kind::AppBefore => {
                if success {
                    &["die", "destroy"]
                } else {
                    &["die"]
                }
            }
            Kind::ProxyBefore if success && id == final_proxy => &["die", "start"],
            Kind::ProxyBefore => {
                if success {
                    &["die", "destroy"]
                } else {
                    &["die"]
                }
            }
            Kind::App | Kind::Proxy if id == final_app || id == final_proxy => {
                if actor.created == actor.name {
                    &["create", "start"]
                } else {
                    &["create", "rename", "start"]
                }
            }
            Kind::App | Kind::Proxy => {
                if actor.created == actor.name {
                    &["create", "start", "die", "destroy"]
                } else {
                    &["create", "rename", "start", "die", "destroy"]
                }
            }
        };
        require(phases == expected)?;
        if matches!(
            actor.kind,
            Kind::AppBefore | Kind::ProxyBefore | Kind::Migration
        ) || actor.kind == Kind::App && id != final_app
        {
            require(actor.killed && actor.stopped)?;
        }
        let key = if matches!(
            actor.kind,
            Kind::AppBefore | Kind::ProxyBefore | Kind::App | Kind::Proxy
        ) {
            &baseline.network
        } else {
            &baseline.none_network
        };
        let edges = networks
            .get(&(key.clone(), id.clone()))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let expected_edges: &[&str] = match actor.kind {
            Kind::AppBefore => &["disconnect"],
            Kind::ProxyBefore if success && id == final_proxy => &["disconnect", "connect"],
            Kind::ProxyBefore => &["disconnect"],
            Kind::App | Kind::Proxy if id == final_app || id == final_proxy => &["connect"],
            Kind::App
            | Kind::Proxy
            | Kind::NativeValidator
            | Kind::ShellValidator
            | Kind::Migration => &["connect", "disconnect"],
            Kind::Asset => {
                if edges.is_empty() {
                    &[]
                } else {
                    &["connect", "disconnect"]
                }
            }
        };
        require(edges.iter().map(String::as_str).collect::<Vec<_>>() == expected_edges)?;
    }
    let mut expected = native_commands(success);
    if success {
        for (command, count) in &baseline.shell_pg_commands {
            *expected.entry(command.clone()).or_default() += *count;
        }
    }
    pg_history(&pg, &expected, &baseline.health, &baseline.health_pending)?;
    interruption_order(rows, baseline)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn container(id: &str, name: &str, service: Option<&str>, action: &str) -> Value {
        let mut attrs = json!({"name":name,"image":format!("sha256:{}", "6".repeat(64)),
            "exitCode":"0","signal":"15"});
        if let Some(service) = service {
            attrs[LABEL] = "a".repeat(32).into();
            attrs["com.docker.compose.project"] = PROJECT.into();
            attrs["com.docker.compose.service"] = service.into();
            attrs["com.docker.compose.container-number"] = "1".into();
            if service == "caddy" {
                attrs["image"] = format!("sha256:{}", "7".repeat(64)).into();
                attrs["signal"] = "3".into();
            }
        }
        json!({"Type":"container","Action":action,"Actor":{"ID":id,"Attributes":attrs}})
    }
    fn network(id: &str, bridge: bool, action: &str) -> Value {
        json!({"Type":"network","Action":action,"Actor":{
            "ID":if bridge{"4".repeat(64)}else{"5".repeat(64)},
            "Attributes":{"container":id,"type":if bridge{"bridge"}else{"null"},
                "name":if bridge{format!("sc{}", "a".repeat(12))}else{"none".into()}}}})
    }
    fn stopped(rows: &mut Vec<Value>, id: &str, name: &str, service: &str) {
        for action in ["kill", "die", "stop"] {
            rows.push(container(id, name, Some(service), action));
        }
        rows.push(network(id, service != "migrate", "disconnect"));
    }
    fn whole_fixture(success: bool) -> (Vec<Value>, Baseline, Request, Value, Value) {
        let request: Request = serde_json::from_value(json!({
            "schemaVersion":1,"scope":"PreparedHostMaintenanceRequest",
            "runId":"a".repeat(32),"revision":"b".repeat(40),
            "scenario":if success{"host-success"}else{"host-launcher-refusal"},
            "setupSeconds":1560,"bodySeconds":3660,"endSeconds":3780,
            "binary":{"bytes":1,"sha256":"1".repeat(64)},
            "manifest":{"bytes":1,"sha256":"1".repeat(64)},
            "archive":{"bytes":1,"sha256":"1".repeat(64)},
            "sourceBundle":{"bytes":1,"sha256":"1".repeat(64)},
            "sourceTree":"b".repeat(40),"sourcePins":{},"helperPins":{},
            "postgresImage":"postgres","caddyImage":"caddy","engineVersion":"29.0.0"
        }))
        .unwrap();
        let app = json!({"Id":"2".repeat(64),"Image":format!("sha256:{}", "6".repeat(64)),
            "State":{"Running":success},"Config":{"StopSignal":"SIGTERM"}});
        let proxy = json!({"Id":"3".repeat(64),"Image":format!("sha256:{}", "7".repeat(64)),
            "State":{"Running":success},"Config":{"StopSignal":"SIGQUIT"}});
        let baseline = Baseline {
            pg: "1".repeat(64),
            app: "2".repeat(64),
            proxy: "3".repeat(64),
            image: format!("sha256:{}", "6".repeat(64)),
            caddy_image: format!("sha256:{}", "7".repeat(64)),
            network: "4".repeat(64),
            none_network: "5".repeat(64),
            health: "health probe".into(),
            health_pending: BTreeMap::new(),
            shell_pg_commands: BTreeMap::from([("shell pg".into(), 1)]),
            shell_attempt: Some(format!("scpub-{}-validate", "f".repeat(32))),
            app_row: app.clone(),
            proxy_row: proxy.clone(),
            attempts: BTreeSet::new(),
        };
        let mut rows = Vec::new();
        for nonce in ["8", "9"] {
            let id = nonce.repeat(64);
            let name = format!("scpub-release-validate-{}", nonce.repeat(32));
            rows.push(container(&id, &name, None, "create"));
            rows.push(network(&id, false, "connect"));
            rows.push(container(&id, &name, None, "start"));
            rows.push(container(&id, &name, None, "die"));
            rows.push(network(&id, false, "disconnect"));
            rows.push(container(&id, &name, None, "destroy"));
        }
        let asset = format!("scpub-release-assets-{}", "a".repeat(32));
        rows.push(container(&"a".repeat(64), &asset, None, "create"));
        rows.push(container(&"a".repeat(64), &asset, None, "destroy"));
        stopped(&mut rows, &baseline.app, APP, "simplestchat");
        stopped(&mut rows, &baseline.proxy, PROXY, "caddy");
        let mut final_app = app;
        if success {
            let shell = baseline.shell_attempt.as_deref().unwrap();
            for action in ["create", "start", "die", "destroy"] {
                let mut row = container(&"e".repeat(64), shell, None, action);
                row["Actor"]["Attributes"]["image"] = baseline.caddy_image.clone().into();
                row["Actor"]["Attributes"]["simplestchat.public.validation"] =
                    "f".repeat(32).into();
                rows.push(row);
                if action == "create" {
                    rows.push(network(&"e".repeat(64), false, "connect"));
                }
                if action == "die" {
                    rows.push(network(&"e".repeat(64), false, "disconnect"));
                }
            }
            let migration = "b".repeat(64);
            rows.push(container(&migration, MIGRATE, Some("migrate"), "create"));
            rows.push(network(&migration, false, "connect"));
            rows.push(container(&migration, MIGRATE, Some("migrate"), "start"));
            stopped(&mut rows, &migration, MIGRATE, "migrate");
            rows.push(container(&migration, MIGRATE, Some("migrate"), "destroy"));
            let mut previous = baseline.app.clone();
            for next in ["c".repeat(64), "d".repeat(64)] {
                let temporary = format!("{}_{APP}", &previous[..12]);
                rows.push(container(&next, &temporary, Some("simplestchat"), "create"));
                rows.push(container(&previous, APP, Some("simplestchat"), "destroy"));
                let mut rename = container(&next, APP, Some("simplestchat"), "rename");
                rename["Actor"]["Attributes"]["oldName"] = format!("/{temporary}").into();
                rows.push(rename);
                rows.push(container(&next, APP, Some("simplestchat"), "start"));
                rows.push(network(&next, true, "connect"));
                if next.starts_with('c') {
                    stopped(&mut rows, &next, APP, "simplestchat");
                }
                previous = next;
            }
            final_app["Id"] = previous.into();
            rows.push(container(&baseline.proxy, PROXY, Some("caddy"), "start"));
            rows.push(network(&baseline.proxy, true, "connect"));
        }
        let mut expected = native_commands(success);
        if success {
            expected.extend(baseline.shell_pg_commands.clone());
        }
        let mut serial = 32u64;
        let mut backup_rows = BTreeMap::new();
        for (command, count) in expected {
            for _ in 0..count {
                let exec_id = format!("{serial:064x}");
                serial += 1;
                let mut execution = Vec::new();
                for action in [
                    format!("exec_create: {command}"),
                    format!("exec_start: {command}"),
                    "exec_die".into(),
                ] {
                    let mut row = container(&baseline.pg, "postgres", None, &action);
                    row["Actor"]["Attributes"]["execID"] = exec_id.clone().into();
                    execution.push(row);
                }
                if [NATIVE_DUMP, NATIVE_LIST].contains(&command.as_str()) {
                    backup_rows.insert(command.clone(), execution);
                } else {
                    rows.extend(execution);
                }
            }
        }
        let backup = backup_rows
            .remove(NATIVE_DUMP)
            .unwrap()
            .into_iter()
            .chain(backup_rows.remove(NATIVE_LIST).unwrap());
        rows.splice(0..0, backup);
        (rows, baseline, request, final_app, proxy)
    }

    #[test]
    fn whole_host_epochs_close_both_fixed_scenarios_and_refuse_neighbor_effects() {
        for success in [true, false] {
            let (rows, baseline, request, app, proxy) = whole_fixture(success);
            assert!(validate(&rows, &baseline, &request, &app, &proxy).is_ok());
            for action in ["pause", "exec_create: unreviewed command", "destroy"] {
                let mut changed = rows.clone();
                changed.push(container(&baseline.app, APP, Some("simplestchat"), action));
                assert!(
                    validate(&changed, &baseline, &request, &app, &proxy).is_err(),
                    "{action}"
                );
            }
            for index in [0, 2, rows.len() - 1] {
                let mut missing = rows.clone();
                missing.remove(index);
                assert!(
                    validate(&missing, &baseline, &request, &app, &proxy).is_err(),
                    "{index}"
                );
            }
            let mut foreign = rows.clone();
            foreign.push(network(&"f".repeat(64), true, "connect"));
            assert!(validate(&foreign, &baseline, &request, &app, &proxy).is_err());
            let mut wrong_signal = rows.clone();
            let kill = wrong_signal
                .iter_mut()
                .find(|row| row["Action"] == "kill")
                .unwrap();
            kill["Actor"]["Attributes"]["signal"] = "9".into();
            assert!(validate(&wrong_signal, &baseline, &request, &app, &proxy).is_err());
        }
    }

    #[test]
    fn whole_epoch_orders_dump_list_application_and_proxy_interruptions() {
        for success in [true, false] {
            let (rows, baseline, request, app, proxy) = whole_fixture(success);
            assert!(validate(&rows, &baseline, &request, &app, &proxy).is_ok());
            let mut list_before_dump = rows.clone();
            for offset in 0..3 {
                list_before_dump.swap(offset, offset + 3);
            }
            assert!(validate(&list_before_dump, &baseline, &request, &app, &proxy).is_err());
            let mut late_backup = rows.clone();
            let backup = late_backup.drain(..6).collect::<Vec<_>>();
            late_backup.extend(backup);
            assert!(validate(&late_backup, &baseline, &request, &app, &proxy).is_err());
            let first = |id: &str| {
                rows.iter()
                    .position(|row| row["Action"] == "kill" && row["Actor"]["ID"] == id)
                    .unwrap()
            };
            let app_at = first(&baseline.app);
            let proxy_at = first(&baseline.proxy);
            let mut proxy_before_app = rows.clone();
            for offset in 0..4 {
                proxy_before_app.swap(app_at + offset, proxy_at + offset);
            }
            assert!(validate(&proxy_before_app, &baseline, &request, &app, &proxy).is_err());
            // Docker may publish the successful stop notification after the die.
            // The independently observed death, rather than stop notification,
            // orders the proxy interruption.
            let mut late_stop_notification = rows;
            let stop = late_stop_notification.remove(app_at + 2);
            late_stop_notification.insert(proxy_at + 1, stop);
            assert!(validate(&late_stop_notification, &baseline, &request, &app, &proxy).is_ok());
        }
    }

    #[test]
    fn native_pg_commands_have_only_the_expected_pre_and_post_ledger_counts() {
        let success = native_commands(true);
        let refusal = native_commands(false);
        assert_eq!(success.len(), 4);
        assert_eq!(refusal.len(), 4);
        for (command, count) in &refusal {
            assert_eq!(
                success.get(command),
                Some(&if command.contains("_sqlx_migrations") {
                    2
                } else {
                    *count
                })
            );
        }
        assert!(
            refusal
                .keys()
                .any(|command| command.ends_with("15s pg_restore --list"))
        );
    }
    #[test]
    fn pg_history_requires_exact_triplets_and_rejects_unknown_or_failed_execs() {
        let command = "known bounded command";
        let expected = BTreeMap::from([(command.into(), 1)]);
        let id = "a".repeat(64);
        let row = |action: &str| json!({"Action":action,"Actor":{"Attributes":{"execID":id,"exitCode":"0"}}});
        let rows = [
            row(&format!("exec_create: {command}")),
            row(&format!("exec_start: {command}")),
            row("exec_die"),
        ];
        let pending = BTreeMap::new();
        assert!(
            pg_history(
                &rows.iter().collect::<Vec<_>>(),
                &expected,
                "health",
                &pending
            )
            .is_ok()
        );
        assert!(
            pg_history(
                &rows[..2].iter().collect::<Vec<_>>(),
                &expected,
                "health",
                &pending
            )
            .is_err()
        );
        let mut failed = rows.clone();
        failed[2]["Actor"]["Attributes"]["exitCode"] = "1".into();
        assert!(
            pg_history(
                &failed.iter().collect::<Vec<_>>(),
                &expected,
                "health",
                &pending
            )
            .is_err()
        );
        assert!(
            pg_history(
                &rows.iter().collect::<Vec<_>>(),
                &BTreeMap::new(),
                "health",
                &pending
            )
            .is_err()
        );
    }
    #[test]
    fn signals_and_transient_names_are_finite_and_source_specific() {
        assert_eq!(signal(&json!({"Config":{"StopSignal":"SIGQUIT"}})), Ok("3"));
        assert_eq!(signal(&json!({"Config":{}})), Ok("15"));
        assert!(signal(&json!({"Config":{"StopSignal":"SIGKILL"}})).is_err());
        assert!(nonce_name(
            &format!("scpub-release-validate-{}", "a".repeat(32)),
            "scpub-release-validate-"
        ));
        assert!(!nonce_name(
            "scpub-release-validate-foreign",
            "scpub-release-validate-"
        ));
    }
    #[test]
    fn health_epoch_edges_require_prior_custody_and_keep_tested_commands_complete() {
        let id = "a".repeat(64);
        let row = |action: &str| json!({"Action":action,"Actor":{"Attributes":{"execID":id,"exitCode":"0"}}});
        let none = BTreeMap::new();
        let no_pending = BTreeMap::new();
        let started = BTreeMap::from([(id.clone(), 2)]);
        let die = row("exec_die");
        assert!(pg_history(&[&die], &none, "health", &started).is_ok());
        assert!(pg_history(&[&die], &none, "health", &no_pending).is_err());
        let trailing = row("exec_create: health");
        assert!(pg_history(&[&trailing], &none, "health", &no_pending).is_ok());
        assert!(
            pg_history(
                &[&trailing],
                &BTreeMap::from([("tested".into(), 1)]),
                "health",
                &no_pending,
            )
            .is_err()
        );
        let two_pending = BTreeMap::from([(id, 1), ("b".repeat(64), 2)]);
        assert!(pg_history(&[], &none, "health", &two_pending).is_err());
        let invalid = BTreeMap::from([("c".repeat(64), 3)]);
        assert!(pg_history(&[], &none, "health", &invalid).is_err());
    }
}
