//! One Signals/Native owner, serial sampling, and all-attempt exact retirement.
use super::{
    cleanup::{self, first},
    config::Settings,
    failure,
    files::{self, Capture, Guard, Output, Paths, Token},
    io::{Budget, Endpoint, Groups, Namespace, Observations, Process, Role},
    journal::{Journal, Owned},
    plan::{self, Time},
    policy,
};
use crate::{
    ci::{
        command::Command,
        container_engine::{self, Clock, Core, Policy, Scope},
        process::Executor,
    },
    tooling::{Environment, Result},
};
use serde_json::{Value, json};
use std::{ffi::OsStr, time::Duration};
pub(super) struct Context<'a, O: Observations> {
    pub obs: &'a O,
    pub guard: &'a Guard,
    pub paths: &'a Paths,
    pub budget: &'a Budget,
    pub time: &'a Time,
    pub token: &'a str,
}
struct Work<'a, O: Observations> {
    owned: &'a mut [Owned; 2],
    groups: &'a mut Option<O::Groups>,
    token: &'a mut Token,
    phase: &'a mut &'static str,
}
enum Probe {
    Bytes(Vec<u8>),
    Unavailable(crate::tooling::Failure),
}
impl Probe {
    fn required(self) -> Result<Vec<u8>> {
        match self {
            Self::Bytes(bytes) => Ok(bytes),
            Self::Unavailable(error) => Err(error),
        }
    }
}
fn finish_probe(
    response: Result<Vec<u8>>,
    retryable: bool,
    post: Result<()>,
    token: Result<()>,
    namespace: Result<()>,
) -> Result<Probe> {
    let complete = post.is_ok() && token.is_ok() && namespace.is_ok();
    match response {
        Err(e) if retryable && complete => Ok(Probe::Unavailable(e)),
        Err(e) => Err(e),
        Ok(bytes) => {
            post?;
            token?;
            namespace?;
            Ok(Probe::Bytes(bytes))
        }
    }
}
fn line(bytes: &[u8]) -> Result<&str> {
    container_engine::line(bytes, Scope::Benchmark)
}
fn process(v: &Value) -> Result<Process> {
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| container_engine::container_id(s))
        .ok_or_else(|| failure(1))?;
    let state = v.get("state").ok_or_else(|| failure(1))?;
    let pid = state
        .get("Pid")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 1)
        .ok_or_else(|| failure(1))?;
    let started_at = state
        .get("StartedAt")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 128 && !s.chars().any(char::is_control))
        .ok_or_else(|| failure(1))?;
    if state.get("Running") != Some(&Value::Bool(true)) {
        return Err(failure(1));
    }
    Ok(Process {
        id: id.into(),
        pid,
        started_at: started_at.into(),
    })
}
fn stopped(v: &Value) -> bool {
    v.pointer("/state/Running") == Some(&Value::Bool(false))
        && v.pointer("/state/Restarting") == Some(&Value::Bool(false))
}
async fn pending<E: Executor>(runner: &mut E, time: &Time, cleanup: bool) -> Result<()> {
    if !cleanup {
        runner.ready().await?;
    }
    time.fence(cleanup)
}
async fn endpoint<E: Executor>(environment: &Environment, runner: &mut E) -> Result<String> {
    // Original benchmark gives nonempty DOCKER_HOST precedence over CONTEXT.
    if let Some(host) = environment
        .get(OsStr::new("DOCKER_HOST"))
        .filter(|v| !v.is_empty())
    {
        return container_engine::endpoint(
            host.to_str().ok_or_else(|| failure(2))?,
            Scope::Benchmark,
        );
    }
    let mut c = Command::capture(
        crate::ci::command::Program::Docker,
        &[
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ],
    );
    c.capture = Some(4096);
    c.timeout = Duration::from_secs(5);
    c.grace = Duration::from_secs(1);
    let b = runner.run(c).await?;
    container_engine::endpoint(line(&b)?, Scope::Benchmark)
}
async fn images<E: Executor, C: Clock>(
    core: &mut Core<'_, E, C>,
    settings: &Settings,
) -> Result<()> {
    for image in [&settings.server_image, &settings.generator_image] {
        let id = core
            .run(plan::command(
                core.endpoint,
                &["image", "inspect", "--format", "{{.Id}}", image],
                false,
            ))
            .await?;
        if line(&id)? != image {
            return Err(failure(1));
        }
        let user = core
            .run(plan::command(
                core.endpoint,
                &["image", "inspect", "--format", "{{.Config.User}}", image],
                false,
            ))
            .await?;
        if line(&user)? != "10001:10001" {
            return Err(failure(1));
        }
        let env = core
            .run(plan::command(
                core.endpoint,
                &[
                    "image",
                    "inspect",
                    "--format",
                    "{{json .Config.Env}}",
                    image,
                ],
                false,
            ))
            .await?;
        let env: Vec<String> = serde_json::from_slice(&env).map_err(|_| failure(1))?;
        if env.iter().any(|v| {
            [
                "DATABASE_URL",
                "JWT_SECRET",
                "TURN_URLS",
                "TURN_SECRET",
                "DIAGNOSTICS_PATH",
                "MEDIA_DIAGNOSTICS_ENABLED",
            ]
            .iter()
            .any(|k| v.starts_with(&format!("{k}=")))
        }) {
            return Err(failure(1));
        }
    }
    let server = core
        .run(plan::command(
            core.endpoint,
            &[
                "image",
                "inspect",
                "--format",
                "{{json .Config}}",
                &settings.server_image,
            ],
            false,
        ))
        .await?;
    let s: Value = serde_json::from_slice(&server).map_err(|_| failure(1))?;
    if s.get("Cmd") != Some(&json!(["/app/simplestChat"]))
        || s.get("Entrypoint")
            .is_some_and(|v| !v.is_null() && v != &json!([]))
    {
        return Err(failure(1));
    }
    let generator = core
        .run(plan::command(
            core.endpoint,
            &[
                "image",
                "inspect",
                "--format",
                "{{json .Config.Entrypoint}}",
                &settings.generator_image,
            ],
            false,
        ))
        .await?;
    if serde_json::from_slice::<Value>(&generator).map_err(|_| failure(1))?
        != json!(["/app/load_test"])
    {
        return Err(failure(1));
    }
    Ok(())
}
async fn request<E: Executor, C: Clock, O: Observations>(
    core: &mut Core<'_, E, C>,
    obs: &O,
    j: &Journal,
    server: &Owned,
    token: &mut Token,
    which: Endpoint,
    time: &Time,
) -> Result<Probe> {
    let before = cleanup::observed(core, j, server, None, false).await?;
    let p = process(&before)?;
    let lease = obs.namespace(&p)?;
    let result = async {
        lease.check(&p)?;
        let after = cleanup::observed(core, j, server, None, false).await?;
        if process(&after)? != p {
            return Err(failure(1));
        }
        lease.check(&p)?;
        let c = obs.curl(&lease, which, token.input()?, Duration::from_secs(4), false)?;
        let proof = c.benchmark.clone();
        let response = core.run(c).await;
        let retryable = response.as_ref().is_err_and(|e| {
            proof.as_ref().is_some_and(|p| p.retryable(e.code)) && core.runner.cleanup_passed()
        });
        let pending = pending(core.runner, time, false).await;
        let binding = lease.check(&p);
        Ok((response, retryable, pending.and(binding)))
    }
    .await;
    let checked = token.check();
    let closed = lease.finish();
    let (response, retryable, post) = result?;
    finish_probe(response, retryable, post, checked, closed)
}
async fn create<E: Executor, C: Clock>(
    core: &mut Core<'_, E, C>,
    s: &Settings,
    j: &Journal,
    output: &Output,
    owned: &mut Owned,
    server: &str,
) -> Result<()> {
    output.mark(owned.role)?;
    owned.attempted = true;
    let bytes = core
        .run(s.create(core.endpoint, &j.run_id, owned.role, server))
        .await?;
    let id = line(&bytes)?;
    if !container_engine::container_id(id) {
        return Err(failure(1));
    }
    owned.id = Some(id.into());
    output.bytes(
        &format!("{}.cid", owned.role.name()),
        format!("{id}\n").as_bytes(),
    )?;
    core.run(plan::command(core.endpoint, &["start", id], false))
        .await?;
    Ok(())
}
fn snapshot<O: Observations>(obs: &O, output: &Output, phase: &str) -> Result<()> {
    for (name, bytes) in obs.snapshot()? {
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        {
            return Err(failure(1));
        }
        output.bytes(&format!("host-{phase}-{name}"), &bytes)?;
    }
    Ok(())
}
fn admitted_stats(bytes: &[u8], j: &Journal, owned: &[Owned; 2]) -> Result<Vec<u8>> {
    let mut rows = Vec::new();
    let mut seen = [false; 2];
    for value in serde_json::Deserializer::from_slice(bytes).into_iter::<Value>() {
        let value = value.map_err(|_| failure(1))?;
        let name = value
            .get("Name")
            .and_then(Value::as_str)
            .ok_or_else(|| failure(1))?;
        let role = Role::ALL
            .into_iter()
            .find(|r| name == plan::name(&j.run_id, *r))
            .ok_or_else(|| failure(1))?;
        if seen[role.index()] {
            return Err(failure(1));
        }
        seen[role.index()] = true;
        let id = owned[role.index()].id()?;
        let short = value
            .get("ID")
            .and_then(Value::as_str)
            .ok_or_else(|| failure(1))?;
        if !matches!(short.len(), 12 | 64) || !id.starts_with(short) {
            return Err(failure(1));
        }
        let mut row = serde_json::Map::new();
        row.insert("role".into(), json!(role.name()));
        row.insert("id".into(), json!(id));
        for key in ["CPUPerc", "MemUsage", "MemPerc", "NetIO", "BlockIO", "PIDs"] {
            let v = value
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| s.len() <= 512 && !s.chars().any(char::is_control))
                .ok_or_else(|| failure(1))?;
            row.insert(key.into(), json!(v));
        }
        rows.extend_from_slice(&serde_json::to_vec(&row).map_err(|_| failure(1))?);
        rows.push(b'\n');
    }
    if seen != [true, true] {
        return Err(failure(1));
    }
    Ok(rows)
}
async fn body<E: Executor, C: Clock, O: Observations>(
    core: &mut Core<'_, E, C>,
    s: &Settings,
    j: &Journal,
    output: &Output,
    work: Work<'_, O>,
    context: &Context<'_, O>,
) -> Result<()> {
    let Work {
        owned,
        groups,
        token,
        phase,
    } = work;
    let obs = context.obs;
    let time = context.time;
    output.json("configuration.json", s)?;
    snapshot(obs, output, "before")?;
    *phase = "server_startup";
    create(core, s, j, output, &mut owned[0], "").await?;
    let ready_start = time.elapsed();
    loop {
        let response = request(core, obs, j, &owned[0], token, Endpoint::Ready, time).await?;
        let elapsed = time.elapsed().saturating_sub(ready_start);
        if elapsed >= Duration::from_secs(60) {
            return Err(failure(124));
        }
        match response {
            Probe::Bytes(b) => {
                let v: Value = serde_json::from_slice(&b).map_err(|_| failure(1))?;
                if v.get("status").and_then(Value::as_str) != Some("ready") {
                    return Err(failure(1));
                }
                output.bytes("readiness.json", b"{\"status\":\"ready\"}\n")?;
                break;
            }
            Probe::Unavailable(_) => {
                pending(core.runner, time, false).await?;
                core.clock.sleep(Duration::from_millis(500)).await;
            }
        }
    }
    let before = request(core, obs, j, &owned[0], token, Endpoint::Metrics, time)
        .await?
        .required()?;
    output.bytes("metrics-before.txt", &token.safe(&before))?;
    *phase = "generator";
    // The two frozen create/start calls each include one info-ID check:
    // 2 * (5+1+20 + 8+1+20) =110s before the selected measurement.
    time.measurement(Duration::from_secs(u64::from(
        s.ramp_up + s.warmup + s.duration,
    )))?;
    let server = owned[0].id()?.to_owned();
    create(core, s, j, output, &mut owned[1], &server).await?;
    let p0 = process(&cleanup::observed(core, j, &owned[0], None, false).await?)?;
    let p1 = process(&cleanup::observed(core, j, &owned[1], Some(&server), false).await?)?;
    *groups = Some(obs.groups(&[p0, p1])?);
    let wait_start = time.elapsed();
    let mut samples = Vec::new();
    let mut total = 0;
    loop {
        pending(core.runner, time, false).await?;
        if time.elapsed().saturating_sub(wait_start) >= s.wait() {
            return Err(failure(124));
        }
        let g = cleanup::observed(core, j, &owned[1], Some(&server), false).await?;
        if time.elapsed().saturating_sub(wait_start) >= s.wait() {
            return Err(failure(124));
        }
        let terminal = stopped(&g);
        let sample = groups
            .as_mut()
            .unwrap()
            .sample(&time.utc(), [false, terminal])?;
        for value in sample {
            let b = serde_json::to_vec(&value).map_err(|_| failure(1))?;
            total += b.len() + 1;
            if total > 64 * 1024 * 1024 || samples.len() >= 1024 {
                return Err(failure(1));
            }
            samples.push(value);
        }
        if time.elapsed().saturating_sub(wait_start) >= s.wait() {
            return Err(failure(124));
        }
        if terminal {
            let waited = core
                .run(plan::command(
                    core.endpoint,
                    &["wait", owned[1].id()?],
                    false,
                ))
                .await?;
            if time.elapsed().saturating_sub(wait_start) >= s.wait() {
                return Err(failure(124));
            }
            if line(&waited)? != "0" {
                return Err(failure(1));
            }
            cleanup::exited(&g)?;
            break;
        }
        let stats = core
            .run(plan::command(
                core.endpoint,
                &[
                    "stats",
                    "--no-stream",
                    "--format",
                    "{{json .}}",
                    &server,
                    owned[1].id()?,
                ],
                false,
            ))
            .await?;
        let stats = admitted_stats(&stats, j, owned)?;
        total += stats.len();
        if total > 64 * 1024 * 1024 {
            return Err(failure(1));
        }
        output.bytes(
            &format!("docker-stats-{}.jsonl", samples.len()),
            &token.safe(&stats),
        )?;
        core.clock.sleep(Duration::from_secs(1)).await;
    }
    output.json("cgroup-samples.json", &samples)?;
    *phase = "summary_validation";
    let summary = output.guest_summary()?;
    let validated = policy::admit_summary(&summary, &s.selection(&j.run_id))?;
    output.json("summary-validation.json", &validated)?;
    *phase = "room_cleanup";
    let zero_start = time.elapsed();
    loop {
        let response = request(core, obs, j, &owned[0], token, Endpoint::Metrics, time).await?;
        if time.elapsed().saturating_sub(zero_start) >= Duration::from_secs(40) {
            return Err(failure(124));
        }
        if let Probe::Bytes(bytes) = &response
            && policy::zero_counts(bytes).is_ok()
        {
            if time.elapsed().saturating_sub(zero_start) >= Duration::from_secs(40) {
                return Err(failure(124));
            }
            output.bytes("metrics-cleanup.txt", &token.safe(bytes))?;
            break;
        }
        pending(core.runner, time, false).await?;
        core.clock.sleep(Duration::from_millis(500)).await;
    }
    snapshot(obs, output, "after")?;
    *phase = "server_shutdown";
    let mut stop = plan::command(core.endpoint, &["stop", "--time", "30", &server], false);
    stop.timeout = Duration::from_secs(33);
    core.run(stop).await?;
    for owned in owned.iter() {
        let v = cleanup::observed(core, j, owned, Some(&server), false).await?;
        cleanup::exited(&v)?;
        output.json(&format!("{}.exit.json", owned.role.name()), &v)?;
    }
    let final_groups = groups.as_mut().unwrap().sample(&time.utc(), [true, true])?;
    output.json("cgroup-final.json", &final_groups)?;
    output.check()?;
    pending(core.runner, time, false).await?;
    *phase = "complete";
    Ok(())
}
/// Guard and selected Tools outlive this transaction, including replay and all
/// file closes. Their caller attempts finalization even when this returns error.
pub(super) async fn run<E: Executor, O: Observations>(
    settings: &Settings,
    environment: &Environment,
    runner: &mut E,
    context: &Context<'_, O>,
) -> Result<()> {
    let obs = context.obs;
    let guard = context.guard;
    let paths = context.paths;
    let budget = context.budget;
    let time = context.time;
    let token_text = context.token;
    obs.platform()?;
    guard.check()?;
    let mut capture = Capture {
        inner: runner,
        spool: &guard.state,
        budget,
        time,
        counter: 0,
        settled: true,
    };
    let endpoint = endpoint(environment, &mut capture).await?;
    let mut clock = time.clone();
    let mut core = Core::bind(
        &endpoint,
        &mut capture,
        &mut clock,
        Policy::benchmark(false),
    )
    .await?;
    let public = core
        .run(plan::command(
            &endpoint,
            &[
                "ps",
                "--quiet",
                "--filter",
                "label=com.docker.compose.project=simplestchat-public",
            ],
            false,
        ))
        .await?;
    if !public.is_empty() {
        return Err(failure(1));
    }
    images(&mut core, settings).await?;
    if let Some(old) = guard.current()?
        && !Journal::decode(&old)?.finalized
    {
        return Err(failure(1));
    }
    pending(core.runner, time, false).await?;
    let run = files::hex(&files::random::<16>(budget)?);
    let output = Output::new(&settings.output, paths, budget, &guard.state)?;
    let mut s = settings.clone();
    s.output = output.path.clone();
    let mut primary = None;
    let mut owned = [Owned::new(Role::Server), Owned::new(Role::Generator)];
    let mut phase = "setup";
    let mut groups = None;
    let j = Journal::new(
        &s,
        run,
        endpoint.clone(),
        output.path.clone(),
        core.identity_sha256(),
    );
    let begin = j.begin(guard, &output);
    let mut token = None;
    if begin.is_ok() {
        match Token::new(&guard.state, budget, token_text) {
            Ok(t) => token = Some(t),
            Err(e) => first(&mut primary, Err(e)),
        }
    }
    first(&mut primary, begin);
    if let Some(token) = &mut token
        && primary.is_none()
    {
        first(
            &mut primary,
            body(
                &mut core,
                &s,
                &j,
                &output,
                Work {
                    owned: &mut owned,
                    groups: &mut groups,
                    token,
                    phase: &mut phase,
                },
                context,
            )
            .await,
        );
    }
    first(&mut primary, core.runner.ready().await);
    let workload = primary.is_none();
    if let Some(groups) = groups.take() {
        first(&mut primary, groups.finish());
    }
    if let Some(token) = token.take() {
        first(&mut primary, token.finish());
    }
    first(&mut primary, time.begin_retirement());
    let server = owned[0].id.clone();
    let mut retired = true;
    for role in Role::ALL.into_iter().rev() {
        let result = cleanup::retire(
            &mut core,
            &j,
            &cleanup::Evidence {
                output: &output,
                spool: &guard.state,
                budget,
                token: token_text,
            },
            &mut owned[role.index()],
            server.as_deref(),
        )
        .await;
        retired &= result.is_ok();
        first(&mut primary, result);
    }
    retired &= core.runner.cleanup_passed();
    if !retired {
        first(&mut primary, Err(failure(1)));
    }
    first(&mut primary, time.fence(true));
    let status = primary.as_ref().map_or(0, |e| e.code);
    first(
        &mut primary,
        output.json(
            "cleanup.json",
            &json!({"schemaVersion":1,"passed":retired,"mode":"normal"}),
        ),
    );
    first(
        &mut primary,
        output.json(
            "result.json",
            &json!({"schemaVersion":1,"runId":j.run_id,
        "completed":true,"passed":workload&&retired&&status==0,"phase":phase,"launcherExit":status,
        "cleanupPassed":retired,"workloadPassed":workload}),
        ),
    );
    if retired {
        let mut final_j = j.clone();
        final_j.finalized = true;
        first(&mut primary, final_j.save(guard));
    }
    first(&mut primary, output.finish());
    first(&mut primary, core.runner.ready().await);
    first(&mut primary, time.fence(true));
    first(
        &mut primary,
        core.runner
            .replay(
                b"Private benchmark transaction complete.\n",
                false,
                time.deadline().min(time.now() + Duration::from_secs(25)),
            )
            .await,
    );
    first(&mut primary, time.fence(true));
    primary.map_or(Ok(()), Err)
}
pub(super) async fn recover<E: Executor, O: Observations>(
    runner: &mut E,
    context: &Context<'_, O>,
) -> Result<()> {
    let obs = context.obs;
    let guard = context.guard;
    let paths = context.paths;
    let budget = context.budget;
    let time = context.time;
    obs.platform()?;
    guard.check()?;
    let Some(bytes) = guard.current()? else {
        return Ok(());
    };
    let mut j = Journal::decode(&bytes)?;
    if j.finalized {
        return Ok(());
    }
    if j.daemon_sha256.is_none() {
        return Err(failure(1));
    }
    time.begin_retirement()?;
    let output = Output::reopen(&j.output, paths, budget, &guard.state)?;
    let mut primary = None;
    first(&mut primary, j.preserve_emergency(&output, budget));
    let mut capture = Capture {
        inner: runner,
        spool: &guard.state,
        budget,
        time,
        counter: 0,
        settled: true,
    };
    let mut clock = time.clone();
    let bound = Core::bind_with(
        &j.endpoint,
        &mut capture,
        &mut clock,
        Policy::benchmark(true),
        true,
    )
    .await;
    match bound {
        Ok(mut core) => {
            let matches = j.daemon_sha256.as_deref() == Some(core.identity_sha256().as_str());
            if !matches {
                first(&mut primary, Err(failure(1)));
            }
            let server = Owned::recover(Role::Server, &output);
            let generator = Owned::recover(Role::Generator, &output);
            if !matches {
                first(&mut primary, server.map(|_| ()));
                first(&mut primary, generator.map(|_| ()));
            } else {
                // A bad peer record cannot suppress an independently admitted
                // role. Generator network authority still needs the server CID.
                let mut owned = [server, generator].map(|result| match result {
                    Ok(owned) => Some(owned),
                    Err(error) => {
                        first(&mut primary, Err(error));
                        None
                    }
                });
                let server = owned[0].as_ref().and_then(|owned| owned.id.clone());
                let mut retired = owned.iter().all(Option::is_some);
                for role in Role::ALL.into_iter().rev() {
                    let Some(owned) = owned[role.index()].as_mut() else {
                        continue;
                    };
                    let result = cleanup::retire(
                        &mut core,
                        &j,
                        &cleanup::Evidence {
                            output: &output,
                            spool: &guard.state,
                            budget,
                            token: "",
                        },
                        owned,
                        server.as_deref(),
                    )
                    .await;
                    retired &= result.is_ok();
                    first(&mut primary, result);
                }
                retired &= core.runner.cleanup_passed();
                if !retired {
                    first(&mut primary, Err(failure(1)));
                }
                first(&mut primary, time.fence(true));
                first(
                    &mut primary,
                    output.json(
                        "cleanup.json",
                        &json!({"schemaVersion":1,
                        "passed":retired,"mode":"emergency"}),
                    ),
                );
                if retired && primary.is_none() {
                    j.finalized = true;
                    first(&mut primary, j.save(guard));
                }
            }
        }
        Err(e) => first(&mut primary, Err(e)),
    }
    first(&mut primary, output.finish());
    first(&mut primary, time.fence(true));
    primary.map_or(Ok(()), Err)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_request_keeps_the_known_tool_status_until_every_boundary_is_positive() {
        for code in [42, 124] {
            for failed in 0..3 {
                let mut boundaries = [Ok(()), Ok(()), Ok(())];
                boundaries[failed] = Err(failure(1));
                let [post, token, namespace] = boundaries;
                let result = finish_probe(Err(failure(code)), true, post, token, namespace);
                assert_eq!(result.err().unwrap().code, code);
            }
            let unavailable = finish_probe(Err(failure(code)), true, Ok(()), Ok(()), Ok(()));
            assert!(matches!(unavailable, Ok(Probe::Unavailable(_))));
            assert_eq!(unavailable.unwrap().required().unwrap_err().code, code);
        }
        assert!(finish_probe(Err(failure(1)), false, Ok(()), Ok(()), Ok(())).is_err());
        assert!(finish_probe(Ok(vec![]), false, Ok(()), Ok(()), Err(failure(1))).is_err());
    }
}
