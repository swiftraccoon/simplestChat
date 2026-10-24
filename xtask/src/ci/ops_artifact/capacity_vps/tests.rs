//! Pure admission and real owned-file regressions. These tests run no SSH,
//! candidate, engine, Ansible, VM or paid workload.

use super::super::namespace::Budget;
use super::{
    archive,
    files::Output,
    inventory::{self, Inventory},
    options::Options,
    plan::{Clock, Phase, Plan},
    worker::Checkpoint,
};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) fn args(extra: &[&str]) -> Vec<String> {
    [
        "--inventory",
        "/private/inventory.yml",
        "--limit",
        "private_host",
        "--revision",
        &"a".repeat(40),
        "--ops-ci-run",
        "123",
        "--ops-run-attempt",
        "1",
        "--ops-artifact",
        "456",
    ]
    .into_iter()
    .chain(extra.iter().copied())
    .map(str::to_owned)
    .collect()
}
fn options(extra: &[&str]) -> crate::tooling::Result<Options> {
    Options::decode(&args(extra), Path::new("/caller"), Path::new("/home/owner"))
}
#[test]
fn artifact_selectors_are_distinct_canonical_ids_and_recovery_never_starts_work() {
    assert!(options(&[]).is_ok());
    for field in ["--ops-ci-run", "--ops-run-attempt", "--ops-artifact"] {
        for invalid in ["0", "01", "+1", "1.0", "1e3", "9007199254740992", "true"] {
            assert!(options(&[field, invalid]).is_err(), "{field} {invalid}");
        }
    }
    let run = "1".repeat(32);
    assert!(options(&["--collect", &run]).is_ok());
    for selector in ["--collect", "--recover"] {
        assert!(options(&[selector, &run, "--prepare"]).is_err());
        assert!(options(&[selector, &run, "--build-images"]).is_err());
    }
    assert!(options(&["--collect", &run, "--recover", &run]).is_err());
    let together =
        options(&["--runtime-seconds=120", "--server-cpus=1.5", "--audio-only"]).unwrap();
    let separate = options(&[
        "--runtime-seconds",
        "120",
        "--server-cpus",
        "1.5",
        "--audio-only",
    ])
    .unwrap();
    assert_eq!(
        together.request(&run).unwrap(),
        separate.request(&run).unwrap()
    );
    assert_eq!(
        options(&["--inventory", "/private/inventory=fixture.yml"])
            .unwrap()
            .inventory,
        Path::new("/private/inventory=fixture.yml")
    );
    assert_eq!(
        options(&["--inventory=/private/inventory=fixture.yml"])
            .unwrap()
            .inventory,
        Path::new("/private/inventory=fixture.yml")
    );
    for flag in [
        "--prepare=true",
        "--build-images=false",
        "--audio-only=",
        "--quick=--audio-only",
    ] {
        assert!(options(&[flag]).is_err());
    }
}
#[test]
fn workload_bounds_and_default_requests_use_the_existing_policy() {
    let selected = options(&[]).unwrap();
    let bytes = selected.request(&"1".repeat(32)).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["runtimeSeconds"], 7200);
    assert_eq!(value["firstSize"], 30);
    assert_eq!(value["speakers"], 30);
    for flags in [
        ["--runtime-seconds", "119"],
        ["--steps", "13"],
        ["--server-cpus", "NaN"],
        ["--first-size", "1"],
        ["--monthly-price", "0"],
    ] {
        assert!(options(&flags).is_err());
    }
    assert!(options(&["--first-size", "+3_0"]).is_ok());
    assert!(options(&["--chat-interval-ms", "-0_0"]).is_ok());
    assert!(options(&["--chat-interval-ms", "-1"]).is_err());
    assert!(options(&["--server-cpus", "+1_0.0"]).is_ok());
    for value in ["_1", "1_", "1__0", "1_.0", "1._0", "1e_1", "1_e1", "+_1"] {
        assert!(options(&["--server-cpus", value]).is_err());
    }
}
fn yaml(field: &str) -> Vec<u8> {
    format!("benchmark_hosts:\n  hosts:\n    target:\n      ansible_user: root\n      {field}\n")
        .into_bytes()
}
#[test]
fn inventory_uses_real_yaml_tokens_and_preserves_boolean_types() {
    let revision = "a".repeat(40);
    for value in ["false", "False", "no", "OFF"] {
        assert!(
            inventory::decode(
                &yaml(&format!("scpub_enabled: {value}")),
                "target",
                &revision
            )
            .is_ok()
        );
    }
    for value in ["true", "yes", "ON", "'false'", "0"] {
        assert!(
            inventory::decode(
                &yaml(&format!("scpub_enabled: {value}")),
                "target",
                &revision
            )
            .is_err()
        );
    }
    assert!(inventory::decode(&yaml("ansible_host: 'safe&name'"), "target", &revision).is_ok());
    for value in [
        "ansible_host: &host example",
        "ansible_host: *host",
        "ansible_host: !!str example",
        "ansible_host: '{{ host }}'",
        "ansible_host: a\n      ansible_host: b",
        "ansible_host: [a,b]",
        "unknown_hook: value",
    ] {
        assert!(
            inventory::decode(&yaml(value), "target", &revision).is_err(),
            "{value}"
        );
    }
}
#[test]
fn ignored_inventory_revision_is_explicitly_canonicalized_before_json_snapshot() {
    let revision = "b".repeat(40);
    for value in [".nan", ".inf", "-.inf", "false", "42", "'an-old-revision'"] {
        let original = yaml(&format!("scbench_revision: {value}"));
        let parsed = inventory::decode(&original, "target", &revision).unwrap();
        assert_eq!(parsed["scbench_revision"], revision);
        assert!(std::str::from_utf8(&original).unwrap().contains(value));
    }
    assert!(inventory::decode(&yaml("scpub_port_mbps: .nan"), "target", &revision).is_err());
}
struct FakeClock(Mutex<Instant>);
impl Clock for FakeClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}
#[test]
fn saved_runtime_tightens_original_entry_and_cannot_restart_the_clock() {
    let options = options(&["--collect", &"1".repeat(32)]).unwrap();
    let start = Instant::now();
    let clock = Arc::new(FakeClock(Mutex::new(start)));
    let plan = Plan::new(clock.clone(), &options, start);
    let original = plan.end();
    *clock.0.lock().unwrap() = start + Duration::from_secs(60);
    plan.tighten(120).unwrap();
    assert_eq!(original - plan.end(), Duration::from_secs(21600 - 120));
    assert!(plan.tighten(121).is_err());
    assert!(plan.tighten(119).is_err());
    assert!(plan.begin(Phase::Build).is_err());
    *clock.0.lock().unwrap() = start + Duration::from_secs(59);
    assert!(plan.now().is_err());
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    caller: PathBuf,
    budget: Budget,
    checkpoint: Checkpoint,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "vps-local-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        rustix::fs::chown(&root, None, Some(rustix::process::getgid())).unwrap();
        let caller = root.join("caller");
        fs::create_dir(&caller).unwrap();
        let clock: Arc<dyn Clock> = Arc::new(super::plan::NativeClock);
        let start = clock.now();
        Self {
            root,
            caller,
            budget: Budget::default(),
            checkpoint: Checkpoint {
                clock,
                start,
                end: start + Duration::from_secs(120),
                abort: Arc::new(AtomicBool::new(false)),
            },
        }
    }
    fn output(&self) -> Output {
        Output::new(
            &self.root.join("evidence"),
            &self.caller,
            &self.budget,
            &self.checkpoint,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.root.as_os_str().is_empty() {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
}
fn tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut output = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut output);
        for (name, bytes) in entries {
            let mut header = tar::Header::new_ustar();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            archive.append_data(&mut header, *name, *bytes).unwrap();
        }
        archive.finish().unwrap();
    }
    output
}
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut output = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    output.write_all(bytes).unwrap();
    output.finish().unwrap()
}
#[test]
fn extraction_streams_real_gzip_tar_and_keeps_changed_originals_negative() {
    let fixture = Fixture::new();
    let mut output = fixture.output();
    let bytes = gzip(&tar(&[("workload/outcome.json", b"{}"), ("empty", b"")]));
    let source = output
        .frame("archive.gz", &bytes, None, &[], &fixture.checkpoint)
        .unwrap();
    let extracted =
        archive::extract(&source, &output, &fixture.budget, &fixture.checkpoint).unwrap();
    assert_eq!(
        fs::read(output.path("remote/workload/outcome.json")).unwrap(),
        b"{}"
    );
    extracted
        .check(&fixture.budget, &fixture.checkpoint)
        .unwrap();
    fs::write(output.path("remote/workload/outcome.json"), b"[]").unwrap();
    assert!(
        extracted
            .check(&fixture.budget, &fixture.checkpoint)
            .is_err()
    );
    source.finish(&fixture.checkpoint).unwrap();
}
#[test]
fn malformed_gzip_trailers_and_duplicate_tar_members_cannot_be_success() {
    let good = gzip(&tar(&[("entry", b"data")]));
    let mut trailing = good.clone();
    trailing.extend_from_slice(&good);
    let mut truncated = good.clone();
    truncated.truncate(truncated.len() - 2);
    for bytes in [
        trailing,
        truncated,
        gzip(&tar(&[("entry", b"a"), ("entry", b"b")])),
    ] {
        let fixture = Fixture::new();
        let mut output = fixture.output();
        let source = output
            .frame("archive.gz", &bytes, None, &[], &fixture.checkpoint)
            .unwrap();
        assert!(archive::extract(&source, &output, &fixture.budget, &fixture.checkpoint).is_err());
        source.finish(&fixture.checkpoint).unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn unix_archive_names_keep_original_bytes_without_utf8_replacement() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let fixture = Fixture::new();
    let mut output = fixture.output();
    let name = Path::new(OsStr::from_bytes(b"raw-\xff/leaf-\xfe"));
    let mut builder = tar::Builder::new(Vec::new());
    let metadata = b"21 hdrcharset=BINARY\n21 path=raw-\xff/leaf-\xfe\n";
    let mut pax = tar::Header::new_ustar();
    pax.set_entry_type(tar::EntryType::XHeader);
    pax.set_size(metadata.len() as u64);
    pax.set_mode(0o600);
    pax.set_cksum();
    builder
        .append_data(&mut pax, "PaxHeader", &metadata[..])
        .unwrap();
    let mut header = tar::Header::new_ustar();
    header.set_size(4);
    header.set_mode(0o600);
    header.set_cksum();
    builder
        .append_data(&mut header, "placeholder", &b"data"[..])
        .unwrap();
    builder.finish().unwrap();
    let bytes = gzip(&builder.into_inner().unwrap());
    let source = output
        .frame("archive.gz", &bytes, None, &[], &fixture.checkpoint)
        .unwrap();
    let extracted =
        archive::extract(&source, &output, &fixture.budget, &fixture.checkpoint).unwrap();
    assert_eq!(fs::read(output.path("remote").join(name)).unwrap(), b"data");
    extracted
        .check(&fixture.budget, &fixture.checkpoint)
        .unwrap();
    source.finish(&fixture.checkpoint).unwrap();
    assert_eq!(fixture.budget.count(), 0);
}

#[test]
fn physical_tar_header_strings_keep_the_reference_strict_utf8_boundary() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let fixture = Fixture::new();
    let mut output = fixture.output();
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_ustar();
    header.set_size(4);
    header.set_mode(0o600);
    header.set_cksum();
    builder
        .append_data(
            &mut header,
            Path::new(OsStr::from_bytes(b"raw-\xff")),
            &b"data"[..],
        )
        .unwrap();
    builder.finish().unwrap();
    let bytes = gzip(&builder.into_inner().unwrap());
    let source = output
        .frame("archive.gz", &bytes, None, &[], &fixture.checkpoint)
        .unwrap();
    assert!(archive::extract(&source, &output, &fixture.budget, &fixture.checkpoint).is_err());
    assert!(!output.path("remote").exists());
    source.finish(&fixture.checkpoint).unwrap();
    assert_eq!(fixture.budget.count(), 0);
}

/// Test-only source and capture factory for the actual Native paired owner.
/// Both selected executables are inert caller-provided fixture scripts; no SSH,
/// Ansible, candidate or authenticated acquisition is executed by this factory.
pub(in crate::ci) struct PreparationFixture {
    pub root: PathBuf,
    pub caller: PathBuf,
    pub environment: crate::tooling::Environment,
    pub end: Instant,
    pub ready: Vec<u8>,
    pub frame_bytes: u64,
    prepared: Option<super::prepare::PrepareInvocation>,
    frame: Option<super::files::Input>,
    pending: [Option<super::files::Pending>; 2],
    proofs: Option<[Arc<super::tools::Invocation>; 2]>,
    output: Option<Output>,
    tools: Option<super::tools::Tools>,
    checkpoint: Checkpoint,
    budget: Budget,
}
impl PreparationFixture {
    pub async fn new(lease_source: &str, ansible_source: &str, seconds: u64) -> Self {
        use simplestchat_ops::capacity_vps::protocol::{
            Action, Envelope, Header, Members, Mode, Pin, Ready, Response, ResultData, Source,
        };
        let mut fixture = Fixture::new();
        let root = fixture.root.clone();
        let caller = fixture.caller.clone();
        // Transfer cleanup ownership to this complete factory, not Fixture::Drop.
        fixture.root = PathBuf::new();
        drop(fixture);
        let budget = Budget::default();
        let clock: Arc<dyn Clock> = Arc::new(super::plan::NativeClock);
        let start = clock.now();
        let checkpoint = Checkpoint {
            clock: clock.clone(),
            start,
            end: start + Duration::from_secs(300),
            abort: Arc::new(AtomicBool::new(false)),
        };
        let mut output =
            Output::new(&root.join("evidence"), &caller, &budget, &checkpoint).unwrap();
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        for name in super::sources::NAMES {
            let target = caller.join(name);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(repository.join(name), &target).unwrap();
        }
        fs::create_dir_all(caller.join(".git/refs/heads")).unwrap();
        fs::write(caller.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::write(
            caller.join(".git/refs/heads/main"),
            format!("{}\n", "a".repeat(40)),
        )
        .unwrap();
        fs::write(caller.join(".git/config"), b"[core]\n bare=false\n").unwrap();
        let credentials = root.join("credentials");
        fs::create_dir(&credentials).unwrap();
        fs::set_permissions(&credentials, fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["key", "known_hosts"] {
            fs::write(credentials.join(name), b"INERT-DATA-NOT-AUTHORIZED\n").unwrap();
            fs::set_permissions(credentials.join(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
        let inventory_path = credentials.join("inventory.yml");
        fs::write(
            &inventory_path,
            format!(
                concat!(
                    "benchmark_hosts:\n  hosts:\n    target:\n",
                    "      ansible_host: inert.invalid\n      ansible_user: root\n",
                    "      ansible_ssh_private_key_file: {}\n",
                    "      scbench_ssh_known_hosts_file: {}\n",
                ),
                credentials.join("key").display(),
                credentials.join("known_hosts").display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&inventory_path, fs::Permissions::from_mode(0o600)).unwrap();
        let sources = Arc::new(
            super::sources::Sources::open(&caller, &output, &budget, &checkpoint).unwrap(),
        );
        let inventory = Arc::new(
            Inventory::open(
                &inventory_path,
                "target",
                &"a".repeat(40),
                &caller,
                &root,
                &budget,
                &checkpoint,
            )
            .unwrap(),
        );
        let source = Source {
            repository: "swiftraccoon/simplestChat".into(),
            revision: "a".repeat(40),
            package_version: "0.1.0".into(),
            target: "x86_64-unknown-linux-musl".into(),
            protocol_version: 1,
            build_run_id: 123,
            run_attempt: 1,
        };
        let request = options(&[]).unwrap().request(&"1".repeat(32)).unwrap();
        let envelope = Envelope::request(Action::Reserve, &request, None)
            .unwrap()
            .encode()
            .unwrap();
        let pin = Pin {
            sha256: "a".repeat(64),
            bytes: 1,
        };
        let header = Header {
            schema_version: 1,
            scope: "OpsCapacityBootstrap".into(),
            mode: Mode::Invoke,
            run: "1".repeat(32),
            selection: source,
            artifact_id: 456,
            transfer: pin.clone(),
            members: Members {
                manifest: pin.clone(),
                selection: pin.clone(),
                binary: pin,
            },
            request: Pin {
                sha256: super::super::sha(&envelope),
                bytes: envelope.len() as u64,
            },
        };
        let ready = Response::encode(
            &header,
            &ResultData::Reserve(Ready {
                private_host: true,
                ready: true,
            }),
        )
        .unwrap();
        let mut bytes = header.encode().unwrap();
        bytes.push(b'\n');
        let frame = output
            .frame("lease-frame.bin", &bytes, None, &envelope, &checkpoint)
            .unwrap();
        let lease_pending = output.capture(1, super::MAX_STATUS).await.unwrap();
        let ansible_pending = output.capture(2, super::MAX_JSON).await.unwrap();
        let reader = output.ready_reader(&lease_pending).unwrap();
        let ready_hex = ready.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let prefix = format!(
            concat!(
                "#!/usr/bin/python3\nimport os,sys,time,json,signal\n",
                "ROOT={}\nREADY=bytes.fromhex('{}')\n",
            ),
            serde_json::to_string(root.to_str().unwrap()).unwrap(),
            ready_hex
        );
        let paths = [root.join("git"), root.join("ssh"), root.join("ansible")];
        fs::write(&paths[0], b"#!/bin/sh\nexit 99\n").unwrap();
        fs::write(&paths[1], prefix.clone() + lease_source).unwrap();
        fs::write(&paths[2], prefix + ansible_source).unwrap();
        for path in &paths {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let tools = super::tools::Tools::fixture(
            &caller,
            &root,
            sources,
            inventory,
            &budget,
            [&paths[0], &paths[1], &paths[2]],
        )
        .unwrap();
        let options = options(&[]).unwrap();
        let plan = Plan::new(clock, &options, start);
        let end = Instant::now() + Duration::from_secs(seconds);
        let body = Duration::from_secs(1);
        let lease_body = Duration::from_secs(seconds.saturating_sub(30).min(1750));
        let grace = Duration::from_secs(1);
        let mut lease_pending = lease_pending;
        let mut ansible_pending = ansible_pending;
        let make = |program, args, input, maximum, body| {
            tools
                .invocation(
                    super::tools::Spec {
                        program,
                        args,
                        body,
                        grace,
                        maximum,
                        input,
                        end,
                        latest: end
                            .checked_sub(body + grace + Duration::from_secs(20))
                            .unwrap_or(start),
                    },
                    &plan,
                )
                .unwrap()
        };
        use crate::ci::command::{Command, Program};
        let lease_args = vec!["lease-fixture".into()];
        let ansible_args = vec!["ansible-fixture".into()];
        let lease_proof = make(
            Program::OpsVpsSsh,
            lease_args.clone(),
            Some(frame.stamp().clone()),
            super::MAX_STATUS,
            lease_body,
        );
        let ansible_proof = make(
            Program::OpsVpsAnsible,
            ansible_args.clone(),
            None,
            super::MAX_JSON,
            body,
        );
        let mut lease = Command::capture(Program::OpsVpsSsh, &[]);
        lease.capture = None;
        lease.arguments = lease_args;
        lease.timeout = lease_body;
        lease.grace = grace;
        lease.ops_vps = Some(lease_proof.clone());
        lease.separate_logs = lease_pending.logs.take();
        lease.stdin = Some(Arc::new(frame.clone_input(&checkpoint).unwrap()));
        let mut ansible = Command::capture(Program::OpsVpsAnsible, &[]);
        ansible.capture = None;
        ansible.arguments = ansible_args;
        ansible.timeout = body;
        ansible.grace = grace;
        ansible.ops_vps = Some(ansible_proof.clone());
        ansible.separate_logs = ansible_pending.logs.take();
        let prepared = super::prepare::PrepareInvocation::new(
            lease,
            ansible,
            header,
            reader,
            [lease_proof.clone(), ansible_proof.clone()],
        );
        let frame_bytes = frame.bytes();
        Self {
            root,
            caller,
            environment: crate::tooling::Environment::new(),
            end,
            ready,
            frame_bytes,
            prepared: Some(prepared),
            frame: Some(frame),
            pending: [Some(lease_pending), Some(ansible_pending)],
            proofs: Some([lease_proof, ansible_proof]),
            output: Some(output),
            tools: Some(tools),
            checkpoint,
            budget,
        }
    }
    pub fn take(&mut self) -> super::prepare::PrepareInvocation {
        self.prepared.take().unwrap()
    }
    pub fn marker(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
    pub fn captures(&self) -> [Option<super::tools::Captures>; 2] {
        self.proofs
            .as_ref()
            .unwrap()
            .each_ref()
            .map(|proof| proof.captures().ok())
    }
    pub fn exits(&self) -> [Option<super::tools::ExitObservation>; 2] {
        self.proofs
            .as_ref()
            .unwrap()
            .each_ref()
            .map(|proof| proof.exit().ok())
    }
    pub fn finish(mut self) -> crate::tooling::Result<()> {
        let proofs = self.proofs.take().unwrap();
        let mut result = Ok(());
        for (index, proof) in proofs.iter().enumerate() {
            if let Some(pending) = self.pending[index].take() {
                let read = proof.captures().and_then(|seal| {
                    self.output.as_mut().unwrap().capture_read(
                        pending,
                        &seal,
                        &self.checkpoint,
                        true,
                    )
                });
                if let Err(error) = read
                    && result.is_ok()
                {
                    result = Err(error);
                }
            }
        }
        let frame = self.frame.take().unwrap().finish(&self.checkpoint);
        drop(proofs);
        let tools = self.tools.take().unwrap().finish();
        result?;
        frame?;
        tools?;
        if self.budget.count() != 0 {
            return Err(super::failure(1));
        }
        Ok(())
    }
}
impl Drop for PreparationFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
