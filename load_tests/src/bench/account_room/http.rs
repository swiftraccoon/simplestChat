//! Fixed loopback HTTP paths with source-address separation for registration.

use super::config::Config;
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

#[async_trait::async_trait]
pub(super) trait Http: Send + Sync {
    async fn ready(&self) -> Result<bool>;
    async fn post(
        &self,
        index: usize,
        path: &str,
        body: &Value,
        token: Option<&str>,
    ) -> Result<Value>;
    async fn metrics(&self) -> Result<String>;
}

pub(super) struct NativeHttp {
    origin: String,
    token: String,
    clients: Vec<reqwest::Client>,
}

async fn body(mut response: reqwest::Response, maximum: usize) -> Result<Vec<u8>> {
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "setup_http_failed"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            chunk.len() <= maximum.saturating_sub(bytes.len()),
            "setup_response_too_large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

impl NativeHttp {
    pub fn new(config: &Config) -> Result<Self> {
        let clients = (0..super::config::CLIENTS)
            .map(|index| {
                reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .local_address(IpAddr::V4(Ipv4Addr::new(127, 0, 1, index as u8 + 2)))
                    .timeout(Duration::from_secs(15))
                    .build()
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Self {
            origin: config.origin.clone(),
            token: config.metrics_token.clone(),
            clients,
        })
    }
}

#[async_trait::async_trait]
impl Http for NativeHttp {
    async fn ready(&self) -> Result<bool> {
        let response = self.clients[0]
            .get(format!("{}/ready", self.origin))
            .timeout(Duration::from_millis(500))
            .send()
            .await?;
        let json: Value = serde_json::from_slice(&body(response, 65_536).await?)?;
        Ok(json["status"] == "ready")
    }
    async fn post(
        &self,
        index: usize,
        path: &str,
        payload: &Value,
        token: Option<&str>,
    ) -> Result<Value> {
        ensure!(
            matches!(path, "/api/auth/register" | "/api/rooms"),
            "invalid_setup_path"
        );
        let client = self
            .clients
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("invalid_setup_source_address"))?;
        let mut request = client
            .post(format!("{}{path}", self.origin))
            .header("Origin", &self.origin)
            .header("Content-Type", "application/json")
            .body(serde_json::to_vec(payload)?);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let bytes = body(request.send().await?, 65_536).await?;
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid_setup_response"))
    }
    async fn metrics(&self) -> Result<String> {
        let response = self.clients[0]
            .get(format!("{}/metrics", self.origin))
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(2))
            .send()
            .await?;
        String::from_utf8(body(response, 4 * 1024 * 1024).await?)
            .map_err(|_| anyhow::anyhow!("invalid_metrics_response"))
    }
}
