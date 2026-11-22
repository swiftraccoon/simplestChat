//! The fixture's host configuration. The deployment templates are rendered
//! by Ansible itself, as a deployment renders them; this module builds that
//! local play and then makes the few fixture edits to the rendered text:
//! ownership labels, loopback-only publication, the fixture certificate and
//! the explicit proxy origin. Each edit names its exact anchor and refuses a
//! rendering in which the anchor no longer occurs exactly as reviewed.

use super::plan::LABEL;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

/// The rendered outputs and the deployment template each comes from.
pub(super) const TEMPLATES: [(&str, &str); 8] = [
    ("app.env", "public-app.env.j2"),
    ("migration.env", "public-migration.env.j2"),
    ("proxy.env", "public-proxy.env.j2"),
    ("compose.public.yml", "public-compose.yml.j2"),
    ("pg_hba.conf", "public-pg_hba.conf.j2"),
    ("init-database.sql", "public-init-database.sql.j2"),
    ("runtime-grants.sql", "public-runtime-grants.sql.j2"),
    (
        "postgres-admin-password",
        "public-postgres-admin-password.j2",
    ),
];
/// The six deployment secrets, each 32 random bytes as 64 hexadecimal digits.
pub(super) const SECRETS: [&str; 6] = [
    "postgres_admin_password",
    "migration_password",
    "database_password",
    "metrics_token",
    "proxy_secret",
    "owner_password",
];
const CHANGED: &str = "Production fixture anchor changed; review its rendering";

/// The immutable identities and ownership token of one fixture run.
#[derive(Clone, Debug)]
pub(super) struct Identity {
    pub image: String,
    pub revision: String,
    pub token: String,
    pub postgres: String,
    pub caddy: String,
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `name:TAG@sha256:DIGEST` for exactly one of `names`, with a tag of
/// lowercase letters, digits, dots and hyphens.
fn pinned(value: &str, names: &[&str]) -> bool {
    names.iter().any(|name| {
        value
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(':'))
            .and_then(|rest| rest.split_once("@sha256:"))
            .is_some_and(|(tag, digest)| {
                !tag.is_empty()
                    && tag
                        .bytes()
                        .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-'))
                    && hex(digest, 64)
            })
    })
}

impl Identity {
    /// Refuse anything but immutable image identities, a full revision and a
    /// 32-digit token, and proxy images from the two reviewed publishers.
    pub(super) fn check(&self) -> Result<(), &'static str> {
        let valid = self
            .image
            .strip_prefix("sha256:")
            .is_some_and(|v| hex(v, 64))
            && hex(&self.revision, 40)
            && hex(&self.token, 32)
            && pinned(&self.postgres, &["docker.io/library/postgres"])
            && pinned(
                &self.caddy,
                &[
                    "docker.io/library/caddy",
                    "ghcr.io/swiftraccoon/simplestchat-caddy",
                ],
            );
        if valid {
            Ok(())
        } else {
            Err("Fixture image identities, revision, and ownership token must be explicit")
        }
    }
}

/// The variables the templates are rendered with: the smallest deployment
/// shape on loopback, and `secrets` by name.
pub(super) fn variables(
    identity: &Identity,
    config: &Path,
    root: &Path,
    secrets: &[String; 6],
) -> Value {
    let named: serde_json::Map<String, Value> = SECRETS
        .iter()
        .zip(secrets)
        .map(|(name, value)| ((*name).to_owned(), Value::String(value.clone())))
        .collect();
    json!({
        "scpub_config": config,
        "scpub_root": root,
        "scpub_server_image": identity.image,
        "scpub_postgres_image": identity.postgres,
        "scpub_caddy_image": identity.caddy,
        "scpub_domain": "localhost",
        "scpub_webauthn_rp_id": "localhost",
        "scpub_announce_ip": "127.0.0.1",
        "scpub_announce_ipv6": "",
        "scpub_ipv6_network": "fd5c:5c68:a7d1::/64",
        "scpub_registration_enabled": false,
        // The smallest shape: the fixture proves the release path, not capacity.
        "scpub_app_cpus": 1,
        "scpub_media_workers": 1,
        "scpub_app_memory_mib": 512,
        "scpub_max_connections": 175,
        "scpub_max_rooms": 175,
        "scpub_max_participants_per_room": 175,
        "scpub_max_broadcasters_per_room": 30,
        "scpub_max_users": 100,
        "scpub_max_persisted_rooms": 100,
        "scpub_postgres_memory_mib": 1024,
        "scpub_postgres_shared_buffers_mib": 256,
        "scpub_postgres_cpus": 1,
        "scpub_caddy_memory_mib": 256,
        "scpub_caddy_cpus": 0.5,
        "scpub_secrets": named,
    })
}

/// One local play that renders every template into `staging` with Ansible's
/// own template module and `variables`.
pub(super) fn play(checkout: &Path, staging: &Path, variables: &Value) -> Value {
    let mut vars = variables.as_object().cloned().unwrap_or_default();
    vars.insert(
        "ansible_python_interpreter".into(),
        "{{ ansible_playbook_python }}".into(),
    );
    let templates = checkout.join("ops/ansible/templates");
    let names: Vec<&str> = TEMPLATES.iter().map(|(_, template)| *template).collect();
    json!([{
        "hosts": "localhost",
        "connection": "local",
        "gather_facts": false,
        "vars": vars,
        "tasks": [{
            "ansible.builtin.template": {
                "src": format!("{}/{{{{ item }}}}", templates.display()),
                "dest": format!("{}/{{{{ item }}}}", staging.display()),
                "mode": "0600",
            },
            "loop": names,
        }],
    }])
}

/// The texts the fixture edits: every rendered template by output name, the
/// checkout's base Compose file and its Caddyfile.
pub(super) struct Sources {
    pub rendered: BTreeMap<String, String>,
    pub base: String,
    pub caddyfile: String,
}

/// Lines as Python's `str.splitlines` separates them.
fn python_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let found = rest.char_indices().find(|(_, character)| {
            matches!(
                character,
                '\n' | '\r'
                    | '\u{0b}'
                    | '\u{0c}'
                    | '\u{1c}'
                    | '\u{1d}'
                    | '\u{1e}'
                    | '\u{85}'
                    | '\u{2028}'
                    | '\u{2029}'
            )
        });
        let Some((at, character)) = found else {
            lines.push(rest);
            break;
        };
        lines.push(&rest[..at]);
        let mut next = at + character.len_utf8();
        if character == '\r' && rest[next..].starts_with('\n') {
            next += 1;
        }
        rest = &rest[next..];
    }
    lines
}

/// Replace the one `NAME=` line of an environment file with `NAME=value`.
fn environment(text: &str, name: &str, value: &str) -> Result<String, &'static str> {
    let prefix = format!("{name}=");
    let lines = python_lines(text);
    if lines
        .iter()
        .filter(|line| line.starts_with(&prefix))
        .count()
        != 1
    {
        return Err("Production environment setting changed; review its rendering");
    }
    let replaced: Vec<String> = lines
        .iter()
        .map(|line| {
            if line.starts_with(&prefix) {
                format!("{prefix}{value}")
            } else {
                (*line).to_owned()
            }
        })
        .collect();
    Ok(replaced.join("\n") + "\n")
}

fn replace_once(text: &str, old: &str, new: &str) -> Result<String, &'static str> {
    if text.matches(old).count() != 1 {
        return Err(CHANGED);
    }
    Ok(text.replacen(old, new, 1))
}

fn label(token: &str) -> String {
    format!("    labels:\n      {LABEL}: \"{token}\"\n")
}

/// The services of a Compose file's top-level `services` block, in order,
/// with each header's byte offset after its line.
fn services(text: &str) -> Result<Vec<(String, usize)>, &'static str> {
    let start = text
        .find("\nservices:\n")
        .map(|at| at + 1)
        .or_else(|| text.starts_with("services:\n").then_some(0))
        .ok_or(CHANGED)?;
    let mut found = Vec::new();
    let mut offset = start + "services:\n".len();
    for line in text[offset..].split_inclusive('\n') {
        let end = offset + line.len();
        let body = line.trim_end_matches('\n');
        if !body.is_empty() && !body.starts_with(' ') && !body.starts_with('#') {
            break;
        }
        if let Some(name) = body
            .strip_prefix("  ")
            .and_then(|rest| rest.strip_suffix(':'))
            .filter(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'-' || byte == b'_')
            })
        {
            found.push((name.to_owned(), end));
        }
        offset = end;
    }
    if found.is_empty() {
        return Err(CHANGED);
    }
    Ok(found)
}

/// Insert the ownership label after each service header. A service that
/// already has labels refuses, as a duplicate key would.
fn labelled(text: &str, token: &str, only: Option<&str>) -> Result<String, &'static str> {
    if text.contains("\n    labels:") {
        return Err(CHANGED);
    }
    let mut result = text.to_owned();
    let mut headers = services(text)?;
    if let Some(only) = only {
        headers.retain(|(name, _)| name == only);
        if headers.len() != 1 {
            return Err(CHANGED);
        }
    }
    for (_, end) in headers.iter().rev() {
        result.insert_str(*end, &label(token));
    }
    Ok(result)
}

/// The project's base Compose file with the application published only on
/// loopback, its one media port, and the ownership label.
fn base(text: &str, token: &str) -> Result<String, &'static str> {
    let text = replace_once(
        text,
        "      - \"40000-${RTC_PORT_END:-40063}:40000-${RTC_PORT_END:-40063}/udp\"\n",
        "      - \"127.0.0.1:40000:40000/udp\"\n",
    )?;
    let (_, ports) = text.split_once("\n    ports:\n").ok_or(CHANGED)?;
    let published: Vec<&str> = ports
        .lines()
        .take_while(|line| line.starts_with("      ") || line.trim_start().starts_with('#'))
        .filter(|line| line.starts_with("      - "))
        .collect();
    if published
        != [
            "      - \"127.0.0.1:3000:3000\"",
            "      - \"127.0.0.1:40000:40000/udp\"",
        ]
    {
        return Err(CHANGED);
    }
    labelled(&text, token, Some("simplestchat"))
}

/// The rendered public project with every service and its network owned by
/// the fixture, the proxy on loopback HTTPS only, and the fixture
/// certificate mounted beside the proxy's configuration.
fn public(
    text: &str,
    identity: &Identity,
    config: &Path,
    root: &Path,
) -> Result<String, &'static str> {
    if text
        .matches(&format!("image: \"{}\"", identity.image))
        .count()
        != 2
    {
        return Err(CHANGED);
    }
    let text = replace_once(
        text,
        "    ports:\n      - \"80:80/tcp\"\n      - \"443:443/tcp\"\n      - \"443:443/udp\"\n",
        "    ports:\n      - \"127.0.0.1:443:443/tcp\"\n",
    )?;
    let assets = format!(
        "      - \"{}/browser-assets:/run/simplestchat-browser-assets:ro\"\n",
        root.display()
    );
    let (_, after) = text.split_once(&assets).ok_or(CHANGED)?;
    if after.starts_with("      - ") {
        return Err(CHANGED);
    }
    let mounts = format!(
        "{assets}      - \"{config}/fixture.crt:/etc/caddy/fixture.crt:ro\"\n      - \
         \"{config}/fixture.key:/etc/caddy/fixture.key:ro\"\n",
        config = config.display()
    );
    let mut text = replace_once(&text, &assets, &mounts)?;
    if text.starts_with("networks:") || text.contains("\nnetworks:") {
        return Err(CHANGED);
    }
    text = labelled(&text, &identity.token, None)?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "networks:\n  default:\n    labels:\n      {LABEL}: \"{}\"\n",
        identity.token
    ));
    Ok(text)
}

/// The checkout's Caddyfile serving only `https://localhost` with the
/// fixture certificate, no admin listener and no automatic HTTPS, and
/// proxying to the application service.
fn caddyfile(text: &str) -> Result<String, &'static str> {
    let text = replace_once(
        text,
        "{\n\t# Keep Caddy's loopback-only admin endpoint for reloads. Do not publish its\n\
         \t# port from a container or override it with a publicly reachable address.\n",
        "{\n\t# Explicit fixture certificate: no ACME, redirects, or admin listener.\n\
         \tadmin off\n\tauto_https off\n",
    )?;
    let text = replace_once(
        &text,
        "{$CADDY_DOMAIN:simplestchat.example.com} {",
        "https://localhost {\n\ttls /etc/caddy/fixture.crt /etc/caddy/fixture.key",
    )?;
    replace_once(
        &text,
        "reverse_proxy {$CADDY_UPSTREAM:127.0.0.1:3000}",
        "reverse_proxy simplestchat:3000",
    )
}

/// Every configuration file of the fixture, by name, each ending in a
/// newline. Nothing is written here.
pub(super) fn outputs(
    identity: &Identity,
    config: &Path,
    root: &Path,
    sources: &Sources,
) -> Result<BTreeMap<String, String>, &'static str> {
    identity.check()?;
    let mut outputs = BTreeMap::new();
    for (name, _) in TEMPLATES {
        let text = sources.rendered.get(name).ok_or(CHANGED)?;
        outputs.insert(name.to_owned(), text.clone());
    }
    let app = environment(&outputs["app.env"], "MEDIA_WORKERS", "1")?;
    let app = environment(&app, "RTC_PORT_END", "40000")?;
    outputs.insert("app.env".into(), app);
    outputs.insert(
        "compose.base.yml".into(),
        base(&sources.base, &identity.token)?,
    );
    let compose = public(&outputs["compose.public.yml"], identity, config, root)?;
    outputs.insert("compose.public.yml".into(), compose);
    let caddy = caddyfile(&sources.caddyfile)?;
    let digest = super::files::sha256(caddy.as_bytes());
    outputs.insert("Caddyfile".into(), caddy);
    outputs.insert(
        "browser-assets-installed.json".into(),
        format!("{{\"schemaVersion\": 1, \"caddyfileSha256\": \"{digest}\"}}\n"),
    );
    outputs.insert(
        "images.json".into(),
        format!(
            "{{\n  \"revision\": \"{}\",\n  \"serverImage\": \"{}\",\n  \"postgresImage\": \
             \"{}\",\n  \"caddyImage\": \"{}\"\n}}\n",
            identity.revision, identity.image, identity.postgres, identity.caddy
        ),
    );
    for text in outputs.values_mut() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    Ok(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_reviewed_publishers_name_the_database_and_proxy_images() {
        let digest = "d".repeat(64);
        let identity = |postgres: String, caddy: String| Identity {
            image: format!("sha256:{digest}"),
            revision: "b".repeat(40),
            token: "c".repeat(32),
            postgres,
            caddy,
        };
        let caddy = format!("docker.io/library/caddy:2.11.4@sha256:{digest}");
        let postgres = format!("docker.io/library/postgres:18.6@sha256:{digest}");
        assert!(identity(postgres.clone(), caddy.clone()).check().is_ok());
        for other in [
            format!("postgres:18.6@sha256:{digest}"),
            format!("docker.io/library/postgresx:18.6@sha256:{digest}"),
            format!("x/docker.io/library/postgres:18.6@sha256:{digest}"),
            format!("docker.io/library/caddy:2.11.4@sha256:{digest}"),
        ] {
            assert!(identity(other, caddy.clone()).check().is_err());
        }
        for other in [
            format!("caddy:2.11.4@sha256:{digest}"),
            format!("ghcr.io/swiftraccoon/simplestchat-caddyx:2@sha256:{digest}"),
            postgres.clone(),
        ] {
            assert!(identity(postgres.clone(), other).check().is_err());
        }
    }

    #[test]
    fn python_line_splitting_keeps_every_separator_python_knows() {
        assert_eq!(python_lines("a\r\nb\rc\n\nd"), ["a", "b", "c", "", "d"]);
        assert_eq!(python_lines("a\u{2028}b\u{85}c\u{0b}"), ["a", "b", "c"]);
        assert_eq!(python_lines(""), Vec::<&str>::new());
        assert_eq!(python_lines("\n"), [""]);
    }
}
