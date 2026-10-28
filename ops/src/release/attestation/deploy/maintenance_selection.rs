//! Reviewed maintenance environment and effective Compose selection.
//!
//! These helpers admit inputs before interruption. They do not publish the
//! selection, consume the candidate, run the launcher or establish host proof.

use super::*;
use std::{collections::BTreeSet, path::Path};

const IDENTITY_KEYS: [&str; 16] = [
    "ALLOWED_ORIGINS",
    "ANNOUNCE_IP",
    "ANNOUNCE_IPV6",
    "BIND_ADDR",
    "DATABASE_URL",
    "JWT_SECRET",
    "METRICS_TOKEN",
    "PORT",
    "RUN_MIGRATIONS",
    "SIMPLESTCHAT_IMAGE",
    "TRUSTED_PROXY_SECRET",
    "TURN_SECRET",
    "TURN_TTL",
    "TURN_URLS",
    "WEBAUTHN_ORIGIN",
    "WEBAUTHN_RP_ID",
];

fn universal_text(bytes: &[u8]) -> policy::Result<String> {
    Ok(raw_text(bytes)?.replace("\r\n", "\n").replace('\r', "\n"))
}

fn dotenv(text: &str) -> std::result::Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    for line in stage::python_lines(text) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or("Malformed app.env line")?;
        ensure(
            key.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'),
            "Malformed app.env line",
        )?;
        ensure(!values.contains_key(key), "Duplicate app.env line")?;
        values.insert(key.into(), value.into());
    }
    Ok(values)
}

fn reviewed_environment(
    installed: &str,
    proposed: &str,
) -> std::result::Result<Vec<String>, String> {
    let installed = dotenv(installed)?;
    let proposed = dotenv(proposed)?;
    for key in IDENTITY_KEYS {
        if installed.get(key).map_or("", String::as_str)
            != proposed.get(key).map_or("", String::as_str)
        {
            return Err(format!(
                "The candidate app.env changes {key}; that needs full maintenance"
            ));
        }
    }
    let keys = installed
        .keys()
        .chain(proposed.keys())
        .collect::<BTreeSet<_>>();
    Ok(keys
        .into_iter()
        .filter(|key| installed.get(*key) != proposed.get(*key))
        .cloned()
        .collect())
}

/// Return exactly the mount rendered by this host, after validating its grant.
/// Absence is admitted only by the caller's explicit legacy-upgrade decision.
fn reviewed_proxy_mount(proxy: &Json, store: &str) -> std::result::Result<Option<Json>, String> {
    let empty = Json::Array(Vec::new());
    let mut selected = None;
    for mount in array(proxy.get("volumes").unwrap_or(&empty))? {
        if object(mount)?.get("target").and_then(Json::string)
            == Some("/run/simplestchat-browser-assets")
        {
            ensure(selected.is_none(), "Ambiguous browser asset proxy mount")?;
            selected = Some(mount.clone());
        }
    }
    if selected.is_some() {
        browser_mount(proxy, store)?;
    }
    Ok(selected)
}

fn expected_maintenance_selection(
    base: &Json,
    after: &Json,
    new_image: &str,
    journal_changed: bool,
    environment_reviewed: bool,
    proxy_upgrade: bool,
    store: &str,
) -> std::result::Result<Json, String> {
    let mut expected = super::steps::expected_selection(base, new_image, journal_changed)?;
    let services = object_mut(
        object_mut(&mut expected)?
            .get_mut("services")
            .ok_or("KeyError")?,
    )?;
    if proxy_upgrade {
        let after_proxy = member(member(after, "services")?, "caddy")?;
        let rendered = reviewed_proxy_mount(after_proxy, store)?
            .ok_or("Candidate proxy lacks the browser asset store mount")?;
        let proxy = object_mut(services.get_mut("caddy").ok_or("KeyError")?)?;
        let volumes = proxy
            .entry("volumes".into())
            .or_insert_with(|| Json::Array(Vec::new()));
        match volumes {
            Json::Array(volumes) => volumes.push(rendered),
            _ => return Err("ValueError".into()),
        }
    }
    if environment_reviewed {
        let application = object_mut(services.get_mut("simplestchat").ok_or("KeyError")?)?;
        object_mut(application.get_mut("environment").ok_or("KeyError")?)?
            .insert("SIMPLESTCHAT_IMAGE".into(), Json::String(new_image.into()));
    }
    Ok(expected)
}

impl State {
    fn maintenance_environment_input(
        &mut self,
        access: &Access,
        path: &Path,
        report: &mut Value,
        owner: &mut Owner,
    ) -> Result<String> {
        ensure(
            self.maintenance_candidate_input.is_none(),
            "Candidate already admitted",
        )
        .map_err(|_| Cause::Changed)?;
        let parent = Dir::open(path.parent().ok_or(Cause::Input)?, owner)?;
        self.maintenance_directories.push(parent.clone());
        parent.protected(access.uid, owner)?;
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(Cause::Input)?;
        let record = Record::open(parent, name, access.uid, 1024 * 1024, owner)?;
        let aliases = self.records.files.iter().any(|other| record.aliases(other));
        let index = self.records.files.len();
        self.records.files.push(record);
        let core_alias = self
            .stage
            .input_aliases(self.records.files[index].file(), owner)?;
        self.policy(
            owner,
            ensure(
                !aliases && !core_alias,
                "Candidate aliases an owned release input",
            ),
        )?;
        let proposed = self.records.files[index].bytes(1024 * 1024, owner, false)?;
        let proposed = self.policy(owner, universal_text(&proposed))?;
        let installed = self.bytes("app.env", 1024 * 1024, owner)?;
        let installed = self.policy(owner, universal_text(&installed))?;
        let changed = self.policy(owner, reviewed_environment(&installed, &proposed))?;
        report["environmentChanges"] = json!(changed);
        self.maintenance_candidate_input = Some(index);
        Ok(proposed)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "The explicit maintenance inputs share the existing operation owner and executor"
    )]
    pub(super) async fn maintenance_candidate(
        &mut self,
        access: &Access,
        old: &Json,
        new_image: &str,
        candidate_env: Option<&Path>,
        browser_assets_upgrade: bool,
        report: &mut Value,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        if browser_assets_upgrade {
            self.input("Caddyfile.candidate", 1024 * 1024, access, owner)?;
            let candidate = self.bytes("Caddyfile.candidate", 1024 * 1024, owner)?;
            let committed = self
                .maintenance_source_file(access, "Caddyfile", owner, executor)
                .await?;
            self.policy(
                owner,
                ensure(
                    candidate == committed,
                    "Candidate Caddyfile differs from exact release source",
                ),
            )?;
        }
        let reviewed = match candidate_env {
            Some(path) => Some(self.maintenance_environment_input(access, path, report, owner)?),
            None => None,
        };
        let old_image = self.policy(owner, text(old, "serverImage"))?.to_owned();
        let changed = report
            .get("environmentChanges")
            .and_then(Value::as_array)
            .is_some_and(|keys| !keys.is_empty());
        self.policy(owner, ensure(
            old_image != new_image || changed || browser_assets_upgrade,
            "This image is already selected and no reviewed configuration changes were supplied",
        ))?;

        let original = self.bytes("compose.public.yml", 1024 * 1024, owner)?;
        let original = self.policy(owner, universal_text(&original))?;
        let before = self
            .compose(
                access,
                argv(&["--profile", "maintenance", "config", "--format", "json"]),
                None,
                false,
                owner,
                executor,
            )
            .await?;
        let before = self.policy(owner, decode(&before))?;
        let socket = access.root.join("postgres-socket");
        let socket = socket.to_str().ok_or(Cause::Input)?;
        self.policy(owner, policy::compose(&before, socket, &old_image))?;
        let services = self.policy(owner, member(&before, "services"))?;
        let proxy = self.policy(owner, member(services, "caddy"))?;
        let store = access.root.join("browser-assets");
        let store = store.to_str().ok_or(Cause::Input)?;
        let installed = self
            .policy(owner, reviewed_proxy_mount(proxy, store))?
            .is_some();
        if installed {
            self.input("browser-assets-installed.json", 512, access, owner)?;
            self.input("Caddyfile", u64::MAX, access, owner)?;
            let receipt = self.bytes("browser-assets-installed.json", 512, owner)?;
            let receipt = self.policy(owner, decode(&receipt))?;
            let caddy = &self.records.files[self.inputs["Caddyfile"]];
            let valid = object(&receipt).is_ok_and(|fields| fields.len() == 2)
                && receipt
                    .get("schemaVersion")
                    .is_some_and(|value| value.semantic_eq(&Json::Integer("1".into())))
                && receipt.get("caddyfileSha256").and_then(Json::string) == caddy.sha256();
            self.policy(
                owner,
                ensure(
                    valid,
                    "Browser asset proxy configuration changed; use explicit maintenance",
                ),
            )?;
        } else {
            self.policy(
                owner,
                ensure(
                    browser_assets_upgrade,
                    concat!(
                        "Browser assets need --maintenance --browser-assets-upgrade ",
                        "before app-only releases"
                    ),
                ),
            )?;
        }
        self.policy(
            owner,
            ensure(
                !browser_assets_upgrade || !installed,
                "Browser asset proxy is already installed",
            ),
        )?;
        let (mut compose, journal_changed) =
            self.selection_journal(access, services, original.clone(), owner)?;
        if browser_assets_upgrade {
            self.input("compose.public.yml.candidate", 1024 * 1024, access, owner)?;
            let data = self.bytes("compose.public.yml.candidate", 1024 * 1024, owner)?;
            compose = self.policy(owner, universal_text(&data))?;
        }
        let needle = format!("image: \"{old_image}\"");
        self.policy(
            owner,
            ensure(
                compose.matches(&needle).count() == 2,
                "Expected only app and migration image selections; reapply reviewed configuration",
            ),
        )?;
        let environment = match reviewed.as_ref() {
            Some(value) => value.clone(),
            None => {
                let data = self.bytes("app.env", 1024 * 1024, owner)?;
                self.policy(owner, universal_text(&data))?
            }
        };
        let needle_env = format!("SIMPLESTCHAT_IMAGE={old_image}");
        self.policy(
            owner,
            ensure(
                stage::python_lines(&environment)
                    .filter(|line| *line == needle_env)
                    .count()
                    == 1,
                "Current image environment differs from selection",
            ),
        )?;
        let selected_compose = compose.replace(&needle, &format!("image: \"{new_image}\""));
        let preview_env = stage::python_lines(&environment)
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
        self.atomic(
            &attempt,
            "candidate-compose.yml",
            selected_compose.as_bytes(),
            owner,
            false,
        )?;
        self.atomic(
            &attempt,
            "candidate.env",
            preview_env.as_bytes(),
            owner,
            false,
        )?;

        let base = if reviewed.is_some() {
            self.policy(
                owner,
                ensure(
                    original.matches("- ./app.env").count() == 1,
                    "Unexpected application env_file selection",
                ),
            )?;
            self.atomic(
                &attempt,
                "reviewed.env",
                environment.as_bytes(),
                owner,
                false,
            )?;
            let reviewed_path = attempt.path().join("reviewed.env");
            let reviewed_path = reviewed_path.to_str().ok_or(Cause::Input)?;
            self.atomic(
                &attempt,
                "reviewed-compose.yml",
                original
                    .replace("- ./app.env", &format!("- \"{reviewed_path}\""))
                    .as_bytes(),
                owner,
                false,
            )?;
            let base = self
                .compose(
                    access,
                    argv(&["--profile", "maintenance", "config", "--format", "json"]),
                    Some(("reviewed-compose.yml", "reviewed.env")),
                    false,
                    owner,
                    executor,
                )
                .await?;
            let base = self.policy(owner, decode(&base))?;
            let preview_path = attempt.path().join("candidate.env");
            let preview_path = preview_path.to_str().ok_or(Cause::Input)?;
            self.atomic(
                &attempt,
                "candidate-compose.yml",
                selected_compose
                    .replace("- ./app.env", &format!("- \"{preview_path}\""))
                    .as_bytes(),
                owner,
                false,
            )?;
            base
        } else {
            before.clone()
        };
        let after = self
            .compose(
                access,
                argv(&["--profile", "maintenance", "config", "--format", "json"]),
                Some(("candidate-compose.yml", "candidate.env")),
                false,
                owner,
                executor,
            )
            .await?;
        let after = self.policy(owner, decode(&after))?;
        self.policy(owner, policy::compose(&after, socket, new_image))?;
        let expected = self.policy(
            owner,
            expected_maintenance_selection(
                &base,
                &after,
                new_image,
                journal_changed,
                reviewed.is_some(),
                browser_assets_upgrade,
                store,
            ),
        )?;
        self.policy(
            owner,
            ensure(
                after.semantic_eq(&expected),
                "Candidate changes configuration beyond the reviewed selection",
            ),
        )?;
        Ok((selected_compose.into_bytes(), preview_env.into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_environment_preserves_identity_and_reports_absent_empty_setting_changes() {
        let before = "SIMPLESTCHAT_IMAGE=sha256:old\nRUN_MIGRATIONS=false\nMAX_CONNECTIONS=100\n";
        let after = "# candidate\nSIMPLESTCHAT_IMAGE=sha256:old\nRUN_MIGRATIONS=false\n\
            MAX_CONNECTIONS=200\nANNOUNCE_IPV6=\nMEDIA_WORKERS=3\n";
        assert_eq!(
            reviewed_environment(before, after).unwrap(),
            ["ANNOUNCE_IPV6", "MAX_CONNECTIONS", "MEDIA_WORKERS"]
        );
        for key in IDENTITY_KEYS {
            let before = format!("{key}=original\n");
            let after = format!("{key}=changed\n");
            assert!(reviewed_environment(&before, &after).is_err(), "{key}");
            assert!(
                reviewed_environment("", &format!("{key}=\n")).is_ok(),
                "{key}"
            );
        }
    }

    #[test]
    fn environment_parser_preserves_verbatim_values_and_refuses_ambiguous_assignments() {
        assert_eq!(
            dotenv("# ignored\nVALUE= =verbatim \n").unwrap()["VALUE"],
            " =verbatim "
        );
        for invalid in [" bad=1", "a=1", "_A=1", "1A=1", "A-B=1", "A", "A=1\nA=2"] {
            assert!(dotenv(invalid).is_err(), "{invalid}");
        }
        assert_eq!(dotenv("A=1\u{2028}B=2\n").unwrap().len(), 2);
        assert_eq!(universal_text(b"A=1\r\nB=2\r").unwrap(), "A=1\nB=2\n");
    }

    fn config(mount: Option<Value>) -> Json {
        let volumes: Vec<Value> = mount.into_iter().collect();
        fixed(json!({"services":{
            "simplestchat":{"image":"old","environment":{"SIMPLESTCHAT_IMAGE":"old"}},
            "migrate":{"image":"old"},"caddy":{"volumes":volumes}
        }}))
        .unwrap()
    }

    #[test]
    fn proxy_upgrade_copies_only_the_validated_host_rendered_mount() {
        let mount = json!({"type":"bind","source":"/public/browser-assets",
            "target":"/run/simplestchat-browser-assets","read_only":true,
            "bind":{"propagation":"rprivate","create_host_path":false}});
        let base = config(None);
        let mut after = config(Some(mount));
        let expected = expected_maintenance_selection(
            &base,
            &after,
            "new",
            false,
            true,
            true,
            "/public/browser-assets",
        )
        .unwrap();
        assert_eq!(
            text(
                member(member(&expected, "services").unwrap(), "simplestchat").unwrap(),
                "image"
            )
            .unwrap(),
            "new"
        );
        assert!(
            member(
                member(member(&expected, "services").unwrap(), "caddy").unwrap(),
                "volumes"
            )
            .unwrap()
            .semantic_eq(
                member(
                    member(member(&after, "services").unwrap(), "caddy").unwrap(),
                    "volumes"
                )
                .unwrap()
            )
        );
        let services =
            object_mut(object_mut(&mut after).unwrap().get_mut("services").unwrap()).unwrap();
        let proxy = object_mut(services.get_mut("caddy").unwrap()).unwrap();
        let Json::Array(volumes) = proxy.get_mut("volumes").unwrap() else {
            panic!("fixture");
        };
        object_mut(&mut volumes[0])
            .unwrap()
            .insert("read_only".into(), Json::Bool(false));
        assert!(
            expected_maintenance_selection(
                &base,
                &after,
                "new",
                false,
                true,
                true,
                "/public/browser-assets"
            )
            .is_err()
        );
    }

    #[test]
    fn proxy_admission_refuses_ambiguous_or_unsafe_capabilities() {
        assert!(
            reviewed_proxy_mount(&fixed(json!({})).unwrap(), "/store")
                .unwrap()
                .is_none()
        );
        for volumes in [
            json!([null]),
            json!([{"target":"/run/simplestchat-browser-assets"}]),
            json!([{"target":"/run/simplestchat-browser-assets"},
                {"target":"/run/simplestchat-browser-assets"}]),
            json!([{"type":"bind","source":"/store","target":"/run/simplestchat-browser-assets",
                "read_only":true,"bind":"rprivate"}]),
        ] {
            assert!(
                reviewed_proxy_mount(&fixed(json!({"volumes":volumes})).unwrap(), "/store")
                    .is_err()
            );
        }
    }
}
