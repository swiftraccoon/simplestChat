//! Ordered preflight, retained browser assets and live backup before replacement.

use super::*;

impl State {
    async fn pause(
        &self,
        access: &Access,
        duration: Duration,
        owner: &mut Owner,
        recovery: bool,
    ) -> Result<()> {
        records::check(owner, recovery)?;
        #[cfg(test)]
        if access.virtual_poll {
            owner.elapsed = Some(owner.elapsed.unwrap_or_default().saturating_add(duration));
            return records::check(owner, recovery);
        }
        #[cfg(not(test))]
        let _ = access;
        tokio::time::sleep(duration).await;
        records::check(owner, recovery)
    }
    pub(super) async fn ready(
        &mut self,
        access: &Access,
        origin: Option<&str>,
        seconds: u64,
        recovery: bool,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        let start = owner.now();
        let deadline = start
            .checked_add(Duration::from_secs(seconds))
            .ok_or(Cause::Deadline)?;
        let _ceiling = budget::ready_ceiling(start, Duration::from_secs(seconds))?;
        let url = format!("{}/ready", origin.unwrap_or("http://127.0.0.1:3000"));
        loop {
            let output = self
                .command(
                    access,
                    Role::Ready,
                    argv(&[
                        "--disable",
                        "--noproxy",
                        "*",
                        "--proto",
                        if origin.is_some() { "=https" } else { "=http" },
                        "--fail",
                        "--silent",
                        "--show-error",
                        "--max-time",
                        "2",
                        &url,
                    ]),
                    CommandIo::ordinary(recovery),
                    owner,
                    executor,
                )
                .await?;
            let ready = output.status == Some(0)
                && decode(&output.stdout).ok().is_some_and(|value| {
                    value.get("status").and_then(Json::string) == Some("ready")
                });
            if ready {
                return Ok(());
            }
            if owner.now() >= deadline {
                return self.refused(owner, "Application readiness deadline exceeded");
            }
            self.pause(access, budget::READY_POLL, owner, recovery)
                .await?;
        }
    }
    fn metrics_token(&self, owner: &mut Owner) -> Result<Option<String>> {
        let data = self.bytes("app.env", 1024 * 1024, owner)?;
        let text = std::str::from_utf8(&data).map_err(|_| Cause::Policy)?;
        for line in stage::python_lines(text) {
            if let Some(value) = line.strip_prefix("METRICS_TOKEN=") {
                let token = stage::python_strip(value);
                return Ok((token.len() >= 32
                    && token.len() <= 512
                    && token
                        .bytes()
                        .all(|v| v.is_ascii_alphanumeric() || b"._~-".contains(&v)))
                .then(|| token.into()));
            }
        }
        Ok(None)
    }
    async fn rooms(
        &mut self,
        access: &Access,
        name: &str,
        recovery: bool,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<Option<Json>> {
        let path = self.attempt().path().join(name);
        let spelling = path.to_str().ok_or(Cause::Input)?;
        let output = self
            .command(
                access,
                Role::Metrics,
                argv(&[
                    "--disable",
                    "--noproxy",
                    "*",
                    "--proto",
                    "=http",
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "2",
                    "--config",
                    spelling,
                    "http://127.0.0.1:3000/metrics",
                ]),
                CommandIo::ordinary(recovery),
                owner,
                executor,
            )
            .await?;
        if output.status != Some(0) {
            return Ok(None);
        }
        let Ok(text) = std::str::from_utf8(&output.stdout) else {
            return Ok(None);
        };
        for line in stage::python_lines(text) {
            if line.starts_with("simplestchat_rooms_active ") {
                return Ok(line
                    .split(stage::python_space)
                    .filter(|v| !v.is_empty())
                    .nth(1)
                    .and_then(metrics_integer));
            }
        }
        Ok(None)
    }
    pub(super) async fn quiet(
        &mut self,
        access: &Access,
        seconds: u64,
        report: &mut Value,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        report["quietWaitRequestedSeconds"] = json!(seconds);
        let token = if seconds > 0 {
            self.metrics_token(owner)?
        } else {
            None
        };
        let Some(token) = token else {
            report["quietWaitSeconds"] = json!(0);
            report["roomsActiveAtQuietCheck"] = Value::Null;
            return Ok(());
        };
        let attempt = self.attempt();
        self.atomic(
            &attempt,
            "metrics-curl.config",
            format!("header = \"Authorization: Bearer {token}\"\n").as_bytes(),
            owner,
            false,
        )?;
        let started = owner.now();
        let deadline = started
            .checked_add(Duration::from_secs(seconds))
            .ok_or(Cause::Deadline)?;
        let mut rooms = self
            .rooms(access, "metrics-curl.config", false, owner, executor)
            .await?;
        while rooms.as_ref().is_some_and(|value| {
            matches!(value, Json::Integer(value)
            if value != "0" && !value.starts_with('-'))
        }) && owner.now() < deadline
        {
            self.pause(
                access,
                budget::QUIET_POLL.min(deadline.saturating_duration_since(owner.now())),
                owner,
                false,
            )
            .await?;
            rooms = self
                .rooms(access, "metrics-curl.config", false, owner, executor)
                .await?;
        }
        let elapsed = owner.now().saturating_duration_since(started).as_secs_f64();
        report["quietWaitSeconds"] = json!((elapsed * 1000.0).round_ties_even() / 1000.0);
        self.quiet_rooms = rooms;
        report["roomsActiveAtQuietCheck"] = Value::Null;
        Ok(())
    }
    pub(super) async fn observe_rooms(
        &mut self,
        access: &Access,
        report: &mut Value,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        let mut rooms = None;
        if let Some(token) = self.metrics_token(owner)? {
            let attempt = self.attempt();
            self.atomic(
                &attempt,
                "replacement-metrics-curl.config",
                format!("header = \"Authorization: Bearer {token}\"\n").as_bytes(),
                owner,
                false,
            )?;
            rooms = self
                .rooms(
                    access,
                    "replacement-metrics-curl.config",
                    false,
                    owner,
                    executor,
                )
                .await?;
        }
        self.replacement_rooms = rooms;
        report["roomsActiveBeforeStop"] = Value::Null;
        #[cfg(test)]
        self.fault(
            super::super::files::Fault::ReplacementObservationTime,
            owner,
        )?;
        report["roomsObservedBeforeStopAt"] = json!(timestamp());
        Ok(())
    }
    pub(super) async fn candidate(
        &mut self,
        access: &Access,
        old: &Json,
        new_image: &str,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let data = self.bytes("compose.public.yml", 1024 * 1024, owner)?;
        // Path.read_text() performs universal CRLF/CR newline translation;
        // rollback copies below retain the exact original bytes separately.
        let mut compose = self
            .policy(owner, raw_text(&data))?
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        let before_data = self
            .compose(
                access,
                argv(&["--profile", "maintenance", "config", "--format", "json"]),
                None,
                false,
                owner,
                executor,
            )
            .await?;
        let before = self.policy(owner, decode(&before_data))?;
        let old_image = self.policy(owner, text(old, "serverImage"))?;
        let socket = access.root.join("postgres-socket");
        let socket = socket.to_str().ok_or(Cause::Input)?;
        self.policy(owner, policy::compose(&before, socket, old_image))?;
        let services = self.policy(owner, member(&before, "services"))?;
        let proxy = self.policy(owner, member(services, "caddy"))?;
        let store = access.root.join("browser-assets");
        self.policy(
            owner,
            browser_mount(proxy, store.to_str().ok_or(Cause::Input)?),
        )?;
        self.input("browser-assets-installed.json", 512, access, owner)?;
        self.input("Caddyfile", u64::MAX, access, owner)?;
        let receipt = self.bytes("browser-assets-installed.json", 512, owner)?;
        let receipt = self.policy(owner, decode(&receipt))?;
        let caddy = &self.records.files[self.inputs["Caddyfile"]];
        let valid = object(&receipt).is_ok_and(|v| v.len() == 2)
            && receipt
                .get("schemaVersion")
                .is_some_and(|v| v.semantic_eq(&Json::Integer("1".into())))
            && receipt.get("caddyfileSha256").and_then(Json::string) == caddy.sha256();
        if !valid {
            return self.refused(
                owner,
                "Browser asset proxy configuration changed; use explicit maintenance",
            );
        }
        #[cfg(test)]
        let journal = access
            .root
            .parent()
            .ok_or(Cause::Input)?
            .join("journal.enabled");
        #[cfg(not(test))]
        let journal =
            std::path::PathBuf::from("/etc/simplestchat-monitoring/application-journal.enabled");
        let mut changed = false;
        if journal.exists() {
            let parent = Dir::open(journal.parent().ok_or(Cause::Input)?, owner)?;
            let record = Record::open(
                parent.clone(),
                journal.file_name().unwrap().to_str().ok_or(Cause::Input)?,
                access.uid,
                2,
                owner,
            );
            let read = match record {
                Ok(record) => {
                    let read = record.bytes(2, owner, false);
                    record.finish(owner);
                    read
                }
                Err(cause) => Err(cause),
            };
            parent.finish(owner);
            if read? != b"1\n" {
                return self.refused(owner, "Invalid journal selection");
            }
            let application = self.policy(owner, member(services, "simplestchat"))?;
            let current = application.get("logging");
            let expected = self.policy(
                owner,
                fixed(json!({"driver":"journald","options":{
                "tag":"simplestchat.app","mode":"non-blocking","max-buffer-size":"4m"}})),
            )?;
            if !current.is_some_and(|v| v.semantic_eq(&expected)) {
                let local = self.policy(
                    owner,
                    fixed(json!({"driver":"local","options":{
                    "max-size":"10m","max-file":"3"}})),
                )?;
                self.policy(
                    owner,
                    ensure(
                        current.is_some_and(|v| v.semantic_eq(&local)),
                        "Unexpected application logging configuration",
                    ),
                )?;
                let (app, dependencies) =
                    compose.split_once("\n  postgres:").ok_or(Cause::Policy)?;
                self.policy(
                    owner,
                    ensure(
                        app.matches("    logging: *bounded-logging").count() == 1,
                        "Unexpected logging layout",
                    ),
                )?;
                compose = format!("{}\n  postgres:{dependencies}", app.replace(
                    "    logging: *bounded-logging", concat!("    logging:\n",
                        "      driver: journald\n      options:\n        tag: simplestchat.app\n",
                        "        mode: non-blocking\n        max-buffer-size: 4m")));
                changed = true;
            }
        }
        let needle = format!("image: \"{old_image}\"");
        self.policy(
            owner,
            ensure(
                compose.matches(&needle).count() == 2,
                "Expected only app and migration image selections; reapply reviewed configuration",
            ),
        )?;
        let env = self.bytes("app.env", 1024 * 1024, owner)?;
        let env = self.policy(owner, raw_text(&env))?;
        let needle_env = format!("SIMPLESTCHAT_IMAGE={old_image}");
        self.policy(
            owner,
            ensure(
                stage::python_lines(env)
                    .filter(|v| *v == needle_env)
                    .count()
                    == 1,
                "Current image environment differs from selection",
            ),
        )?;
        let preview = compose
            .replace(&needle, &format!("image: \"{new_image}\""))
            .into_bytes();
        let environment = stage::python_lines(env)
            .map(|line| {
                if line == needle_env {
                    format!("SIMPLESTCHAT_IMAGE={new_image}")
                } else {
                    line.into()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let attempt = self.attempt();
        self.atomic(&attempt, "candidate-compose.yml", &preview, owner, false)?;
        self.atomic(
            &attempt,
            "candidate.env",
            environment.as_bytes(),
            owner,
            false,
        )?;
        let after_data = self
            .compose(
                access,
                argv(&["--profile", "maintenance", "config", "--format", "json"]),
                Some(("candidate-compose.yml", "candidate.env")),
                false,
                owner,
                executor,
            )
            .await?;
        let after = self.policy(owner, decode(&after_data))?;
        self.policy(owner, policy::compose(&after, socket, new_image))?;
        let expected = self.policy(owner, expected_selection(&before, new_image, changed))?;
        self.policy(
            owner,
            ensure(
                after.semantic_eq(&expected),
                "Candidate changes configuration beyond the reviewed selection",
            ),
        )?;
        Ok((preview, environment.into_bytes()))
    }
}

fn expected_selection(before: &Json, image: &str, journal: bool) -> policy::Result<Json> {
    let mut expected = before.clone();
    let services = object_mut(
        object_mut(&mut expected)?
            .get_mut("services")
            .ok_or("KeyError")?,
    )?;
    for name in ["simplestchat", "migrate"] {
        object_mut(services.get_mut(name).ok_or("KeyError")?)?
            .insert("image".into(), Json::String(image.into()));
    }
    if journal {
        object_mut(services.get_mut("simplestchat").ok_or("KeyError")?)?.insert(
            "logging".into(),
            fixed(json!({"driver":"journald","options":{
                "tag":"simplestchat.app","mode":"non-blocking","max-buffer-size":"4m"}}))?,
        );
    }
    Ok(expected)
}
