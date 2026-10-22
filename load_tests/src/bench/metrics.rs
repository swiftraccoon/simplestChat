//! Private loopback health and metrics requests for an explicitly owned server.

use anyhow::{Result, ensure};
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub raw: String,
    pub values: BTreeMap<String, f64>,
}

impl Snapshot {
    pub fn cleaned(&self) -> bool {
        ["connections", "rooms", "participants"]
            .iter()
            .all(|name| self.values.get(&format!("simplestchat_{name}_active")) == Some(&0.0))
    }
}

#[async_trait::async_trait]
pub trait Metrics: Send + Sync {
    async fn healthy(&self, origin: &str) -> Result<bool>;
    async fn fetch(&self, origin: &str, token: &str) -> Result<Snapshot>;
}

pub struct HttpMetrics {
    client: reqwest::Client,
}

fn target(origin: &str, path: &str) -> Result<url::Url> {
    let mut url =
        url::Url::parse(origin).map_err(|_| anyhow::anyhow!("Invalid owned metrics origin"))?;
    ensure!(
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "Invalid owned metrics origin"
    );
    url.set_path(path);
    Ok(url)
}

impl HttpMetrics {
    /// Redirects are disabled so the private metrics capability never leaves the
    /// exact owned loopback origin. Both response bodies and time are bounded.
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(2))
                .build()?,
        })
    }
}

#[async_trait::async_trait]
impl Metrics for HttpMetrics {
    async fn healthy(&self, origin: &str) -> Result<bool> {
        let url = target(origin, "/health")?;
        let response = self
            .client
            .get(url)
            .timeout(Duration::from_millis(500))
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Owned server health unavailable"))?;
        Ok(response.status().is_success())
    }
    async fn fetch(&self, origin: &str, token: &str) -> Result<Snapshot> {
        let url = target(origin, "/metrics")?;
        let mut response = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Owned server metrics unavailable"))?;
        ensure!(
            response.status().is_success(),
            "Owned server metrics unavailable"
        );
        const MAX_BYTES: usize = 4 * 1024 * 1024;
        ensure!(
            response
                .content_length()
                .is_none_or(|size| size <= MAX_BYTES as u64),
            "Owned metrics body limit"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("Owned server metrics unavailable"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= MAX_BYTES,
                "Owned metrics body limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        let raw = String::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!("Invalid owned metrics encoding"))?;
        let mut values = BTreeMap::new();
        for line in raw.lines() {
            let Some((key, value)) = line.split_once(' ') else {
                continue;
            };
            if !key.strip_prefix("simplestchat_").is_some_and(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            }) || !value.as_bytes().first().is_some_and(u8::is_ascii_digit)
            {
                continue;
            }
            if let Some(value) = super::options::number(value.split(' ').next().unwrap()) {
                values.insert(key.to_owned(), value);
            }
        }
        Ok(Snapshot { raw, values })
    }
}
