//! Exact-source adapter for the fixed thirteen configuration outputs.
//! No supplied template language, YAML parser, command or filesystem effect.

use super::{Error, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) const CADDY: &str = concat!(
    "ghcr.io/swiftraccoon/simplestchat-caddy:",
    "2.11.7-73c81fdbfe625d3df3989ba807064c81fbe2c071",
    "@sha256:23c127cb8945a1de0c6753010614b50af1643bbcf93ab8a9a7d7461efd50b7c0"
);
pub(super) const SECRET_NAMES: [&str; 7] = [
    "database_password",
    "jwt_secret",
    "metrics_token",
    "migration_password",
    "owner_password",
    "postgres_admin_password",
    "proxy_secret",
];
const TEMPLATES: [(&str, &str, &str); 8] = [
    (
        "app.env",
        "app.env",
        "79843435a487687e783936259d404058b44c7b974c6bb1127d23fa7bbd3b66d7",
    ),
    (
        "migration.env",
        "migration.env",
        "741fdd93baf369a1bca0f6ea286957f1676b545e156fc45c604ff17a65b6ea37",
    ),
    (
        "proxy.env",
        "proxy.env",
        "f3b06778c9230ab2cf1e3f4fd1b6508d3aa2e998b24b77f85935787c2d578487",
    ),
    (
        "compose.public.yml",
        "compose.yml",
        "134c3f4ab6029d822f3c47fdc397b76e784cc0e55a153bc8733dffcb819a9af0",
    ),
    (
        "pg_hba.conf",
        "pg_hba.conf",
        "1df752048c7b1aa4a4d5e61fbfe6b37b46d6d5743bbe91998326c7c41162296e",
    ),
    (
        "init-database.sql",
        "init-database.sql",
        "9434dccf7cc9563b871876522640450ebad9099540bd7440cf2960f2b85e6b53",
    ),
    (
        "runtime-grants.sql",
        "runtime-grants.sql",
        "d133f5e28e5a30c9adf83bb22a03c4d000f1ab90884dfb6cc26aeb52c61d8cf2",
    ),
    (
        "postgres-admin-password",
        "postgres-admin-password",
        "94b01b10c6678feed0ce478b6bfc90c077a4136fbe62a846587de8a74c580bb3",
    ),
];
pub(super) struct Identity<'a> {
    pub image: &'a str,
    pub revision: &'a str,
    pub run_id: &'a str,
    pub postgres: &'a str,
    pub caddy: &'a str,
    pub bridge: &'a str,
}
pub(super) fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn hex(text: &str, count: usize) -> bool {
    text.len() == count
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn reference(text: &str) -> bool {
    let Some((name, hash)) = text.split_once("@sha256:") else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"./:-".contains(&b))
        && hex(hash, 64)
}
fn source(sources: &BTreeMap<String, Vec<u8>>, name: &str, hash: &str) -> Result<String> {
    let value = sources.get(name).ok_or(Error::Input)?;
    if value.is_empty() || value.len() > 1024 * 1024 || digest(value) != hash {
        return Err(Error::Changed);
    }
    String::from_utf8(value.clone()).map_err(|_| Error::Input)
}
fn replace(text: &mut String, from: &str, to: &str, count: usize) -> Result<()> {
    if text.matches(from).count() != count {
        return Err(Error::Changed);
    }
    *text = text.replace(from, to);
    Ok(())
}
fn false_block(text: &mut String, begin: &str) -> Result<()> {
    if text.matches(begin).count() != 1 || text.matches("{% endif %}\n").count() != 1 {
        return Err(Error::Changed);
    }
    let at = text.find(begin).ok_or(Error::Changed)?;
    let end = text[at..].find("{% endif %}\n").ok_or(Error::Changed)? + at + "{% endif %}\n".len();
    text.replace_range(at..end, "");
    Ok(())
}

/// Render only the reviewed fixed source profile. The caller owns output custody.
pub(super) fn render(
    sources: &BTreeMap<String, Vec<u8>>,
    identity: Identity<'_>,
    secrets: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, Vec<u8>>> {
    if identity
        .image
        .strip_prefix("sha256:")
        .is_none_or(|v| !hex(v, 64))
        || !hex(identity.revision, 40)
        || !hex(identity.run_id, 32)
        || !reference(identity.postgres)
        || identity.caddy != CADDY
        || identity
            .bridge
            .strip_prefix("sc")
            .is_none_or(|v| !hex(v, 12))
        || secrets.len() != SECRET_NAMES.len()
        || SECRET_NAMES
            .iter()
            .any(|name| secrets.get(*name).is_none_or(|v| !hex(v, 64)))
    {
        return Err(Error::Input);
    }
    let mut values: Vec<(String, String, usize)> = vec![
        ("scpub_server_image".into(), identity.image.into(), 3),
        ("scpub_domain".into(), "maintain.localhost".into(), 3),
        (
            "scpub_webauthn_rp_id".into(),
            "maintain.localhost".into(),
            1,
        ),
        ("scpub_announce_ip".into(), "127.0.0.1".into(), 1),
        ("scpub_announce_ipv6".into(), String::new(), 1),
        (
            "'true' if scpub_registration_enabled else 'false'".into(),
            "false".into(),
            1,
        ),
        (
            "40000 + (scpub_media_workers | int) - 1".into(),
            "40000".into(),
            1,
        ),
        ("scpub_media_workers".into(), "1".into(), 1),
        ("scpub_app_cpus".into(), "1".into(), 1),
        ("scpub_app_memory_mib".into(), "512".into(), 1),
        ("scpub_max_connections".into(), "175".into(), 1),
        ("scpub_max_users".into(), "100".into(), 1),
        ("scpub_max_rooms".into(), "175".into(), 1),
        ("scpub_max_persisted_rooms".into(), "100".into(), 1),
        ("scpub_max_participants_per_room".into(), "175".into(), 1),
        ("scpub_max_broadcasters_per_room".into(), "30".into(), 1),
        ("scpub_root".into(), "/srv/simplestchat-public".into(), 7),
        ("scpub_config".into(), "/etc/simplestchat-public".into(), 4),
        ("scpub_postgres_image".into(), identity.postgres.into(), 1),
        ("scpub_caddy_image".into(), identity.caddy.into(), 1),
        ("scpub_postgres_shared_buffers_mib".into(), "256".into(), 1),
        ("scpub_postgres_memory_mib".into(), "1024".into(), 1),
        ("scpub_postgres_cpus".into(), "1".into(), 1),
        ("scpub_caddy_memory_mib".into(), "256".into(), 1),
        ("scpub_caddy_cpus".into(), "0.5".into(), 1),
    ];
    for (name, count) in [
        ("database_password", 2),
        ("jwt_secret", 1),
        ("metrics_token", 1),
        ("proxy_secret", 2),
        ("migration_password", 2),
        ("postgres_admin_password", 1),
    ] {
        values.push((
            format!("scpub_secrets.{name}"),
            secrets[name].clone(),
            count,
        ));
    }
    let mut templates = BTreeMap::new();
    for (output, template, hash) in TEMPLATES {
        let name = format!("ops/ansible/templates/public-{template}.j2");
        templates.insert(output, source(sources, &name, hash)?);
    }
    false_block(
        templates.get_mut("app.env").ok_or(Error::Input)?,
        "{% if scpub_turn_enabled | default(false) %}\n",
    )?;
    false_block(
        templates
            .get_mut("compose.public.yml")
            .ok_or(Error::Input)?,
        "{% if scpub_announce_ipv6 or ':' in scpub_announce_ip %}\n",
    )?;
    for (name, value, expected) in values {
        let token = format!("{{{{ {name} }}}}");
        let count = templates
            .values()
            .map(|v| v.matches(&token).count())
            .sum::<usize>();
        if count != expected {
            return Err(Error::Changed);
        }
        for text in templates.values_mut() {
            *text = text.replace(&token, &value);
        }
    }
    let label = format!(
        "    labels:\n      simplestchat.maintain-prerequisite: \"{}\"\n",
        identity.run_id
    );
    let public = templates
        .get_mut("compose.public.yml")
        .ok_or(Error::Input)?;
    for service in ["simplestchat", "postgres", "caddy", "migrate"] {
        let key = format!("\n  {service}:\n");
        replace(public, &key, &format!("{key}{label}"), 1)?;
    }
    replace(
        public,
        "      - \"80:80/tcp\"\n      - \"443:443/tcp\"\n      - \"443:443/udp\"\n",
        concat!(
            "      - \"127.0.0.1:80:80/tcp\"\n",
            "      - \"127.0.0.1:443:443/tcp\"\n",
            "      - \"127.0.0.1:443:443/udp\"\n",
        ),
        1,
    )?;
    public.push_str(&format!(
        "networks:\n  default:\n    name: \"{}\"\n    external: true\n",
        identity.bridge
    ));
    let mut base = source(
        sources,
        "docker-compose.yml",
        "d728f15a6e51f5afb6eea553178af72360feb01533ab82df288a70a32f432e27",
    )?;
    replace(
        &mut base,
        "\n  simplestchat:\n",
        &format!("\n  simplestchat:\n{label}"),
        1,
    )?;
    replace(
        &mut base,
        "      - \"40000-${RTC_PORT_END:-40063}:40000-${RTC_PORT_END:-40063}/udp\"",
        "      - \"127.0.0.1:40000:40000/udp\"",
        1,
    )?;
    templates.insert("compose.base.yml", base);
    let mut outputs = BTreeMap::new();
    for (name, text) in templates {
        if ["{{", "}}", "{%", "%}", "{#", "#}"]
            .iter()
            .any(|token| text.contains(token))
        {
            return Err(Error::Changed);
        }
        outputs.insert(name.into(), text.into_bytes());
    }
    let caddy = source(
        sources,
        "Caddyfile",
        "cb48cb809b5f80cb4187f3976be868c1d71a8411ad893ba0ad2b4df03ffd6039",
    )?;
    let caddy_hash = digest(caddy.as_bytes());
    outputs.insert("Caddyfile".into(), caddy.into_bytes());
    for (name, value) in [
        ("secrets.json", json!(secrets)),
        (
            "images.json",
            json!({"revision":identity.revision,"serverImage":identity.image,
            "postgresImage":identity.postgres,"caddyImage":identity.caddy}),
        ),
        (
            "browser-assets-installed.json",
            json!({"schemaVersion":1,"caddyfileSha256":caddy_hash}),
        ),
    ] {
        outputs.insert(
            name.into(),
            serde_json::to_vec(&value).map_err(|_| Error::Input)?,
        );
    }
    if outputs.len() != 13 {
        return Err(Error::Refused);
    }
    Ok(outputs)
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
