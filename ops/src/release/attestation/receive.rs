//! Complete host receive, private evidence and immutable pair publication.
//! Caller/SSH authority is separate from the passive envelope's typed shape.
mod channel;
mod download;
mod options;
#[cfg(test)]
mod oracle_tests;
#[cfg(test)]
mod tests;

use super::{
    bindings,
    control::{self, Cause, Owner, Result, Window},
    files::{self, Dir, Retained},
    verify::{tool::Environment, zip},
};
use crate::{
    json::Json,
    release::{manifest::Manifest, trust},
};
use channel::{Channel, Standard};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    HttpRead,
    Write,
    Flush,
    Fsync,
    ExtractWrite,
    FirstLink,
    SecondLink,
    RaceEqual,
    RaceDifferent,
    RaceHardlink,
    ReplacePublishedAlias,
    CapathMember,
    PublishSync,
    ReceiptCreate,
    ReceiptSync,
    Interrupt,
    Alarm,
}
struct Access {
    root: PathBuf,
    config: PathBuf,
    work: PathBuf,
    ca: PathBuf,
    uid: u32,
    environment: Environment,
    #[cfg(test)]
    free_bytes: Option<u64>,
    #[cfg(test)]
    fault: Option<Fault>,
}
impl Access {
    fn fixed() -> Self {
        Self {
            root: "/srv/simplestchat-public".into(),
            config: "/etc/simplestchat-public".into(),
            work: "/run/simplestchat-bench".into(),
            ca: "/etc/ssl/certs/ca-certificates.crt".into(),
            uid: 0,
            environment: std::env::vars_os().collect(),
            #[cfg(test)]
            free_bytes: None,
            #[cfg(test)]
            fault: None,
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    schema_version: u8,
    status: &'static str,
    passed: bool,
    settled: bool,
    phase: &'static str,
    failure_class: Option<&'static str>,
    evidence: Option<String>,
    revision: Option<String>,
    artifact_id: Option<i64>,
    zip_sha256: Option<String>,
    artifact_zip_bytes: Option<u64>,
    downloaded_bytes: u64,
    archive_sha256: Option<String>,
    manifest_sha256: Option<String>,
    elapsed_seconds: f64,
    finalized: bool,
}
impl Report {
    fn new() -> Self {
        Self {
            schema_version: 1,
            status: "failed",
            passed: false,
            settled: false,
            phase: "envelope",
            failure_class: None,
            evidence: None,
            revision: None,
            artifact_id: None,
            zip_sha256: None,
            artifact_zip_bytes: None,
            downloaded_bytes: 0,
            archive_sha256: None,
            manifest_sha256: None,
            elapsed_seconds: 0.0,
            finalized: false,
        }
    }
    fn encode(&self, pretty: bool) -> Result<Vec<u8>> {
        let mut bytes = if pretty {
            serde_json::to_vec_pretty(self)
        } else {
            serde_json::to_vec(self)
        }
        .map_err(|_| Cause::Policy)?;
        if bytes.len() >= 131072 {
            return Err(Cause::Bounds);
        }
        bytes.push(b'\n');
        Ok(bytes)
    }
    fn failed(&mut self, class: &'static str) {
        self.status = "failed";
        self.passed = false;
        self.failure_class = Some(class);
    }
}
struct State {
    root: Option<Dir>,
    config: Option<Dir>,
    releases: Option<Dir>,
    results: Option<Dir>,
    work: Option<Dir>,
    work_parent: Option<Dir>,
    destination: Option<Dir>,
    attempt: Option<Dir>,
    lock: Option<Retained>,
    envelope: Option<Retained>,
    zip: Option<Retained>,
    members: [Option<Retained>; 10],
    targets: [Option<Retained>; 2],
    downloader: download::Downloader,
    report: Report,
    refusal: &'static str,
}
impl State {
    fn new() -> Self {
        Self {
            root: None,
            config: None,
            releases: None,
            results: None,
            work: None,
            work_parent: None,
            destination: None,
            attempt: None,
            lock: None,
            envelope: None,
            zip: None,
            members: std::array::from_fn(|_| None),
            targets: std::array::from_fn(|_| None),
            downloader: download::Downloader::new(),
            report: Report::new(),
            refusal: "invalid_request",
        }
    }
    fn file(&self, name: &str) -> &Retained {
        self.members[zip::SELECTED
            .iter()
            .position(|value| *value == name)
            .unwrap()]
        .as_ref()
        .unwrap()
    }
    fn read(&mut self, name: &str, owner: &mut Owner) -> Result<Json> {
        let bytes = self.file(name).bytes(bindings::METADATA, owner)?;
        decode(&bytes, &mut self.refusal, owner)
    }
    fn originals(&self, owner: &mut Owner) -> Result<()> {
        for dir in [
            &self.root,
            &self.config,
            &self.releases,
            &self.results,
            &self.work,
            &self.destination,
            &self.attempt,
        ]
        .into_iter()
        .flatten()
        {
            dir.check(owner, Window::Body)?;
        }
        for file in self.targets.iter().chain(&self.members).flatten() {
            file.check(owner, Window::Body)?;
        }
        if let Some(file) = &self.envelope {
            file.check(owner, Window::Body)?;
        }
        if let Some(file) = &self.zip {
            file.check(owner, Window::Body)?;
        }
        if let Some(file) = &self.lock {
            file.check(owner, Window::Body)?;
        }
        Ok(())
    }
    fn guard_journal(
        &mut self,
        dir: &Dir,
        name: &str,
        access: &Access,
        owner: &mut Owner,
    ) -> Result<()> {
        if !dir.exists(OsStr::new(name), owner)? {
            return Ok(());
        }
        let file = Retained::open(dir.clone(), name, owner)?;
        let result = (|| {
            file.protected(access.uid, owner)?;
            let value = decode(&file.bytes(16384, owner)?, &mut self.refusal, owner)?;
            require(
                owner,
                value.get("schemaVersion").and_then(Json::integer) == Some(1)
                    && value.get("finalized").and_then(Json::boolean) == Some(true),
            )
        })();
        file.finish(owner);
        result
    }
    fn preflight(
        &mut self,
        access: &Access,
        revision: &str,
        envelope: &Json,
        owner: &mut Owner,
    ) -> Result<()> {
        self.refusal = "unsafe_filesystem";
        self.root = Some(Dir::open(&access.root, owner)?);
        self.root.as_ref().unwrap().protected(access.uid, owner)?;
        self.config = Some(Dir::open(&access.config, owner)?);
        self.config.as_ref().unwrap().protected(access.uid, owner)?;
        self.releases = Some(Dir::open(&access.root.join("releases"), owner)?);
        self.releases
            .as_ref()
            .unwrap()
            .protected(access.uid, owner)?;
        self.results = Some(Dir::open(&access.root.join("results"), owner)?);
        self.results
            .as_ref()
            .unwrap()
            .protected(access.uid, owner)?;
        let parent = access.work.parent().ok_or(Cause::Input)?;
        self.work_parent = Some(Dir::open(parent, owner)?);
        self.work = Some(
            self.work_parent
                .as_ref()
                .unwrap()
                .child(access.work.file_name().ok_or(Cause::Input)?, owner)?,
        );
        let work = self.work.as_ref().unwrap().clone();
        work.protected(access.uid, owner)?;
        self.lock = Some(if work.exists(OsStr::new("workload.lock"), owner)? {
            Retained::open(work.clone(), "workload.lock", owner)?
        } else {
            let mut file = Retained::create(work.clone(), "workload.lock", owner)?;
            if let Err(cause) = file.write(b"", owner) {
                file.finish(owner);
                return Err(cause);
            }
            file
        });
        self.lock.as_ref().unwrap().protected(access.uid, owner)?;
        self.lock.as_ref().unwrap().lock(owner)?;
        self.refusal = "unfinished_operation";
        self.guard_journal(&work, "current.json", access, owner)?;
        let root = self.root.as_ref().unwrap().clone();
        self.guard_journal(&root, "release-state.json", access, owner)?;
        self.refusal = "unsafe_filesystem";
        self.destination = Some(
            self.releases
                .as_ref()
                .unwrap()
                .child(OsStr::new(revision), owner)?,
        );
        self.destination
            .as_ref()
            .unwrap()
            .protected(access.uid, owner)?;
        self.releases.as_ref().unwrap().sync(owner)?;
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| Cause::Io)?;
        owner.check(Window::Body)?;
        self.attempt = Some(self.destination.as_ref().unwrap().create_child(
            OsStr::new(&format!("download.{}", files::hex(&random))),
            owner,
        )?);
        let attempt = self.attempt.as_ref().unwrap();
        self.report.evidence = Some(attempt.path().to_str().ok_or(Cause::Input)?.to_owned());
        let mut bytes = serde_json::to_vec_pretty(envelope).map_err(|_| Cause::Policy)?;
        bytes.push(b'\n');
        self.envelope = Some(Retained::create(attempt.clone(), "envelope.json", owner)?);
        self.envelope.as_mut().unwrap().write(&bytes, owner)?;
        attempt.sync(owner)?;
        self.destination.as_ref().unwrap().sync(owner)?;
        self.refusal = "insufficient_disk";
        let free = self.destination.as_ref().unwrap().free_bytes(owner)?;
        #[cfg(test)]
        let free = access.free_bytes.unwrap_or(free);
        require(
            owner,
            free >= self.report.artifact_zip_bytes.unwrap()
                + zip::MAX_ZIP
                + 128 * 1024 * 1024
                + 1024 * 1024 * 1024,
        )
    }
    async fn perform(
        &mut self,
        access: &Access,
        channel: &mut impl Channel,
        owner: &mut Owner,
    ) -> Result<()> {
        owner.check(Window::Body)?;
        self.refusal = "root_required";
        require(owner, rustix::process::geteuid().as_raw() == access.uid)?;
        self.refusal = "invalid_request";
        let line = channel.line(owner).await?;
        let value = decode(&line, &mut self.refusal, owner)?;
        require(owner, value.object().is_some())?;
        self.refusal = "invalid_envelope";
        let envelope = trust::Envelope::decode(&line).map_err(|_| Cause::Policy)?;
        self.report.revision = Some(envelope.value().selection().revision().into());
        self.report.artifact_id = value.get("artifactId").and_then(Json::integer);
        self.report.zip_sha256 = value
            .get("zipSha256")
            .and_then(Json::string)
            .map(str::to_owned);
        self.report.artifact_zip_bytes = value
            .get("artifactZipBytes")
            .and_then(Json::integer)
            .and_then(|n| n.try_into().ok());
        self.report.phase = "preflight";
        self.preflight(
            access,
            envelope.value().selection().revision(),
            &value,
            owner,
        )?;
        self.report.phase = "await_url";
        self.refusal = "invalid_request";
        channel
            .write(
                b"{\"schemaVersion\":1,\"status\":\"ready\"}\n",
                false,
                owner,
            )
            .await?;
        let line = channel.line(owner).await?;
        let url = decode(&line, &mut self.refusal, owner)?;
        require(owner, url.object().is_some())?;
        self.refusal = "invalid_url";
        require(
            owner,
            url.object()
                .is_some_and(|o| o.len() == 1 && o.contains_key("url")),
        )?;
        let grant = super::acquire::grant::Grant::receiver(
            url.get("url").and_then(Json::string).ok_or(Cause::Policy)?,
            owner,
        )?;
        self.report.phase = "download";
        self.refusal = "transport_error";
        #[cfg(test)]
        {
            use super::acquire::transport::{ReceiverFault, receiver_fault};
            receiver_fault(match access.fault {
                Some(Fault::HttpRead) => Some(ReceiverFault::Read),
                Some(Fault::Interrupt) => Some(ReceiverFault::Interrupt),
                Some(Fault::Alarm) => Some(ReceiverFault::Deadline),
                Some(Fault::Flush) => Some(ReceiverFault::Flush),
                Some(Fault::Fsync) => Some(ReceiverFault::Fsync),
                _ => None,
            });
            if access.fault == Some(Fault::Write) {
                files::copy_fault("artifact.zip".into(), false, 3);
            }
        }
        let completed = self
            .downloader
            .run(
                access,
                self.attempt.as_ref().unwrap(),
                &grant,
                self.report.artifact_zip_bytes.unwrap(),
                self.report.zip_sha256.as_ref().unwrap(),
                owner,
            )
            .await;
        self.zip = completed.file;
        self.report.downloaded_bytes = completed.observation.body_bytes;
        if let Some(failure) = completed.observation.receiver_failure {
            self.refusal = failure;
        }
        #[cfg(test)]
        {
            files::clear_copy_fault();
            super::acquire::transport::receiver_fault(None);
            if matches!(
                access.fault,
                Some(Fault::HttpRead | Fault::Write | Fault::Flush | Fault::Fsync)
            ) {
                self.refusal = "filesystem_error";
            }
        }
        completed.result?;
        self.originals(owner)?;
        self.report.phase = "extract";
        self.refusal = "zip_shape_rejected";
        #[cfg(test)]
        if access.fault == Some(Fault::ExtractWrite) {
            files::copy_fault("image.tar".into(), false, 3);
        }
        let extracted = zip::receive(
            self.zip.as_ref().unwrap(),
            self.attempt.as_ref().unwrap(),
            &mut self.members,
            zip::Pin {
                bytes: self.report.artifact_zip_bytes.unwrap(),
                sha256: self.report.zip_sha256.as_ref().unwrap(),
            },
            owner,
            &mut self.refusal,
        );
        #[cfg(test)]
        files::clear_copy_fault();
        extracted?;
        self.report.phase = "verify";
        self.verify(&value, envelope.value(), owner)?;
        self.attempt.as_ref().unwrap().sync(owner)?;
        self.report.phase = "publish";
        self.refusal = "retained_artifact_differs";
        self.publish(access, owner)?;
        self.report.status = "complete";
        self.report.passed = true;
        self.report.phase = "complete";
        self.report.finalized = true;
        Ok(())
    }
    fn verify(
        &mut self,
        original: &Json,
        envelope: &trust::Envelope,
        owner: &mut Owner,
    ) -> Result<()> {
        let selected = envelope.selection();
        self.refusal = "build_outcome_rejected";
        let outcome = self.read("outcome.json", owner)?;
        require(
            owner,
            outcome.get("schemaVersion").and_then(Json::integer) == Some(1)
                && outcome.get("passed").and_then(Json::boolean) == Some(true)
                && matches!(outcome.get("error"), Some(Json::Null))
                && bindings::text(&outcome, "revision") == Some(selected.revision()),
        )?;
        self.refusal = "source_evidence_rejected";
        let source = self.read("source.json", owner)?;
        let inputs = source.get("inputsSha256").and_then(Json::object);
        let names = [
            "Dockerfile",
            ".dockerignore",
            "Cargo.lock",
            "web/package-lock.json",
            "build/pip-constraints.txt",
        ];
        require(
            owner,
            source.object().is_some_and(|o| o.len() == 2)
                && bindings::text(&source, "revision") == Some(selected.revision())
                && inputs.is_some_and(|o| {
                    o.len() == names.len()
                        && names
                            .iter()
                            .all(|n| o.get(*n).and_then(Json::string).is_some_and(|s| hex(s, 64)))
                }),
        )?;
        self.refusal = "validation_failed";
        let manifest = Manifest::decode(
            &self
                .file("release.json")
                .bytes(crate::release::manifest::MAX_BYTES, owner)?,
        )
        .map_err(|_| Cause::Policy)?;
        self.refusal = "release_revision_mismatch";
        require(owner, manifest.fields().revision == selected.revision())?;
        self.refusal = "validation_failed";
        self.members[0]
            .as_mut()
            .unwrap()
            .archive(&manifest, owner)?;
        let predicate = self.read("release-predicate.json", owner)?;
        let expected = original
            .get("attestation")
            .and_then(|a| a.get("predicate"))
            .ok_or(Cause::Policy)?;
        require(owner, predicate.semantic_eq(expected))?;
        let bundle = self.file("release-attestation.jsonl").hash(owner)?;
        require(
            owner,
            Some(bundle.as_str())
                == original
                    .get("attestation")
                    .and_then(|a| bindings::text(a, "bundleSha256")),
        )?;
        let mut digests = BTreeMap::new();
        for name in trust::BOUND_FILES {
            let file = self.file(name);
            file.bound(
                if name == "image.tar" {
                    bindings::IMAGE
                } else {
                    bindings::METADATA as u64
                },
                owner,
            )?;
            let digest = file.hash(owner)?;
            require(
                owner,
                Some(digest.as_str()) == envelope.proof().predicate().file_digest(name),
            )?;
            digests.insert(name.to_owned(), digest);
        }
        let image = bindings::text(expected, "imageId").ok_or(Cause::Policy)?;
        let security = self.read("image-security.json", owner)?;
        bindings::bind_security(&security, selected, image, &digests, owner)?;
        let required = bindings::runtime_security(&security, &digests, owner)?;
        let proof = self.read("runtime-proof.json", owner)?;
        bindings::runtime_proof(&proof, required, selected, image, &digests, owner)?;
        let vex = self.read("vex.openvex.json", owner)?;
        bindings::runtime_vex(&vex, required, owner)?;
        bindings::runtime_review(&security, &proof, required, owner)?;
        bindings::bind_outcome(&outcome, selected, image, owner)?;
        let manifest_value = self.read("release.json", owner)?;
        bindings::bind_manifest(&manifest_value, selected, &digests, owner)?;
        self.originals(owner)?;
        self.report.archive_sha256 = Some(manifest.fields().archive_sha256.clone());
        self.report.manifest_sha256 = Some(self.file("release.json").hash(owner)?);
        Ok(())
    }
    fn publish(&mut self, access: &Access, owner: &mut Owner) -> Result<()> {
        let destination = self.destination.as_ref().unwrap().clone();
        // Both existing names are checked before either own link is attempted.
        for name in ["release.json", "image.tar"] {
            if destination.exists(OsStr::new(name), owner)? {
                self.target(name, access, owner)?;
            }
        }
        for name in ["release.json", "image.tar"] {
            #[cfg(test)]
            self.publication_fault(name, access, owner)?;
            if destination.exists(OsStr::new(name), owner)? {
                self.target(name, access, owner)?;
            }
            let expected = if name == "image.tar" {
                self.report.archive_sha256.as_ref().unwrap()
            } else {
                self.report.manifest_sha256.as_ref().unwrap()
            };
            let actual = self.file(name).hash(owner)?;
            require(owner, actual == *expected)?;
            let at = zip::SELECTED.iter().position(|n| *n == name).unwrap();
            self.members[at]
                .as_mut()
                .unwrap()
                .publish(&destination, owner)?;
            self.target(name, access, owner)?;
        }
        #[cfg(test)]
        if access.fault == Some(Fault::PublishSync) {
            return control::failed(owner, Cause::Io);
        }
        self.originals(owner)?;
        for file in self.targets.iter().flatten() {
            file.check(owner, Window::Body)?;
        }
        destination.sync(owner)
    }
    #[cfg(test)]
    fn publication_fault(&mut self, name: &str, access: &Access, owner: &mut Owner) -> Result<()> {
        if (name == "release.json" && access.fault == Some(Fault::FirstLink))
            || (name == "image.tar" && access.fault == Some(Fault::SecondLink))
        {
            return control::failed(owner, Cause::Io);
        }
        if name == "image.tar" && access.fault == Some(Fault::ReplacePublishedAlias) {
            let destination = self.destination.as_ref().unwrap().path();
            control::io(
                owner,
                std::fs::rename(
                    destination.join("release.json"),
                    destination.join("retained-original.json"),
                ),
            )?;
            control::io(
                owner,
                std::fs::copy(
                    destination.join("retained-original.json"),
                    destination.join("release.json"),
                ),
            )?;
            control::io(
                owner,
                std::fs::set_permissions(
                    destination.join("release.json"),
                    std::os::unix::fs::PermissionsExt::from_mode(0o600),
                ),
            )?;
        }
        if name != "release.json" {
            return Ok(());
        }
        let target = self.destination.as_ref().unwrap().path().join(name);
        let source = self.attempt.as_ref().unwrap().path().join(name);
        match access.fault {
            Some(Fault::RaceHardlink) => control::io(owner, std::fs::hard_link(source, target)),
            Some(Fault::RaceEqual) => {
                control::io(owner, std::fs::copy(source, &target))?;
                control::io(
                    owner,
                    std::fs::set_permissions(
                        target,
                        std::os::unix::fs::PermissionsExt::from_mode(0o600),
                    ),
                )
            }
            Some(Fault::RaceDifferent) => {
                control::io(owner, std::fs::write(&target, b"racing-different"))?;
                control::io(
                    owner,
                    std::fs::set_permissions(
                        target,
                        std::os::unix::fs::PermissionsExt::from_mode(0o600),
                    ),
                )
            }
            _ => Ok(()),
        }
    }
    fn target(&mut self, name: &str, access: &Access, owner: &mut Owner) -> Result<()> {
        self.refusal = "unsafe_filesystem";
        self.destination
            .as_ref()
            .unwrap()
            .protected_file(OsStr::new(name), access.uid, owner)?;
        let target = usize::from(name == "image.tar");
        if self.targets[target].is_none() {
            self.targets[target] = Some(Retained::open(
                self.destination.as_ref().unwrap().clone(),
                name,
                owner,
            )?);
        }
        let file = self.targets[target].as_ref().unwrap();
        file.protected(access.uid, owner)?;
        self.refusal = "retained_artifact_differs";
        let at = zip::SELECTED.iter().position(|n| *n == name).unwrap();
        self.members[at]
            .as_mut()
            .unwrap()
            .publication_alias(file, owner)?;
        require(
            owner,
            file.len() == self.members[at].as_ref().unwrap().len(),
        )?;
        let actual = file.hash(owner)?;
        let expected = if name == "image.tar" {
            self.report.archive_sha256.as_ref().unwrap()
        } else {
            self.report.manifest_sha256.as_ref().unwrap()
        };
        require(owner, actual == *expected)?;
        let source = self.members[at].as_ref().unwrap().hash(owner)?;
        require(owner, source == *expected)
    }
    fn finish(&mut self, owner: &mut Owner) {
        for file in &mut self.targets {
            if let Some(file) = file.take() {
                file.finish(owner);
            }
        }
        for file in &mut self.members {
            if let Some(file) = file.take() {
                file.finish(owner);
            }
        }
        for file in [&mut self.zip, &mut self.envelope] {
            if let Some(file) = file.take() {
                file.finish(owner);
            }
        }
        self.downloader.finish(owner);
        // Closing the original locked descriptor releases the flock only after
        // the durable outcome attempt. A close refusal stays negative.
        if let Some(file) = self.lock.take() {
            file.finish(owner);
        }
        for dir in [
            &mut self.attempt,
            &mut self.destination,
            &mut self.results,
            &mut self.releases,
            &mut self.work,
            &mut self.work_parent,
            &mut self.config,
            &mut self.root,
        ] {
            if let Some(dir) = dir.take() {
                dir.finish(owner);
            }
        }
    }
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn require(owner: &mut Owner, condition: bool) -> Result<()> {
    if condition {
        owner.check(Window::Body)
    } else {
        control::failed(owner, Cause::Policy)
    }
}
fn decode(bytes: &[u8], refusal: &mut &'static str, owner: &mut Owner) -> Result<Json> {
    owner.check(Window::Body)?;
    crate::json::decode_limited(bytes, bindings::METADATA).map_err(|error| {
        *refusal = if error == crate::Error::AmbiguousJson {
            "ambiguous_json"
        } else {
            "validation_failed"
        };
        owner.record(Cause::Policy);
        Cause::Policy
    })
}
fn failure(cause: Cause, policy: &'static str) -> &'static str {
    match cause {
        Cause::Deadline => "deadline_exceeded",
        Cause::Interrupt | Cause::Terminate => "interrupted",
        Cause::Io | Cause::Close if policy == "transport_error" => "transport_error",
        Cause::Io | Cause::Close => "filesystem_error",
        _ => policy,
    }
}
async fn execute(
    access: &Access,
    channel: &mut impl Channel,
    owner: &mut Owner,
    started: Instant,
) -> bool {
    let mut state = State::new();
    let result = state.perform(access, channel, owner).await;
    if let Err(cause) = result {
        owner.record(cause);
        state.report.failed(failure(cause, state.refusal));
    }
    state.report.elapsed_seconds =
        (owner.now().saturating_duration_since(started).as_secs_f64() * 1000.0).round() / 1000.0;
    state.report.settled = state.attempt.is_some();
    if let Some(attempt) = &state.attempt {
        #[cfg(test)]
        let refuse_create = access.fault == Some(Fault::ReceiptCreate);
        #[cfg(not(test))]
        let refuse_create = false;
        let journal = if refuse_create {
            control::failed(owner, Cause::Io)
        } else {
            state
                .report
                .encode(true)
                .and_then(|bytes| attempt.receiver_journal(&bytes, owner))
                .and_then(|()| attempt.final_sync(owner))
        };
        #[cfg(test)]
        let journal = if matches!(access.fault, Some(Fault::ReceiptSync | Fault::PublishSync)) {
            control::failed(owner, Cause::Io)
        } else {
            journal
        };
        if journal.is_err() {
            state.report.failed("receipt_write_failed");
            state.report.settled = false;
        }
    }
    state.finish(owner);
    if result.is_ok()
        && state.report.failure_class.is_none()
        && let Some(cause) = owner.error()
    {
        state.report.failed(failure(cause, "filesystem_error"));
    }
    let bytes = state.report.encode(false);
    let output = match bytes {
        Ok(bytes) => channel.write(&bytes, true, owner).await,
        Err(cause) => {
            owner.record(cause);
            Err(cause)
        }
    };
    channel.finish(owner);
    state.report.passed
        && state.report.settled
        && state.report.finalized
        && output.is_ok()
        && owner.check(Window::Final).is_ok()
}
#[cfg(test)]
thread_local! {
    static COMPLETION_OFFSET: std::cell::Cell<Option<Duration>> = const {
        std::cell::Cell::new(None)
    };
    static FRAME_CLOSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static COMPLETION_AFTER_FRAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static RESTORE_SIGNAL: std::cell::Cell<Option<i32>> = const {
        std::cell::Cell::new(None)
    };
    static COMPLETION_CAUSE: std::cell::Cell<Option<Cause>> = const { std::cell::Cell::new(None) };
}
fn lifecycle(
    started: Instant,
    whole: Duration,
    operation: impl FnOnce() -> (bool, Option<Owner>),
) -> ExitCode {
    let Some(end) = started.checked_add(whole) else {
        return ExitCode::from(2);
    };
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let (passed, owner) = operation();
    nix::sys::stat::umask(previous);
    #[cfg(test)]
    if let Some(signal) = RESTORE_SIGNAL.take()
        && signal_hook::low_level::raise(signal).is_err()
    {
        return ExitCode::FAILURE;
    }
    let settled = owner.map_or(Err(Cause::Input), Owner::finish);
    #[cfg(test)]
    COMPLETION_CAUSE.set(settled.err());
    let completed = Instant::now();
    #[cfg(test)]
    let completed = match COMPLETION_OFFSET.get() {
        Some(offset) => {
            COMPLETION_AFTER_FRAME.set(FRAME_CLOSED.get());
            end.checked_add(offset).unwrap_or(end)
        }
        None => completed,
    };
    if passed && settled.is_ok() && completed < end {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
/// Receive one bounded envelope and fresh URL into private host evidence.
///
/// This dedicated synchronous Linux CLI retains failed evidence and publishes
/// only the verified manifest/image hardlink pair. It does not authenticate an
/// envelope, change installed routes, install an Ops upgrade, run an engine or
/// deploy. Explicit N/M include setup, transfer, verification, receipt, standard
/// descriptor restoration, signals, runtime and umask restoration. Nonzero exit
/// invalidates earlier Ready/report bytes, including late finalization failures.
/// Kernel filesystem/random/reap stalls are outside a hard wall guarantee.
///
/// # Panics
///
/// A valid operation must run outside an entered Tokio runtime. Starting or
/// dropping this dedicated runtime inside another runtime can panic.
///
/// ```
/// use simplestchat_ops::release::receive;
/// use std::{ffi::OsString,process::ExitCode};
/// assert_eq!(receive::run(&[OsString::from("--help")]),ExitCode::SUCCESS);
/// ```
pub fn run(arguments: &[OsString]) -> ExitCode {
    let started = Instant::now();
    if arguments.len() == 1 && arguments[0] == "--licenses" {
        return if super::acquire::licenses::stdout().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let options = match options::Options::parse(arguments) {
        Ok(None) => {
            return if super::prepare::stdout(format!("{}\n", options::USAGE).as_bytes()).is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Ok(Some(options)) => options,
        Err(_) => {
            let _ = writeln!(io::stderr().lock(), "{}", options::USAGE);
            return ExitCode::from(2);
        }
    };
    lifecycle(started, options.whole, || {
        let mut owner = match Owner::new(started, options.whole, options.reserve) {
            Ok(owner) => owner,
            Err(_) => return (false, None),
        };
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                owner.record(Cause::Io);
                return (false, Some(owner));
            }
        };
        let passed = runtime.block_on(async {
            let mut channel = match Standard::new(&mut owner) {
                Ok(channel) => channel,
                Err(cause) => {
                    owner.record(cause);
                    return false;
                }
            };
            execute(&Access::fixed(), &mut channel, &mut owner, started).await
        });
        drop(runtime);
        (passed, Some(owner))
    })
}
