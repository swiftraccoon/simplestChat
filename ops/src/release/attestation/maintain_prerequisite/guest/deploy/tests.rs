use super::*;

const FIXTURE_CONFIGURATION: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/ansible/group_vars/benchmark_hosts.yml"
));
fn fixture_scalar(key: &str) -> &'static str {
    // These two source-bound plain scalars are data, without a template evaluator.
    let prefix = format!("{key}: ");
    let values = FIXTURE_CONFIGURATION
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 1, "{key}");
    values[0]
}

fn request() -> Value {
    let pins = |names: Vec<String>| {
        names
            .into_iter()
            .map(|name| (name, Value::String("a".repeat(64))))
            .collect::<serde_json::Map<_, _>>()
    };
    let pin = json!({"sha256":"b".repeat(64),"bytes":1});
    let engine = fixture_scalar("scbench_docker_engine_version")
        .split_once(':')
        .unwrap()
        .1
        .split_once('-')
        .unwrap()
        .0;
    json!({"schemaVersion":1,"scope":"NativeHostDeployRequest","runId":"c".repeat(32),
        "revision":"d".repeat(40),"previousRevision":"e".repeat(40),
        "setupSeconds":600,"bodySeconds":4320,"sourcePins":pins(sources::guest_names()),
        "helperPins":pins(sources::helper_names()),"binary":pin,
        "current":{"release.json":pin,"image.tar":pin},
        "previous":{"release.json":pin,"image.tar":pin},"sourceBundle":pin,
        "sourceTree":"f".repeat(40),"engineVersion":engine,
        "postgresImage":fixture_scalar("scpub_postgres_image"),
        "caddyImage":super::super::protocol::CADDY_IMAGE})
}

#[test]
fn deploy_admits_the_actual_frozen_postgres_profile_and_refuses_other_images() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../build/release_container_harness.py"
    ));
    // Read only the frozen two-literal selection, never execute the reference module.
    let block = source
        .split_once("\nPOSTGRES = (\n")
        .unwrap()
        .1
        .split_once("\n)\n")
        .unwrap()
        .0;
    let selected = block
        .lines()
        .map(|line| {
            let literal = line.trim();
            literal
                .strip_prefix("+ ")
                .unwrap_or(literal)
                .strip_prefix('"')
                .unwrap()
                .strip_suffix('"')
                .unwrap()
        })
        .collect::<String>();
    assert_eq!(selected, fixture_scalar("scpub_postgres_image"));
    assert_eq!(selected, protocol::POSTGRES_IMAGE);
    let value = request();
    assert_eq!(value["postgresImage"], selected);
    assert_eq!(value["engineVersion"], "29.8.2");
    assert!(protocol::Request::decode(&serde_json::to_vec(&value).unwrap()).is_ok());
    let (locator, digest) = selected.split_once("@sha256:").unwrap();
    for image in [
        format!("postgres:17-alpine@sha256:{digest}"),
        format!("registry.invalid/library/postgres:18.6-bookworm@sha256:{digest}"),
        format!("{locator}@sha256:{}", "b".repeat(64)),
        format!("{locator}@sha256:{}", digest.to_ascii_uppercase()),
        format!("{locator}@sha256:{}", &digest[..63]),
        locator.to_owned(),
        format!("{locator}@sha256:"),
    ] {
        let mut changed = value.clone();
        changed["postgresImage"] = image.into();
        assert!(protocol::Request::decode(&serde_json::to_vec(&changed).unwrap()).is_err());
    }
    let mut missing = value;
    missing.as_object_mut().unwrap().remove("postgresImage");
    assert!(protocol::Request::decode(&serde_json::to_vec(&missing).unwrap()).is_err());
}
#[test]
fn deploy_request_is_closed_and_distinct_from_maintenance() {
    let value = request();
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() <= 16384);
    let parsed = protocol::Request::decode(&bytes).unwrap();
    assert!(parsed.adapter().is_deploy());
    assert!(!parsed.adapter().is_host());
    for (name, replacement) in [
        ("scope", json!("PreparedHostMaintenanceRequest")),
        ("setupSeconds", json!(601)),
        ("bodySeconds", json!(3719)),
        ("previousRevision", value["revision"].clone()),
        ("engineVersion", json!("unreviewed")),
        ("sourcePins", json!({})),
        ("helperPins", json!({})),
    ] {
        let mut changed = value.clone();
        changed[name] = replacement;
        assert!(
            protocol::Request::decode(&serde_json::to_vec(&changed).unwrap()).is_err(),
            "{name}"
        );
    }
    let mut changed = value;
    changed["candidatePath"] = "/untrusted".into();
    assert!(protocol::Request::decode(&serde_json::to_vec(&changed).unwrap()).is_err());
    let duplicate = bytes
        .into_iter()
        .take_while(|byte| *byte != b'}')
        .collect::<Vec<_>>();
    assert!(protocol::Request::decode(&duplicate).is_err());
}
#[test]
fn deploy_owner_never_borrows_a_maintenance_profile_or_refreshes_time() {
    let start = Instant::now();
    let mut owner = Control::new_deploy(start).unwrap();
    assert_eq!(owner.phase_end(), start + Duration::from_secs(600));
    assert_eq!(owner.body_end(), start + Duration::from_secs(4320));
    assert_eq!(owner.end(), start + Duration::from_secs(4350));
    assert_eq!(owner.monitor_limit(), Duration::from_secs(4500));
    owner.configure_deploy(500, 4000).unwrap();
    let end = owner.end();
    assert!(owner.configure_deploy(600, 4320).is_err());
    assert!(
        owner
            .configure(
                &protocol::Request::decode(&serde_json::to_vec(&request()).unwrap())
                    .unwrap()
                    .adapter()
            )
            .is_err()
    );
    assert!(owner.advance().is_err());
    assert!(owner.advance_host().is_err());
    owner.advance_deploy().unwrap();
    assert!(owner.advance_deploy().is_err());
    assert_eq!(owner.end(), end);
    owner.begin_settling();
    let _ = owner.finish();
}
#[test]
fn expired_deploy_body_still_has_only_its_original_final_thirty_for_known_cleanup() {
    let start = Instant::now() - Duration::from_secs(4330);
    let mut owner = Control::new_deploy(start).unwrap();
    let end = owner.end();
    assert!(owner.check().is_err());
    owner.begin_settling();
    assert_eq!(owner.end(), end);
    assert_eq!(owner.phase_end(), end);
    assert!(owner.check().is_ok());
    assert_eq!(owner.first, Some(Error::Deadline));
    assert!(owner.finish().is_err());
}
#[test]
fn checksum_probe_keeps_the_frozen_ten_thousand_row_bound() {
    let digest = "a".repeat(96);
    let rows = (1..=10000)
        .map(|version| format!("{digest}  /app/migrations/{version}_fixture.sql\n"))
        .collect::<String>();
    assert_eq!(probe::migrations(rows.as_bytes()).unwrap().len(), 10000);
    let too_many = format!("{rows}{digest}  /app/migrations/10001_fixture.sql\n");
    assert!(probe::migrations(too_many.as_bytes()).is_err());
    assert!(
        probe::migrations(
            format!("{digest}  /app/migrations/001_a.sql\n{digest}  /app/migrations/1_b.sql\n")
                .as_bytes()
        )
        .is_err()
    );
    assert!(probe::migrations(b"malformed\n").is_err());
}

#[test]
fn injection_timestamps_keep_shape_only_authority_and_nine_digit_fraction_bound() {
    for text in [
        "0001-01-01T00:00:00Z",
        "2026-10-28T12:34:56.123456789+23:59",
        "2026-99-99T99:99:99-99:99",
        "2026-10-28T12:34:56.1Z",
    ] {
        assert_eq!(injection::finished_at(&json!(text)).unwrap(), text);
    }
    for text in [
        "",
        "garbage",
        "2026-10-28 12:34:56Z",
        "2026-10-28T12:34:56.Z",
        "2026-10-28T12:34:56.1234567890Z",
        "2026-10-28T12:34:56+00:0",
        "2026-10-28T12:34:56Zextra",
        "２０２６-10-28T12:34:56Z",
    ] {
        assert!(injection::finished_at(&json!(text)).is_err(), "{text}");
    }
    assert!(injection::finished_at(&Value::Null).is_err());
}

#[test]
fn empty_observations_do_not_change_nonempty_source_profiles() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "deploy-empty-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let path = directory.join("stream");
    fs::write(&path, []).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut control = Control::new_deploy(Instant::now()).unwrap();
    let mut observed = Original::fixture_observation(&path, 0, &mut control).unwrap();
    assert_eq!(observed.bytes, 0);
    assert_eq!(observed.sha256, hash(b""));
    assert_eq!(observed.data(0, &mut control).unwrap(), b"");
    observed.finish(&mut control).unwrap();
    assert!(observed.closed());
    assert!(
        Original::observe(
            &path,
            super::super::files::ObservedFile::InjectionAlias,
            &mut control
        )
        .is_err()
    );
    assert!(
        Original::observe(
            &path,
            super::super::files::ObservedFile::NativeStream,
            &mut control
        )
        .is_err()
    );
    assert!(control.finish().is_ok());
    let mut control = Control::new_deploy(Instant::now()).unwrap();
    assert!(Original::fixture(&path, 1, 0o600, &mut control).is_err());
    assert_eq!(control.first, Some(Error::Refused));
    let _ = control.finish();
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[test]
fn deploy_boundary_metadata_and_explicit_digest_replay_refuse_distinct_changes() {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "deploy-replay-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let path = directory.join("input");
    fs::write(&path, b"original bytes").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut control = Control::new_deploy(Instant::now()).unwrap();
    let mut original = Original::fixture(&path, 128, 0o600, &mut control).unwrap();
    let admitted_digest = original.sha256.clone();
    original.sha256 = "0".repeat(64);
    // Ordinary metadata binding is separate from the explicit full replay gate.
    original.check_metadata(&mut control).unwrap();
    assert_eq!(original.check(&mut control), Err(Error::Changed));
    original.sha256 = admitted_digest;
    original.check(&mut control).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(original.check_metadata(&mut control), Err(Error::Changed));
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    original.check_metadata(&mut control).unwrap();
    fs::write(&path, b"modified bytes").unwrap();
    assert_eq!(original.check_metadata(&mut control), Err(Error::Changed));
    assert_eq!(original.finish(&mut control), Err(Error::Changed));
    assert!(original.closed());
    assert_eq!(control.first, Some(Error::Changed));
    let _ = control.finish();
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[test]
fn closed_controller_stdin_stays_a_body_failure_but_permits_only_known_final_retirement() {
    use std::{
        fs,
        os::{
            fd::OwnedFd,
            unix::{fs::PermissionsExt, net::UnixStream},
        },
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "deploy-eof-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let path = directory.join("known");
    fs::write(&path, b"owned cleanup input").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut control = Control::new_deploy(Instant::now()).unwrap();
    let original = Original::fixture(&path, 128, 0o600, &mut control).unwrap();
    let (input, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let output = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .unwrap();
    let request = protocol::Request::decode(&serde_json::to_vec(&request()).unwrap()).unwrap();
    let mut ctx = Context {
        source_root: directory.clone(),
        request: request.adapter(),
        control,
        commands: Commands::new_deploy(),
        sources: BTreeMap::new(),
        originals: vec![original],
        observed: Observations::new(),
        wire: super::super::protocol::Wire::fixture(OwnedFd::from(input).into(), output),
        socket: None,
        host_retired: BTreeSet::new(),
        host_diagnostic: None,
    };
    assert_eq!(ctx.check(), Err(Error::Refused));
    ctx.control.record(Error::Refused);
    let end = ctx.control.end();
    ctx.control.begin_settling();
    assert_eq!(ctx.control.end(), end);
    assert_eq!(ctx.control.phase_end(), end);
    ctx.check().unwrap();
    // The old maintenance profile keeps its existing closed-input refusal.
    ctx.request.scope = "MaintainLauncherPrerequisiteRequest".into();
    assert_eq!(ctx.check(), Err(Error::Refused));
    ctx.request.scope = "NativeHostDeployRequest".into();
    ctx.originals[0]
        .retire_owned(b"owned cleanup input", &mut ctx.control)
        .unwrap();
    assert!(ctx.originals[0].closed() && !path.exists());
    ctx.host_retired.insert(0);
    ctx.check().unwrap();
    ctx.wire.close(&mut ctx.control).unwrap();
    assert_eq!(ctx.control.finish(), Err(Error::Refused));
    fs::remove_dir(directory).unwrap();
}
