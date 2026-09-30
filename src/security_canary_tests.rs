//! Bounded observation helpers for real in-process request canaries. These
//! helpers never print sensitive values, even when an assertion fails.

use base64::{Engine as _, engine::general_purpose};
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    overflow: bool,
}

#[derive(Clone, Default)]
pub(crate) struct Capture(Arc<Mutex<Captured>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut output = self.0.lock().unwrap();
        if bytes.len() > MAX_CAPTURE_BYTES.saturating_sub(output.bytes.len()) {
            output.overflow = true;
            return Err(io::Error::other("runtime canary capture limit exceeded"));
        }
        output.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Capture {
    pub(crate) fn text(&self) -> String {
        let output = self.0.lock().unwrap();
        assert!(!output.overflow, "runtime canary capture overflowed");
        String::from_utf8(output.bytes.clone())
            .unwrap_or_else(|_| panic!("observation output must be UTF-8"))
    }

    pub(crate) fn records(&self) -> Vec<serde_json::Value> {
        let text = self.text();
        assert!(!text.is_empty(), "runtime observation must not be empty");
        text.lines()
            .map(|line| serde_json::from_str(line).expect("complete JSON observation"))
            .collect()
    }

    pub(crate) fn subscriber(&self) -> tracing::Dispatch {
        let output = self.clone();
        tracing::Dispatch::new(
            tracing_subscriber::fmt()
                .json()
                .with_ansi(false)
                .with_env_filter("simplestChat=info,mediasoup=warn,sqlx::pool=warn")
                .with_writer(move || output.clone())
                .finish(),
        )
    }
}

#[derive(Default)]
pub(crate) struct SensitiveValues(Vec<(&'static str, String)>);

impl SensitiveValues {
    pub(crate) fn add(&mut self, label: &'static str, value: &str) {
        assert!(value.len() >= 12, "canary values must be distinctive");
        self.0.push((label, value.to_owned()));
    }

    pub(crate) fn assert_absent(&self, surface: &str, output: &str) {
        assert!(
            !self.0.is_empty(),
            "at least one sensitive value is required"
        );
        for (label, value) in &self.0 {
            // Check exact values and common standalone encodings. This does
            // not claim to detect arbitrary fragments or nested encodings.
            let encodings = [
                value.clone(),
                url::form_urlencoded::byte_serialize(value.as_bytes()).collect(),
                general_purpose::STANDARD.encode(value),
                general_purpose::URL_SAFE_NO_PAD.encode(value),
            ];
            for encoded in encodings {
                assert!(
                    !output.contains(&encoded),
                    "sensitive {label} appeared in {surface}"
                );
            }
        }
    }
}

#[test]
fn runtime_canary_capture_overflow_is_sticky_and_bounded() {
    let mut capture = Capture::default();
    capture.write_all(&vec![b'x'; MAX_CAPTURE_BYTES]).unwrap();
    assert!(capture.write_all(b"overflow").is_err());
    assert_eq!(capture.0.lock().unwrap().bytes.len(), MAX_CAPTURE_BYTES);
    assert!(std::panic::catch_unwind(|| capture.text()).is_err());
}

#[test]
fn runtime_canary_guard_rejects_raw_and_standalone_encoded_values() {
    let value = format!("GuardCanary-{}@example.invalid", uuid::Uuid::new_v4());
    let mut secrets = SensitiveValues::default();
    secrets.add("guard fixture", &value);
    for output in [
        value.clone(),
        url::form_urlencoded::byte_serialize(value.as_bytes()).collect(),
        general_purpose::STANDARD.encode(&value),
        general_purpose::URL_SAFE_NO_PAD.encode(&value),
    ] {
        assert!(
            std::panic::catch_unwind(|| secrets.assert_absent("guard fixture", &output)).is_err()
        );
    }
    secrets.assert_absent("safe fixture", "only a fixed operation name");
}

#[tokio::test]
async fn runtime_canary_subscriber_captures_events_across_pending_request_polls() {
    use tracing::instrument::WithSubscriber;
    let capture = Capture::default();
    async {
        tracing::info!("canary capture before yield");
        tokio::task::yield_now().await;
        tracing::warn!("canary capture after yield");
    }
    .with_subscriber(capture.subscriber())
    .await;
    let records = capture.records();
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0]["fields"]["message"],
        "canary capture before yield"
    );
    assert_eq!(
        records[1]["fields"]["message"],
        "canary capture after yield"
    );
}
