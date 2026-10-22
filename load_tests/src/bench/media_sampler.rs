//! Sequential, bounded native observations over the runner's monotonic clock.
//! Missing samples and persistence failures remain explicit coverage evidence.

use super::{artifacts, options::Options};
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

pub fn schedule(options: &Options) -> Vec<f64> {
    let start = f64::from(options.ramp_up + options.warmup) * 1000.0;
    let length = f64::from(options.duration) * 1000.0;
    let mut times = vec![start, start + length / 2.0];
    if length >= 4000.0 {
        times.push(start + length - 2000.0);
    }
    times.sort_by(f64::total_cmp);
    times.dedup();
    let mut result = Vec::new();
    for time in times {
        if result
            .last()
            .is_none_or(|previous| time - previous >= 250.0)
        {
            result.push(time);
        }
    }
    result
}

#[async_trait::async_trait]
pub trait Media: Send + Sync {
    /// Only a validated snapshot or fixed allowlisted failure label is returned.
    async fn fetch(&self, origin: &str, token: &str) -> Value;
}

pub struct HttpMedia {
    client: reqwest::Client,
}

impl HttpMedia {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(2))
                .build()?,
        })
    }

    async fn request(&self, origin: &str, token: &str) -> std::result::Result<Value, &'static str> {
        let started = std::time::Instant::now();
        let mut url = url::Url::parse(origin).map_err(|_| "invalid_media_target")?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("invalid_media_target");
        }
        url.set_path("/diagnostics/media");
        let mut response = self
            .client
            .get(url)
            .bearer_auth(token)
            .header(reqwest::header::CACHE_CONTROL, "no-store")
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    "media_timeout"
                } else {
                    "media_request_failed"
                }
            })?;
        match response.status().as_u16() {
            200..=299 => {}
            404 => return Err("media_unavailable"),
            429 => return Err("media_busy"),
            401 | 403 => return Err("media_unauthorized"),
            _ => return Err("media_http_error"),
        }
        const MAXIMUM: usize = 4 * 1024 * 1024;
        if response
            .content_length()
            .is_some_and(|length| length > MAXIMUM as u64)
        {
            return Err("media_body_limit");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                "media_timeout"
            } else {
                "media_request_failed"
            }
        })? {
            if bytes.len() + chunk.len() > MAXIMUM {
                return Err("media_body_limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        let snapshot = serde_json::from_slice(&bytes).map_err(|_| "invalid_media_json")?;
        crate::report::validate_media_snapshot(&snapshot).map_err(|_| "invalid_media_schema")?;
        if started.elapsed() >= Duration::from_secs(2) {
            return Err("media_timeout");
        }
        Ok(snapshot)
    }
}

#[async_trait::async_trait]
impl Media for HttpMedia {
    async fn fetch(&self, origin: &str, token: &str) -> Value {
        // The timeout includes body decoding and schema validation. The owned
        // future is dropped on expiry; redirects/proxies cannot carry the token.
        match tokio::time::timeout(Duration::from_secs(2), self.request(origin, token)).await {
            Ok(Ok(snapshot)) => json!({"status":"ok","snapshot":snapshot}),
            Ok(Err(issue)) => json!({"status":"error","issue":issue}),
            Err(_) => json!({"status":"error","issue":"media_timeout"}),
        }
    }
}

pub struct Sampler {
    pub samples: Vec<Value>,
    directory: PathBuf,
    schedule: Vec<f64>,
    start: f64,
    end: f64,
    index: usize,
    last_start: f64,
}

impl Sampler {
    pub fn new(options: &Options, directory: PathBuf) -> Self {
        Self {
            samples: Vec::new(),
            directory,
            schedule: schedule(options),
            start: f64::from(options.ramp_up + options.warmup) * 1000.0,
            end: f64::from(options.ramp_up + options.warmup + options.duration) * 1000.0,
            index: 0,
            last_start: f64::NEG_INFINITY,
        }
    }

    fn missed(&mut self, now: f64, alive: Value, issue: &str) -> Value {
        let sample = json!({"schemaVersion":1,"windowStartElapsedMs":self.start,
            "windowEndElapsedMs":self.end,"ordinal":self.index+1,
            "scheduledElapsedMs":self.schedule[self.index],"startedElapsedMs":null,
            "finishedElapsedMs":now,"aliveAtStart":alive,"aliveAtEnd":alive,
            "status":"missed","issue":issue});
        self.index += 1;
        sample
    }

    /// Return at most one due observation. A missed observation has its status
    /// already set; a live one must be completed once after the awaited request.
    pub fn prepare(&mut self, now: f64, alive: Value) -> Option<Value> {
        if self.index == self.schedule.len()
            || now < self.schedule[self.index]
            || now - self.last_start < 250.0
        {
            return None;
        }
        if alive["server"] != true || alive["generator"] != true || now >= self.end {
            return Some(self.missed(now, alive, "media_sample_not_live"));
        }
        let sample = json!({"schemaVersion":1,"windowStartElapsedMs":self.start,
            "windowEndElapsedMs":self.end,"ordinal":self.index+1,
            "scheduledElapsedMs":self.schedule[self.index],"startedElapsedMs":now,
            "aliveAtStart":alive});
        self.index += 1;
        self.last_start = now;
        Some(sample)
    }

    pub fn complete(&self, sample: &mut Value, result: Value, now: f64, alive: Value) {
        // Media implementations share this fixed result contract. Untrusted
        // native JSON was validated before it entered the result.
        if result["status"] == "ok" {
            sample["status"] = json!("ok");
            sample["snapshot"] = result["snapshot"].clone();
        } else {
            sample["status"] = json!("error");
            sample["issue"] = result["issue"].clone();
        }
        sample["finishedElapsedMs"] = json!(now);
        sample["aliveAtEnd"] = alive.clone();
        if sample["startedElapsedMs"].as_f64().unwrap()
            - sample["scheduledElapsedMs"].as_f64().unwrap()
            > 1000.0
        {
            sample["timingIssue"] = json!("media_sample_late");
        }
        if alive["server"] != true || alive["generator"] != true || now >= self.end {
            sample["timingIssue"] = json!("media_sample_crossed_teardown");
        }
    }

    pub fn save(&mut self, mut sample: Value) {
        let name = format!(
            "server-media-sample-{:02}.json",
            sample["ordinal"].as_u64().unwrap()
        );
        if artifacts::json(&self.directory.join(name), &sample).is_err() {
            sample["persistenceIssue"] = json!("media_sample_write_failed");
        }
        self.samples.push(sample);
    }

    pub fn finish(&mut self, now: f64, alive: Value) {
        while self.index < self.schedule.len() {
            let sample = self.missed(now, alive.clone(), "media_sample_missed");
            self.save(sample);
        }
    }
}
