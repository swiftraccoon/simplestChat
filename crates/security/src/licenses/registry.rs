//! What the package registries say about a locked dependency.
//!
//! One session asks three hosts and no other, over HTTPS and without
//! credentials, through the host's `curl`. Every answer is bounded, the
//! session has a budget of fetches and of time, and an address that was
//! answered is not asked for again. Nothing a registry says is trusted for
//! itself: an answer counts only where it agrees with what the lock pins.

use super::Dependency;
use crate::{
    Fault,
    context::{Context, Outcome, Run},
    digest,
    fast::Programs,
    record, require,
};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    rc::Rc,
    time::Duration,
};

/// The hosts a session asks.
const HOSTS: [&str; 3] = ["pypi.org", "files.pythonhosted.org", "registry.npmjs.org"];
/// The most seconds one fetch has, and the seconds a connection has.
const PATIENCE: (u64, &str) = (30, "10");
/// How many bytes of complaints a fetch may make.
const COMPLAINTS: u64 = 16384;
/// What a fetch that was answered ends with: the status on a line of its own.
const ANSWERED: &[u8] = b"\n200";

/// The bounds of one session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The most bytes of a page of the package index.
    pub index: u64,
    /// The most bytes of the metadata of one package.
    pub metadata: u64,
    /// The most fetches.
    pub fetches: usize,
    /// How many seconds the session lasts.
    pub seconds: u64,
    /// The most bytes of answers that are kept.
    pub kept: u64,
    /// The most files a page of the package index lists.
    pub files: usize,
}

impl Bounds {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        index: 16 * 1024 * 1024,
        metadata: 1024 * 1024,
        fetches: 512,
        seconds: 240,
        kept: 64 * 1024 * 1024,
        files: 50_000,
    };
}

/// One session with the registries.
pub struct Registry<'a> {
    programs: &'a dyn Programs,
    since: &'a dyn Fn() -> Duration,
    bounds: Bounds,
    requests: usize,
    kept: u64,
    answers: BTreeMap<String, Rc<[u8]>>,
}

/// How long a fetch may take when `curl` gives itself `left` seconds: two
/// more, so that its own limit is the one that ends it.
fn within(left: u64) -> Duration {
    Duration::from_secs(left + 2)
}

/// Whether `url` is a path of one of the three hosts over HTTPS, written in
/// visible ASCII with neither a query nor a fragment.
fn origin(url: &str) -> bool {
    let path = url.strip_prefix("https://").and_then(|rest| {
        let below = |host: &&str| rest.strip_prefix(*host)?.strip_prefix('/');
        HOSTS.iter().find_map(below)
    });
    let plain = |byte: u8| byte.is_ascii_graphic() && !matches!(byte, b'?' | b'#');
    path.is_some_and(|path| path.bytes().all(plain))
}

impl<'a> Registry<'a> {
    /// A session that finds `curl` through `programs` and has lasted for
    /// what `since` answers.
    pub fn new(
        programs: &'a dyn Programs,
        since: &'a dyn Fn() -> Duration,
        bounds: Bounds,
    ) -> Self {
        Self {
            programs,
            since,
            bounds,
            requests: 0,
            kept: 0,
            answers: BTreeMap::new(),
        }
    }

    /// The bounds of the session.
    pub fn bounds(&self) -> &Bounds {
        &self.bounds
    }

    /// How many fetches were begun, refused ones among them.
    pub fn requests(&self) -> usize {
        self.requests
    }

    /// What `url` answers, of at most `limit` bytes and never nothing. An
    /// address that was answered before is answered from what was kept.
    pub fn fetch(&mut self, context: &Context, url: &str, limit: u64) -> Outcome<Rc<[u8]>> {
        if let Some(answer) = self.answers.get(url) {
            return Ok(Rc::clone(answer));
        }
        require(origin(url), "dependency_registry_origin")?;
        // A second that has begun is spent.
        let spent = (self.since)();
        let begun = spent.as_secs() + u64::from(spent.subsec_nanos() > 0);
        let left = self.bounds.seconds.saturating_sub(begun).min(PATIENCE.0);
        self.requests += 1;
        require(
            left > 0 && self.requests <= self.bounds.fetches,
            "dependency_registry_budget",
        )?;
        let curl = self.programs.prerequisite("curl")?;
        let (seconds, size) = (left.to_string(), limit.to_string());
        let argv = [
            curl.as_os_str(),
            OsStr::new("--disable"),
            OsStr::new("--fail"),
            OsStr::new("--silent"),
            OsStr::new("--show-error"),
            OsStr::new("--proto"),
            OsStr::new("=https"),
            OsStr::new("--tlsv1.2"),
            OsStr::new("--connect-timeout"),
            OsStr::new(PATIENCE.1),
            OsStr::new("--max-time"),
            OsStr::new(&seconds),
            OsStr::new("--max-filesize"),
            OsStr::new(&size),
            OsStr::new("--header"),
            OsStr::new("Accept: application/vnd.pypi.simple.v1+json, application/json"),
            OsStr::new("--write-out"),
            OsStr::new("\n%{http_code}"),
            OsStr::new(url),
        ];
        let how = Run {
            within: within(left),
            ..Run::default()
        };
        let room = limit + ANSWERED.len() as u64;
        let (status, mut said) = context.ask(&argv, &how, (room, COMPLAINTS))?;
        require(
            status == 0 && said.ends_with(ANSWERED),
            "dependency_registry_fetch",
        )?;
        said.truncate(said.len() - ANSWERED.len());
        require(!said.is_empty(), "dependency_registry_size")?;
        self.kept += said.len() as u64;
        require(
            self.kept <= self.bounds.kept,
            "dependency_registry_cache_size",
        )?;
        let answer: Rc<[u8]> = said.into();
        self.answers.insert(url.to_owned(), Rc::clone(&answer));
        Ok(answer)
    }
}

/// What a registry says of one artifact of a dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// The npm registry's document of the locked version.
    Npm {
        /// The licence the document and the lock both declare.
        license: String,
        /// Where the document is.
        source: String,
        /// The SHA-256 of the document.
        metadata: String,
    },
    /// A locked wheel as the package index lists it.
    Wheel {
        /// The licence its metadata declares, or the reviewed one.
        license: String,
        /// Where the wheel is.
        source: String,
        /// The SHA-256 of the wheel.
        wheel: String,
        /// The SHA-256 of its metadata.
        metadata: String,
    },
    /// A locked source distribution: recorded, and never installed from.
    Sdist {
        /// Where the source distribution is.
        source: String,
        /// Its SHA-256.
        artifact: String,
    },
}

/// Why a source distribution needs no licence of its own here.
const WHEEL_ONLY: &str = "wheel-only-installation";

impl Evidence {
    /// The evidence as the receipt holds it.
    pub fn record(&self) -> Value {
        match self {
            Self::Npm {
                license,
                source,
                metadata,
            } => json!({"license": license, "source": source, "metadataSha256": metadata}),
            Self::Wheel {
                license,
                source,
                wheel,
                metadata,
            } => json!({
                "kind": "wheel",
                "license": license,
                "source": source,
                "wheelSha256": wheel,
                "metadataSha256": metadata,
            }),
            Self::Sdist { source, artifact } => json!({
                "kind": "sdist",
                "excluded": WHEEL_ONLY,
                "source": source,
                "artifactSha256": artifact,
            }),
        }
    }
}

/// `text` as one step of a path: every byte but a letter, a digit and
/// `-._~` is escaped.
fn step(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            escaped.push(char::from(byte));
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

/// The licence of the npm package `dependency`: the registry's document of
/// that version names the lock's tarball and integrity, and declares the
/// licence the lock declares.
pub fn npm_license(
    context: &Context,
    registry: &mut Registry<'_>,
    dependency: &Dependency,
) -> Outcome<Vec<Evidence>> {
    const IDENTITY: Fault = Fault("dependency_npm_registry_identity");
    const LICENSE: Fault = Fault("dependency_npm_license_identity");
    let (name, version) = (step(&dependency.name), step(&dependency.version));
    let url = format!("https://registry.npmjs.org/{name}/{version}");
    let content = registry.fetch(context, &url, registry.bounds.metadata)?;
    let document = record::strict(&content).map_err(|_| IDENTITY)?;
    let raw = document.as_object().ok_or(IDENTITY)?;
    let dist = raw.get("dist").and_then(Value::as_object).ok_or(IDENTITY)?;
    let said = |record: &Map<String, Value>, field: &str| {
        let text = record.get(field).and_then(Value::as_str);
        text.map(str::to_owned)
    };
    let pinned = |integrity: String| dependency.hashes == BTreeSet::from([integrity]);
    require(
        said(raw, "name").as_ref() == Some(&dependency.name)
            && said(raw, "version").as_ref() == Some(&dependency.version)
            && said(dist, "tarball").as_ref() == Some(&dependency.source)
            && said(dist, "integrity").is_some_and(pinned),
        IDENTITY.0,
    )?;
    let declared = |license: &String| *license == dependency.license && !license.is_empty();
    let license = said(raw, "license").filter(declared).ok_or(LICENSE)?;
    Ok(vec![Evidence::Npm {
        license,
        source: url,
        metadata: digest(&content),
    }])
}

#[cfg(test)]
mod tests {
    use super::within;
    use std::time::Duration;

    #[test]
    fn a_fetch_outlasts_the_limit_curl_is_given_by_two_seconds() {
        assert_eq!(within(1), Duration::from_secs(3));
        assert_eq!(within(30), Duration::from_secs(32));
    }
}
