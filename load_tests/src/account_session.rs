// Owned account authentication for the opt-in, loopback-only media/chat cohort.
// Credentials and cookies remain private; reports contain identities and timing only.
use anyhow::{Context, Result};
use base64::Engine;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, ORIGIN, SET_COOKIE};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use simplestchat::signaling::protocol::{ClientMessage, ServerMessage};
use std::collections::HashSet;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_CREDENTIAL_BYTES: u64 = 256 * 1024;
const MAX_RESPONSE_BYTES: usize = 32 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const REFRESH_BEFORE_SECONDS: u64 = 60;
const MAX_RENEWAL_ATTEMPTS: usize = 4;
const REFRESH_COOKIE: &str = "__Host-refresh_token";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Credentials {
    email: String,
    password: String,
    account_id: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("account_id", &self.account_id)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    pub fn load(path: &Path, expected_clients: usize) -> Result<Vec<Self>> {
        let metadata = std::fs::symlink_metadata(path)
            .context("Could not inspect the private account credentials file")?;
        anyhow::ensure!(
            metadata.is_file(),
            "Account credentials must be a regular file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            anyhow::ensure!(
                metadata.permissions().mode() & 0o777 == 0o600,
                "Account credentials file permissions must be 0600"
            );
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_CREDENTIAL_BYTES + 1)
            .read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_CREDENTIAL_BYTES,
            "Account credentials file is too large"
        );
        let credentials: Vec<Self> = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Invalid account credentials file"))?;
        anyhow::ensure!(
            credentials.len() == expected_clients && expected_clients > 0,
            "Account credentials count must match the client count"
        );
        let mut accounts = HashSet::new();
        let mut emails = HashSet::new();
        for credential in &credentials {
            anyhow::ensure!(
                uuid::Uuid::parse_str(&credential.account_id).is_ok()
                    && accounts.insert(&credential.account_id)
                    && !credential.email.is_empty()
                    && credential.email.len() <= 254
                    && emails.insert(&credential.email)
                    && !credential.password.is_empty()
                    && credential.password.len() <= 512,
                "Account credentials must name distinct valid accounts"
            );
        }
        Ok(credentials)
    }
}

#[derive(Deserialize)]
struct AuthResponse {
    token: String,
    user: AuthUser,
}

#[derive(Deserialize)]
struct AuthUser {
    id: String,
}

#[derive(Deserialize)]
struct TokenClaims {
    sub: String,
    exp: u64,
    sid: uuid::Uuid,
}

#[derive(Deserialize)]
struct TicketResponse {
    ticket: String,
    expires_in: u64,
}

struct PendingRenewal {
    request_id: String,
    started: Instant,
    next: Instant,
    retry: bool,
    attempts: usize,
}

pub struct AccountSession {
    http: reqwest::Client,
    origin: String,
    account_id: String,
    token: String,
    cookie: String,
    session_id: uuid::Uuid,
    original_expires_at: u64,
    expires_at: u64,
    socket_expires_at: u64,
    refresh_at: Instant,
    pending: Option<PendingRenewal>,
    refresh_latencies_ms: Vec<u64>,
    renewal_latencies_ms: Vec<u64>,
    deferred_renewals: usize,
    failures: usize,
}

impl std::fmt::Debug for AccountSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountSession")
            .field("account_id", &self.account_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReport {
    pub passed: bool,
    pub account_id: String,
    pub original_expires_at: u64,
    pub latest_expires_at: u64,
    pub original_expiry_crossed: bool,
    pub http_refreshes: usize,
    pub socket_renewals: usize,
    pub deferred_renewals: usize,
    pub failures: usize,
    pub refresh_latencies_ms: Vec<u64>,
    pub renewal_latencies_ms: Vec<u64>,
}

impl AccountSession {
    pub async fn login(
        server_url: &str,
        credentials: &Credentials,
        source_address: Option<Ipv4Addr>,
    ) -> Result<Self> {
        let origin = owned_origin(server_url)?;
        anyhow::ensure!(
            source_address.is_none_or(|address| address.is_loopback()),
            "Authenticated load clients require loopback source addresses"
        );
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .local_address(source_address.map(IpAddr::V4))
            .build()
            .map_err(|_| anyhow::anyhow!("Could not create account HTTP client"))?;
        let request = http
            .post(format!("{origin}/api/auth/login"))
            .header(ORIGIN, &origin)
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&serde_json::json!({
                "email": credentials.email,
                "password": credentials.password,
            }))?);
        let (headers, authentication): (_, AuthResponse) = request_json(request).await?;
        let now = epoch_seconds()?;
        let claims = token_claims(&authentication, &credentials.account_id, now)?;
        let cookie = refresh_cookie(&headers)?;
        Ok(Self {
            http,
            origin,
            account_id: credentials.account_id.clone(),
            token: authentication.token,
            cookie,
            session_id: claims.sid,
            original_expires_at: claims.exp,
            expires_at: claims.exp,
            socket_expires_at: claims.exp,
            refresh_at: refresh_deadline(claims.exp, now),
            pending: None,
            refresh_latencies_ms: Vec::new(),
            renewal_latencies_ms: Vec::new(),
            deferred_renewals: 0,
            failures: 0,
        })
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub async fn ticket(&self) -> Result<String> {
        let request = self
            .http
            .post(format!("{}/api/auth/ws-ticket", self.origin))
            .header(ORIGIN, &self.origin)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(CONTENT_TYPE, "application/json")
            .body("{}");
        let (_, response): (_, TicketResponse) = request_json(request).await?;
        anyhow::ensure!(
            response.ticket.len() == 43
                && response.ticket.bytes().all(url_symbol)
                && (1..=30).contains(&response.expires_in),
            "Invalid one-use WebSocket ticket response"
        );
        Ok(response.ticket)
    }

    /// While awaiting renewal, this is the ACK deadline or a bounded retry time.
    pub fn next_refresh(&self) -> Instant {
        self.pending
            .as_ref()
            .map_or(self.refresh_at, |pending| pending.next)
    }

    pub async fn refresh(&mut self) -> Result<ClientMessage> {
        let result = self.refresh_inner().await;
        if result.is_err() {
            self.failures += 1;
        }
        result
    }

    async fn refresh_inner(&mut self) -> Result<ClientMessage> {
        if let Some(pending) = &mut self.pending {
            anyhow::ensure!(
                pending.retry && Instant::now() >= pending.next,
                "Authenticated socket renewal was not acknowledged before its deadline"
            );
            anyhow::ensure!(
                pending.attempts < MAX_RENEWAL_ATTEMPTS,
                "Authenticated socket renewal exceeded its retry bound"
            );
            pending.request_id = uuid::Uuid::new_v4().to_string();
            pending.attempts += 1;
            pending.retry = false;
            pending.next = Instant::now() + REQUEST_TIMEOUT;
            return renewal_message(&self.token, &pending.request_id);
        }
        anyhow::ensure!(
            self.refresh_latencies_ms.len() < 8,
            "Account refresh observation bound exceeded"
        );
        let started = Instant::now();
        let request = self
            .http
            .post(format!("{}/api/auth/refresh", self.origin))
            .header(ORIGIN, &self.origin)
            .header(COOKIE, &self.cookie)
            .header(CONTENT_TYPE, "application/json")
            .body("{}");
        let (headers, authentication): (_, AuthResponse) = request_json(request).await?;
        let now = epoch_seconds()?;
        let claims = token_claims(&authentication, &self.account_id, now)?;
        let cookie = refresh_cookie(&headers)?;
        anyhow::ensure!(
            cookie != self.cookie && claims.exp > self.expires_at && claims.sid == self.session_id,
            "Account refresh must rotate its cookie and extend the same session"
        );
        self.refresh_latencies_ms
            .push(started.elapsed().as_millis() as u64);
        self.cookie = cookie;
        self.token = authentication.token;
        self.expires_at = claims.exp;
        self.refresh_at = refresh_deadline(claims.exp, now);
        let request_id = uuid::Uuid::new_v4().to_string();
        let message = renewal_message(&self.token, &request_id)?;
        self.pending = Some(PendingRenewal {
            request_id,
            started: Instant::now(),
            next: Instant::now() + REQUEST_TIMEOUT,
            retry: false,
            attempts: 1,
        });
        Ok(message)
    }

    pub fn observe(&mut self, message: &ServerMessage) -> Result<()> {
        let result = self.observe_inner(message);
        if result.is_err() {
            self.failures += 1;
        }
        result
    }

    fn observe_inner(&mut self, message: &ServerMessage) -> Result<()> {
        match message {
            ServerMessage::AuthenticationRenewed {
                request_id,
                expires_at,
            } => {
                let pending = self
                    .pending
                    .as_ref()
                    .context("Unrequested socket renewal")?;
                anyhow::ensure!(
                    pending.request_id == *request_id && self.expires_at == *expires_at,
                    "Socket renewal acknowledgement did not match the requested session"
                );
                self.renewal_latencies_ms
                    .push(pending.started.elapsed().as_millis() as u64);
                self.socket_expires_at = *expires_at;
                self.pending = None;
            }
            ServerMessage::AuthenticationRenewalFailed { .. } => {
                anyhow::bail!("Authenticated socket renewal was rejected");
            }
            ServerMessage::AuthenticationRenewalDeferred {
                request_id,
                retry_after_ms,
                expires_at,
            } => {
                let pending = self
                    .pending
                    .as_mut()
                    .context("Unrequested deferred socket renewal")?;
                let now = epoch_seconds()?;
                anyhow::ensure!(
                    pending.request_id == *request_id
                        && !pending.retry
                        && *expires_at == self.socket_expires_at
                        && (1..=5000).contains(retry_after_ms)
                        && pending.attempts < MAX_RENEWAL_ATTEMPTS
                        && now
                            + u64::from(retry_after_ms.div_ceil(1000))
                            + REQUEST_TIMEOUT.as_secs()
                            < *expires_at,
                    "Deferred socket renewal cannot safely retry within the original credential lifetime"
                );
                pending.retry = true;
                pending.next = Instant::now() + Duration::from_millis(u64::from(*retry_after_ms));
                self.deferred_renewals += 1;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn report(&self) -> AccountReport {
        let now = epoch_seconds().unwrap_or_default();
        let crossed = now >= self.original_expires_at.saturating_add(60);
        let refreshes = self.refresh_latencies_ms.len();
        let renewals = self.renewal_latencies_ms.len();
        AccountReport {
            passed: crossed
                && refreshes > 0
                && refreshes == renewals
                && self.pending.is_none()
                && self.socket_expires_at > now
                && self.failures == 0,
            account_id: self.account_id.clone(),
            original_expires_at: self.original_expires_at,
            latest_expires_at: self.socket_expires_at,
            original_expiry_crossed: crossed,
            http_refreshes: refreshes,
            socket_renewals: renewals,
            deferred_renewals: self.deferred_renewals,
            failures: self.failures,
            refresh_latencies_ms: self.refresh_latencies_ms.clone(),
            renewal_latencies_ms: self.renewal_latencies_ms.clone(),
        }
    }
}

fn epoch_seconds() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn refresh_deadline(expiry: u64, now: u64) -> Instant {
    Instant::now() + Duration::from_secs(expiry.saturating_sub(now + REFRESH_BEFORE_SECONDS))
}

fn owned_origin(server_url: &str) -> Result<String> {
    let url = url::Url::parse(server_url)
        .map_err(|_| anyhow::anyhow!("Invalid account workload endpoint"))?;
    let loopback = match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost",
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    anyhow::ensure!(
        url.scheme() == "ws"
            && loopback
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/ws",
        "Authenticated workloads require an owned credential-free loopback ws:// endpoint"
    );
    let mut origin = url;
    origin
        .set_scheme("http")
        .map_err(|()| anyhow::anyhow!("Invalid account HTTP scheme"))?;
    Ok(origin.origin().ascii_serialization())
}

fn url_symbol(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn token_claims(response: &AuthResponse, account: &str, now: u64) -> Result<TokenClaims> {
    anyhow::ensure!(
        response.user.id == account && response.token.len() <= 16_384,
        "Authentication response belongs to a different account or exceeds its bound"
    );
    let parts: Vec<_> = response.token.split('.').collect();
    anyhow::ensure!(
        parts.len() == 3
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.bytes().all(url_symbol)),
        "Invalid account token envelope"
    );
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| anyhow::anyhow!("Invalid account token payload"))?;
    let claims: TokenClaims = serde_json::from_slice(&payload)
        .map_err(|_| anyhow::anyhow!("Invalid account token claims"))?;
    anyhow::ensure!(
        claims.sub == account && claims.exp > now + 600 && claims.exp <= now + 960,
        "Account token identity or unmodified fifteen-minute lifetime did not match"
    );
    Ok(claims)
}

fn refresh_cookie(headers: &reqwest::header::HeaderMap) -> Result<String> {
    let mut found = None;
    for header in headers.get_all(SET_COOKIE) {
        let value = header
            .to_str()
            .map_err(|_| anyhow::anyhow!("Invalid refresh cookie header"))?;
        if let Some(pair) = value.split(';').next()
            && let Some((name, token)) = pair.split_once('=')
            && name == REFRESH_COOKIE
        {
            anyhow::ensure!(
                found.is_none()
                    && !token.is_empty()
                    && token.len() <= 512
                    && token.bytes().all(url_symbol),
                "Invalid or duplicate refresh cookie"
            );
            found = Some(pair.to_owned());
        }
    }
    found.context("Authentication response did not rotate the refresh cookie")
}

fn renewal_message(token: &str, request_id: &str) -> Result<ClientMessage> {
    // AuthenticationToken has an intentionally private constructor. Its wire
    // deserializer retains that production wrapper and its redacted Debug.
    serde_json::from_value(serde_json::json!({
        "type": "renewAuthentication", "requestId": request_id, "token": token,
    }))
    .map_err(|_| anyhow::anyhow!("Could not construct authenticated socket renewal"))
}

async fn request_json<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<(reqwest::header::HeaderMap, T)> {
    let mut response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Account HTTP request failed"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "Account HTTP request returned status {}",
        response.status().as_u16()
    );
    anyhow::ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= MAX_RESPONSE_BYTES as u64),
        "Account HTTP response exceeds its bound"
    );
    let headers = response.headers().clone();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("Account HTTP response interrupted"))?
    {
        anyhow::ensure!(
            bytes.len() + chunk.len() <= MAX_RESPONSE_BYTES,
            "Account HTTP response exceeds its bound"
        );
        bytes.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Invalid account HTTP response"))?;
    Ok((headers, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn authentication(account: &str, session: uuid::Uuid, expiry: u64) -> AuthResponse {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "sub": account, "sid": session, "exp": expiry,
            }))
            .unwrap(),
        );
        AuthResponse {
            token: format!("e30.{payload}.fixture"),
            user: AuthUser {
                id: account.to_owned(),
            },
        }
    }

    fn authentication_json(authentication: &AuthResponse) -> serde_json::Value {
        serde_json::json!({
            "token": authentication.token,
            "user": {"id": authentication.user.id},
        })
    }

    #[test]
    fn account_requests_are_restricted_to_owned_loopback_endpoints() {
        assert_eq!(
            owned_origin("ws://127.0.0.1:3119/ws").unwrap(),
            "http://127.0.0.1:3119"
        );
        assert_eq!(
            owned_origin("ws://[::1]:3119/ws").unwrap(),
            "http://[::1]:3119"
        );
        for invalid in [
            "ws://example.com/ws",
            "wss://127.0.0.1/ws",
            "ws://name:secret@127.0.0.1/ws",
            "ws://127.0.0.1/ws?token=secret",
            "ws://127.0.0.1/ws#secret",
            "ws://127.0.0.1/other",
        ] {
            assert!(owned_origin(invalid).is_err());
        }
    }

    #[test]
    fn account_tokens_require_the_expected_identity_and_real_lifetime() {
        let id = uuid::Uuid::new_v4().to_string();
        let authentication = authentication(&id, uuid::Uuid::new_v4(), 1900);
        assert_eq!(token_claims(&authentication, &id, 1000).unwrap().exp, 1900);
        assert!(token_claims(&authentication, &uuid::Uuid::new_v4().to_string(), 1000).is_err());
        assert!(token_claims(&authentication, &id, 1300).is_err());
        assert!(token_claims(&authentication, &id, 1).is_err());
    }

    #[test]
    fn only_one_nonempty_host_refresh_cookie_is_accepted() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append(SET_COOKIE, "other=value; Secure".parse().unwrap());
        assert!(refresh_cookie(&headers).is_err());
        headers.append(
            SET_COOKIE,
            "__Host-refresh_token=fixture; HttpOnly; Secure; Path=/"
                .parse()
                .unwrap(),
        );
        assert_eq!(
            refresh_cookie(&headers).unwrap(),
            "__Host-refresh_token=fixture"
        );
        headers.append(
            SET_COOKIE,
            "__Host-refresh_token=another; Secure; Path=/"
                .parse()
                .unwrap(),
        );
        assert!(refresh_cookie(&headers).is_err());
    }

    #[test]
    fn private_cohort_files_require_private_permissions_and_unique_accounts() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let directory = std::env::current_dir()
                .unwrap()
                .join("target/account-session-tests");
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join(format!("{}.json", uuid::Uuid::new_v4()));
            let credential = serde_json::json!({
                "email": "owned@example.test", "password": "owned test credential",
                "accountId": uuid::Uuid::new_v4().to_string(),
            });
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            serde_json::to_writer(file, &vec![credential.clone()]).unwrap();
            let credentials = Credentials::load(&path, 1).unwrap();
            let debug = format!("{:?}", credentials[0]);
            assert!(!debug.contains("owned@example.test"));
            assert!(!debug.contains("owned test credential"));
            assert!(Credentials::load(&path, 2).is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(Credentials::load(&path, 1).is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::write(
                &path,
                serde_json::to_vec(&vec![credential.clone(), credential]).unwrap(),
            )
            .unwrap();
            assert!(Credentials::load(&path, 2).is_err());
            std::fs::remove_file(path).unwrap();
        }
    }

    #[tokio::test]
    async fn login_ticket_refresh_and_correlated_socket_renewal_share_the_real_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/ws", listener.local_addr().unwrap());
        let account_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4();
        let now = epoch_seconds().unwrap();
        let initial = authentication(&account_id, session_id, now + 900);
        let refreshed = authentication(&account_id, session_id, now + 901);
        let initial_json = authentication_json(&initial);
        let refreshed_json = authentication_json(&refreshed);
        let initial_bearer = initial.token;
        let server = tokio::spawn(async move {
            for (path, cookie, body) in [
                ("/api/auth/login", Some("fixture-initial"), initial_json),
                (
                    "/api/auth/ws-ticket",
                    None,
                    serde_json::json!({"ticket":"t".repeat(43),"expires_in":30}),
                ),
                ("/api/auth/refresh", Some("fixture-rotated"), refreshed_json),
            ] {
                let (mut stream, peer) = listener.accept().await.unwrap();
                assert_eq!(peer.ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
                let mut request = Vec::new();
                let mut chunk = [0; 1024];
                loop {
                    let size = stream.read(&mut chunk).await.unwrap();
                    assert!(size > 0);
                    request.extend_from_slice(&chunk[..size]);
                    assert!(request.len() <= 16_384);
                    if let Some(boundary) = request.windows(4).position(|part| part == b"\r\n\r\n")
                    {
                        let header = std::str::from_utf8(&request[..boundary]).unwrap();
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if request.len() >= boundary + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with(&format!("POST {path} HTTP/1.1\r\n")));
                if path == "/api/auth/ws-ticket" {
                    assert!(request.to_ascii_lowercase().contains(&format!(
                        "authorization: bearer {}",
                        initial_bearer.to_ascii_lowercase()
                    )));
                }
                if path == "/api/auth/refresh" {
                    assert!(request.contains("__Host-refresh_token=fixture-initial"));
                    assert!(!request.to_ascii_lowercase().contains("authorization:"));
                }
                let cookie = cookie.map_or_else(String::new, |cookie| {
                    format!(
                        "Set-Cookie: __Host-refresh_token={cookie}; Secure; HttpOnly; Path=/\r\n"
                    )
                });
                let bytes = serde_json::to_vec(&body).unwrap();
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{cookie}\r\n",
                    bytes.len()
                );
                stream.write_all(header.as_bytes()).await.unwrap();
                stream.write_all(&bytes).await.unwrap();
            }
        });
        let credentials = Credentials {
            email: "owned@example.test".to_owned(),
            password: "owned test credential".to_owned(),
            account_id,
        };
        let mut session = AccountSession::login(&endpoint, &credentials, Some(Ipv4Addr::LOCALHOST))
            .await
            .unwrap();
        assert_eq!(session.account_id(), credentials.account_id);
        assert!(session.origin().starts_with("http://127.0.0.1:"));
        assert_eq!(session.ticket().await.unwrap(), "t".repeat(43));
        let renewal = session.refresh().await.unwrap();
        let ClientMessage::RenewAuthentication { request_id, .. } = renewal else {
            panic!("expected renewal");
        };
        assert!(!format!("{session:?}").contains("fixture-rotated"));
        assert!(
            !session.report().passed,
            "short mock checks must never claim full expiry coverage"
        );
        session
            .observe(&ServerMessage::AuthenticationRenewalDeferred {
                request_id: request_id.clone(),
                retry_after_ms: 1,
                expires_at: now + 900,
            })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        let ClientMessage::RenewAuthentication {
            request_id: retry_id,
            ..
        } = session.refresh().await.unwrap()
        else {
            panic!("expected deferred retry");
        };
        assert_ne!(retry_id, request_id);
        session
            .observe(&ServerMessage::AuthenticationRenewed {
                request_id: retry_id,
                expires_at: now + 901,
            })
            .unwrap();
        assert_eq!(
            session.report().http_refreshes,
            1,
            "a socket deferral must not rotate the cookie again"
        );
        assert_eq!(session.report().socket_renewals, 1);
        assert_eq!(session.report().deferred_renewals, 1);
        assert!(session.next_refresh() > Instant::now() + Duration::from_secs(800));
        session.original_expires_at = now - 61;
        let report = session.report();
        assert!(report.passed);
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("fixture"));
        assert!(!serialized.contains("token"));
        server.await.unwrap();
    }
}
