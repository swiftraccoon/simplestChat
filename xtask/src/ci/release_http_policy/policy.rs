//! Original selected predicates; no response payload is projected into a receipt.

use super::json::Value;
use super::{Result, fail, json};
use serde_json::{Value as Table, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Endpoint {
    pub path: &'static str,
    pub status: u16,
    pub content_type: &'static str,
    pub limit: usize,
}
pub(super) const ENDPOINTS: [Endpoint; 5] = [
    Endpoint {
        path: "/",
        status: 200,
        content_type: "text/html",
        limit: 131072,
    },
    Endpoint {
        path: "/api/capabilities",
        status: 200,
        content_type: "application/json",
        limit: 4096,
    },
    Endpoint {
        path: "/api/auth/profile",
        status: 401,
        content_type: "application/json",
        limit: 4096,
    },
    Endpoint {
        path: "/metrics",
        status: 404,
        content_type: "text/plain",
        limit: 4096,
    },
    Endpoint {
        path: "/diagnostics/media",
        status: 404,
        content_type: "text/plain",
        limit: 4096,
    },
];
pub(super) fn require(ok: bool, code: &'static str) -> Result<()> {
    if ok { Ok(()) } else { Err(fail(1, code)) }
}
// Python str.isspace adds the four ASCII information separators to Unicode White_Space.
fn space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}
pub(super) fn headers(values: &[(String, String)]) -> Result<BTreeMap<String, String>> {
    require(values.len() <= 64, "http_header_count")?;
    require(
        values
            .iter()
            .map(|(k, v)| k.chars().count() + v.chars().count() + 4)
            .sum::<usize>()
            <= 16384,
        "http_headers_size",
    )?;
    let mut result = BTreeMap::new();
    for (key, value) in values {
        require(
            !result.contains_key(&key.to_lowercase()),
            "http_duplicate_header",
        )?;
        require(
            !key.chars()
                .chain(value.chars())
                .any(|c| matches!(c, '\r' | '\n' | '\0')),
            "http_header_control",
        )?;
        result.insert(key.to_lowercase(), value.trim_matches(space).into());
    }
    Ok(result)
}
pub(super) fn security(h: &BTreeMap<String, String>) -> Result<()> {
    let table: Table = serde_json::from_str(include_str!("../../../../security/http-policy.json"))
        .map_err(|_| fail(1, "http_policy_failed"))?;
    let fixed = table["headers"]
        .as_object()
        .ok_or_else(|| fail(1, "http_policy_failed"))?;
    require(
        fixed
            .iter()
            .filter(|(k, _)| *k != "strict-transport-security")
            .all(|(k, v)| h.get(k).map(String::as_str) == v.as_str()),
        "http_security_headers",
    )?;
    require(!h.contains_key("server"), "http_server_header")?;
    require(!h.contains_key("set-cookie"), "http_anonymous_cookie")?;
    require(
        h.get("strict-transport-security").map(String::as_str) == Some("max-age=31536000"),
        "http_hsts",
    )?;
    let mut directives = serde_json::Map::new();
    for directive in h
        .get("content-security-policy")
        .map_or("", String::as_str)
        .split(';')
    {
        let mut words = directive.split(space).filter(|s| !s.is_empty());
        if let Some(name) = words.next() {
            require(
                directives
                    .insert(
                        name.to_lowercase(),
                        words.collect::<Vec<_>>().join(" ").into(),
                    )
                    .is_none(),
                "http_csp_duplicate",
            )?;
        }
    }
    require(Table::Object(directives) == table["directives"], "http_csp")
}
pub(super) fn validate(
    index: usize,
    status: u16,
    h: &BTreeMap<String, String>,
    body: &[u8],
) -> Result<()> {
    let endpoint = ENDPOINTS
        .get(index)
        .ok_or_else(|| fail(1, "http_unlisted_endpoint"))?;
    require(status == endpoint.status, "http_status")?;
    security(h)?;
    require(
        h.get("content-type")
            .map_or("", String::as_str)
            .split(';')
            .next()
            .unwrap_or("")
            .to_lowercase()
            == endpoint.content_type,
        "http_content_type",
    )?;
    require(
        h.get("content-encoding").map_or("identity", String::as_str) == "identity",
        "http_content_encoding",
    )?;
    require(
        !body.is_empty() && body.len() <= endpoint.limit,
        "http_body_size",
    )?;
    if index == 1 {
        require(
            h.get("cache-control").map(String::as_str) == Some("no-store"),
            "http_cache_policy",
        )?;
    }
    match index {
        0 => require(
            body[..body.len().min(256)]
                .windows(15)
                .any(|s| s.eq_ignore_ascii_case(b"<!doctype html>")),
            "http_homepage",
        ),
        1 => {
            let v = json::object(body)?;
            let bools = [
                "accounts",
                "passwordLogin",
                "passkeyLogin",
                "roomDirectory",
                "roomCreation",
                "adHocRooms",
            ];
            require(
                v.len() == 9
                    && v.get("version").and_then(Value::as_u64) == Some(1)
                    && bools
                        .iter()
                        .all(|k| v.get(*k).is_some_and(Value::is_boolean))
                    && matches!(
                        v.get("passwordRegistration").and_then(Value::as_str),
                        Some("disabled" | "open" | "invite")
                    )
                    && matches!(
                        v.get("passkeyRegistration").and_then(Value::as_str),
                        Some("disabled" | "open")
                    ),
                "http_public_capabilities",
            )
        }
        2 => require(
            {
                let value = json::object(body)?;
                value.len() == 1
                    && value.get("error").and_then(Value::as_str) == Some("Missing authorization")
            },
            "http_auth_omission",
        ),
        _ => require(body.trim_ascii() == b"Not Found", "http_private_endpoint"),
    }
}
pub(super) fn report() -> Table {
    json!({"passed":true,"requests":5,"scope":"owned-loopback-release-fixture",
        "securityHeaders":true,"contentSecurityPolicy":true,"jsonNoStore":true,
        "anonymousCookiesAbsent":true,"missingAuthorizationRejected":true,
        "privateEndpointsOmitted":true,"responseLimits":true})
}
pub(super) fn owned(config: &super::config::Config, index: usize, bytes: &[u8]) -> Result<()> {
    let value = json::object(bytes)?;
    let id = if index.is_multiple_of(2) {
        &config.caddy
    } else {
        &config.app
    };
    let role = if index.is_multiple_of(2) {
        "caddy"
    } else {
        "simplestchat"
    };
    let labels = value
        .get("labels")
        .and_then(Value::as_object)
        .ok_or_else(|| fail(1, "http_policy_failed"))?;
    let id_matches = value
        .get("id")
        .ok_or_else(|| fail(1, "http_policy_failed"))?
        .as_str()
        == Some(id);
    require(id_matches, "fixture_container_ownership")?;
    let running = value
        .get("running")
        .ok_or_else(|| fail(1, "http_policy_failed"))?;
    require(
        running == &Value::Bool(true)
            && labels
                .get("simplestchat.release-test")
                .and_then(Value::as_str)
                == Some(config.token.as_str())
            && labels
                .get("com.docker.compose.project")
                .and_then(Value::as_str)
                == Some("simplestchat-public")
            && labels
                .get("com.docker.compose.service")
                .and_then(Value::as_str)
                == Some(role),
        "fixture_container_ownership",
    )?;
    if index.is_multiple_of(2) {
        let ports = value
            .get("ports")
            .and_then(Value::as_object)
            .ok_or_else(|| fail(1, "http_policy_failed"))?;
        require(
            ports
                .get("443/tcp")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.len() == 1
                        && rows[0].as_object().is_some_and(|row| {
                            row.len() == 2
                                && row.get("HostIp").and_then(Value::as_str) == Some("127.0.0.1")
                                && row.get("HostPort").and_then(Value::as_str) == Some("443")
                        })
                })
                && ports.iter().all(|(k, v)| k == "443/tcp" || v.is_null()),
            "fixture_https_publication",
        )?;
    }
    Ok(())
}
