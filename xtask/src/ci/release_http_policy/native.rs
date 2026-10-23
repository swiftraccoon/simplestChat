//! Mature HTTPS transport. Client/task Drop is not an observed internal close receipt.

use super::{
    Result, fail,
    policy::{self, Endpoint},
};
use reqwest::{Certificate, Client, redirect, retry, tls};
use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

pub(super) struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
pub(super) trait Http {
    async fn request(&self, endpoint: Endpoint) -> Result<Response>;
}
pub(super) struct Native {
    client: Client,
}
impl Native {
    pub fn new(ca: &[u8]) -> Result<Self> {
        let certificates = Certificate::from_pem_bundle(ca).map_err(|_| fail(1, "fixture_tls"))?;
        if certificates.is_empty() {
            return Err(fail(1, "fixture_tls"));
        }
        let client = Client::builder()
            .tls_certs_only(certificates)
            .tls_version_min(tls::Version::TLS_1_2)
            .https_only(true)
            .http1_only()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .retry(retry::never())
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .pool_max_idle_per_host(0)
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(Duration::from_secs(3))
            .resolve(
                "localhost",
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443),
            )
            .build()
            .map_err(|_| fail(1, "fixture_tls"))?;
        Ok(Self { client })
    }
}
fn transport(_: reqwest::Error) -> crate::tooling::Failure {
    fail(1, "http_policy_failed")
}
// HTTP header values in http.client are ISO-8859-1 strings, not UTF-8 strings.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|b| char::from(*b)).collect()
}
impl Http for Native {
    async fn request(&self, endpoint: Endpoint) -> Result<Response> {
        if !policy::ENDPOINTS.contains(&endpoint) {
            return Err(fail(1, "http_unlisted_endpoint"));
        }
        let mut response = self
            .client
            .get(format!("https://localhost{}", endpoint.path))
            .header("Host", "localhost")
            .header("Accept", endpoint.content_type)
            .header("Accept-Encoding", "identity")
            .header("Connection", "close")
            .send()
            .await
            .map_err(transport)?;
        // These are accepted parsed-header limits; the client parser owns its
        // own internal allocation limits before this observation is available.
        let values = response
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), latin1(v.as_bytes())))
            .collect::<Vec<_>>();
        let headers = policy::headers(&values)?;
        content_length(&headers, endpoint.limit)?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport)? {
            let remaining = (endpoint.limit + 1).saturating_sub(body.len());
            body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            if body.len() > endpoint.limit {
                break;
            }
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}
pub(super) fn content_length(headers: &BTreeMap<String, String>, maximum: usize) -> Result<()> {
    if let Some(length) = headers.get("content-length") {
        // Parsed HTTP Content-Length is an ASCII wire number in reqwest; the
        // policy seam also accepts Unicode decimal digits from Python fixtures.
        let mut n = 0_usize;
        if length.is_empty() {
            return Err(fail(1, "http_content_length"));
        }
        if length.chars().any(|c| decimal(c).is_none()) {
            return Err(fail(1, "http_content_length"));
        }
        // CPython's default int conversion limit applies before comparison,
        // including leading zeros. A conversion ValueError is the generic code.
        if length.chars().count() > 4300 {
            return Err(fail(1, "http_policy_failed"));
        }
        for c in length.chars() {
            let d = decimal(c).ok_or_else(|| fail(1, "http_content_length"))?;
            n = n
                .checked_mul(10)
                .and_then(|n| n.checked_add(d as usize))
                .filter(|n| *n <= maximum)
                .ok_or_else(|| fail(1, "http_content_length"))?;
        }
    }
    Ok(())
}
fn decimal(c: char) -> Option<u32> {
    // Every Unicode Nd block consists of one or more contiguous sets of ten.
    // Rust's regex tables are the reviewed Unicode16 profile used elsewhere.
    static DIGIT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = DIGIT.get_or_init(|| regex::Regex::new(r"^\p{Nd}$").expect("fixed decimal regex"));
    if !re.is_match(c.encode_utf8(&mut [0; 4])) {
        return None;
    }
    let mut first = c as u32;
    while first > 0
        && char::from_u32(first - 1).is_some_and(|c| re.is_match(c.encode_utf8(&mut [0; 4])))
    {
        first -= 1;
    }
    Some((c as u32 - first) % 10)
}
