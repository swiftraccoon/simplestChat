//! Original owned inert tools/files and explicit negative edges, no network/GH.
use super::super::verification_fixture as cached;
use super::*;
#[path = "fixture.rs"]
pub(in crate::release::attestation) mod fixture;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::symlink};
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
fn owner() -> Owner {
    Owner::new(
        Instant::now(),
        Duration::from_secs(60),
        Duration::from_secs(10),
    )
    .unwrap()
}
fn options(fixture: &fixture::Fixture, cached: bool) -> Options {
    let mut options = Options::parse(&fixture.arguments(cached)).unwrap().unwrap();
    options.environment = Some(fixture.environment());
    options
}
#[test]
fn whole_fresh_and_cached_paths_keep_secrets_and_curl_in_their_original_roles() {
    for cached in [false, true] {
        let fixture = fixture::Fixture::new();
        if cached {
            fs::remove_file(fixture.bin.join("curl")).unwrap();
        }
        let before = fixture.snapshot();
        let options = options(&fixture, cached);
        let runtime = runtime();
        let (result, cause, closes) = runtime.block_on(async {
            let mut owner = owner();
            files::start_probe(None);
            let result = execute(&options, &mut owner).await;
            (result, owner.error(), files::take_probe())
        });
        drop(runtime);
        let after = fixture.snapshot();
        let argv = fixture.read("argv");
        let attempts = fixture.attempts();
        let empty_cwd = fs::read_dir(&fixture.cwd).map(|entries| entries.count());
        let expected = fixture.selection["revision"].clone();
        drop(fixture);
        assert_eq!(empty_cwd.unwrap(), 0);
        assert_eq!(cause, None, "fixed operation phase {}", LAST_PHASE.get());
        assert!(closes.0 > 0 && closes.1 == 0);
        let result: Value = serde_json::from_slice(&result.unwrap()).unwrap();
        assert_eq!(result.as_object().unwrap().len(), 4);
        assert_eq!(result["revision"], expected);
        assert_eq!(before.unwrap(), after.unwrap());
        let argv = String::from_utf8(argv.unwrap()).unwrap();
        assert_eq!(argv.matches("--call--").count(), if cached { 6 } else { 7 });
        assert_eq!(argv.contains("auth\ntoken"), !cached);
        assert_eq!(attempts.unwrap().len(), usize::from(!cached));
    }
}

#[test]
fn custom_gh_config_reaches_each_gh_role_without_curl_or_output_leaks() {
    for cached in [false, true] {
        let fixture = fixture::Fixture::new();
        let config = fixture.home.join("PRIVATE-GH-CONFIG-SENTINEL");
        fs::create_dir(&config).unwrap();
        let gh = String::from_utf8(fs::read(fixture.bin.join("gh")).unwrap()).unwrap();
        fixture::write(
            &fixture.bin.join("gh"),
            gh.replacen(
                "#!/bin/sh\n",
                concat!(
                    "#!/bin/sh\n",
                    "[ \"${GH_CONFIG_DIR-}\" = \"$HOME/PRIVATE-GH-CONFIG-SENTINEL\" ] || exit 96\n",
                    "[ \"${GH_TOKEN+x}${GITHUB_TOKEN+x}\" = '' ] || exit 97\n"
                ),
                1,
            )
            .as_bytes(),
            0o700,
        )
        .unwrap();
        let curl = String::from_utf8(fs::read(fixture.bin.join("curl")).unwrap()).unwrap();
        fixture::write(
            &fixture.bin.join("curl"),
            curl.replacen(
                "#!/bin/sh\n",
                "#!/bin/sh\n[ \"${GH_CONFIG_DIR+x}\" = '' ] || exit 98\n",
                1,
            )
            .as_bytes(),
            0o700,
        )
        .unwrap();
        if cached {
            fs::remove_file(fixture.bin.join("curl")).unwrap();
        }
        let mut environment = fixture.environment();
        environment.remove(OsStr::new("GH_TOKEN"));
        environment.remove(OsStr::new("GITHUB_TOKEN"));
        environment.insert("GH_CONFIG_DIR".into(), config.as_os_str().to_owned());
        let mut options = options(&fixture, cached);
        options.environment = Some(environment);
        let before = fixture.snapshot().unwrap();
        let runtime = runtime();
        let (result, cause, closes) = runtime.block_on(async {
            let mut owner = owner();
            files::start_probe(None);
            let result = execute(&options, &mut owner).await;
            (result, owner.error(), files::take_probe())
        });
        drop(runtime);
        let after = fixture.snapshot().unwrap();
        let argv = String::from_utf8(fixture.read("argv").unwrap()).unwrap();
        let attempts = fixture.attempts().unwrap();
        let config_text = config.to_string_lossy().into_owned();
        drop(fixture);
        assert_eq!(cause, None, "fixed operation phase {}", LAST_PHASE.get());
        assert!(closes.0 > 0 && closes.1 == 0);
        assert_eq!(before, after);
        assert_eq!(argv.matches("--call--").count(), if cached { 6 } else { 7 });
        assert_eq!(argv.matches("\napi\n").count(), 2);
        assert_eq!(argv.matches("\nattestation\nverify\n").count(), 4);
        assert_eq!(argv.contains("auth\ntoken"), !cached);
        assert_eq!(attempts.len(), usize::from(!cached));
        let bytes = result.unwrap();
        let result: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result.as_object().unwrap().len(), 4);
        let output = String::from_utf8(bytes).unwrap();
        for private in [config_text.as_str(), "INERT_TOKEN"] {
            assert!(!output.contains(private));
            assert!(!argv.contains(private));
        }
    }
}

#[test]
fn grant_and_raw_headers_preserve_reference_values_without_httparse_trimming() {
    let runtime = runtime();
    let results = runtime.block_on(async {
        let mut results = Vec::new();
        for (url, valid, target) in [
            ("https://a.blob.core.windows.net/p?q=v#", true, "/p?q=v"),
            (
                "HTTPS://a.blob.core.windows.net/a/../b?sig=a%2Fb+q",
                true,
                "/a/../b?sig=a%2Fb+q",
            ),
            ("https://a.blob.core.windows.net/p??q=v#", true, "/p??q=v"),
            ("https://a.blob.core.windows.net/p?q=v##", false, ""),
            ("https://A.blob.core.windows.net/p?q=v", false, ""),
        ] {
            let mut owner = owner();
            let got = grant::Grant::decode(url, &mut owner).map(|grant| grant.target().to_owned());
            results.push((got, valid, target));
        }
        results
    });
    let headers = runtime.block_on(async {
        let mut results = Vec::new();
        for (suffix, valid) in [("", true), (" ", false), ("\t", false), ("\r\n x", false)] {
            let wire = format!(
                concat!(
                    "HTTP/1.1 302 Found\r\n",
                    "Location: https://a.blob.core.windows.net/p?q=v{}\r\n\r\n"
                ),
                suffix
            );
            let mut owner = owner();
            let mut decoder = headers::Decoder::default();
            let result = decoder
                .push(wire.as_bytes(), &mut owner)
                .and_then(|result| {
                    let (header, _) = result.ok_or(Cause::Policy)?;
                    grant::Grant::decode(&header.location(&mut owner)?, &mut owner).map(|_| ())
                });
            results.push((result, valid));
        }
        results
    });
    drop(runtime);
    for (result, valid, target) in results {
        assert_eq!(result.is_ok(), valid);
        if valid {
            assert_eq!(result.unwrap(), target);
        }
    }
    for (result, valid) in headers {
        assert_eq!(result.is_ok(), valid);
    }
}
#[test]
fn metadata_bool_and_nonfinite_fields_never_acquire_integer_identity() {
    let fixture = fixture::Fixture::new();
    let bytes = fixture.read("artifact.json").unwrap();
    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
    value["id"] = true.into();
    let bytes = serde_json::to_vec(&value).unwrap();
    let runtime = runtime();
    let refused = runtime.block_on(async {
        let mut owner = owner();
        let options = options(&fixture, true);
        let value = crate::json::decode_limited(&bytes, GH_LIMIT).unwrap();
        policy::artifact(&value, &options.selected, &mut owner).is_err()
    });
    drop(runtime);
    drop(fixture);
    assert!(refused);
}
#[test]
fn denied_parent_and_copy_prefix_are_retained_and_all_original_handles_close() {
    for symlinked in [false, true] {
        let fixture = fixture::Fixture::new();
        let options = options(&fixture, false);
        if symlinked {
            symlink(&fixture.home, fixture.root.join("output")).unwrap();
        } else {
            fixture::write(
                &fixture.home.join("blob.http"),
                b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\n\r\n",
                0o600,
            )
            .unwrap();
        }
        let runtime = runtime();
        let (result, closes) = runtime.block_on(async {
            files::start_probe(None);
            let mut owner = owner();
            let result = execute(&options, &mut owner).await;
            (result, files::take_probe())
        });
        drop(runtime);
        let targets = fixture.attempts();
        let zip_exists = targets
            .as_ref()
            .ok()
            .and_then(|paths| paths.first())
            .is_some_and(|path| path.join("artifact/artifact.zip").exists());
        drop(fixture);
        assert!(result.is_err());
        assert!(closes.0 > 0 && closes.1 == 0);
        assert!(!zip_exists);
    }
}
#[test]
fn explicit_reserve_disk_and_close_failures_do_not_reset_the_original_owner() {
    let fixture = fixture::Fixture::new();
    let options = options(&fixture, false);
    let runtime = runtime();
    let (result, cause, closes, artifact_exists) = runtime.block_on(async {
        files::start_probe(None);
        let mut owner = owner();
        let mut state = State::new(&options);
        state.free_bytes = Some(0);
        let result = state.perform(&options, &mut owner).await;
        if let Err(cause) = result.as_ref() {
            owner.record(*cause);
        }
        let artifact_exists = state.artifact.is_some();
        state.finish(&mut owner);
        (result, owner.error(), files::take_probe(), artifact_exists)
    });
    drop(runtime);
    drop(fixture);
    assert!(
        artifact_exists,
        "fixed operation phase {} / cause {cause:?}",
        LAST_PHASE.get()
    );
    assert!(result.is_err());
    assert_eq!(cause, Some(Cause::Policy));
    assert!(closes.0 > 0 && closes.1 == 0);
}
#[test]
fn notices_keep_the_authenticated_license_bytes_and_kernel_write_refusals() {
    assert_eq!(licenses::RANDOM_LICENSE.len(), 1130);
    assert_eq!(
        files::hex(&Sha256::digest(licenses::RANDOM_LICENSE.as_bytes())),
        "523a42c25d245dde9c015f882cec7f4555aad883382a6cf19b4b7d9b2cd5419b"
    );
    assert_eq!(licenses::LICENSE.len(), 1063);
    assert_eq!(
        files::hex(&Sha256::digest(licenses::LICENSE.as_bytes())),
        "391a5396cec6230bfabd4ef4eb2350eb895bc5efce377a2218f5702ed020d3e3"
    );
    let input = fs::File::open("/dev/null").unwrap();
    use std::os::fd::AsFd;
    let result = licenses::write(licenses::Descriptor(input.as_fd()));
    let closed = nix::unistd::close(input);
    assert!(result.is_err());
    closed.unwrap();
}
fn lifecycle_worker() {
    FRAME_CLOSED.set(false);
    COMPLETION_SAW_CLOSED.set(false);
    COMPLETION_OFFSET.set(Some(Duration::ZERO));
    let original = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o027));
    let fixture = fixture::Fixture::new();
    fs::remove_file(fixture.bin.join("curl")).unwrap();
    let options = options(&fixture, true);
    let mut emitted = None;
    let started = Instant::now();
    let code = lifecycle(started, Duration::from_secs(60), || {
        let mut owner =
            Owner::new(started, Duration::from_secs(60), Duration::from_secs(10)).unwrap();
        let runtime = runtime();
        let result = runtime.block_on(async {
            let result = execute(&options, &mut owner).await;
            let passed = result.is_ok();
            emitted = result.ok();
            passed && owner.check(Window::Final).is_ok()
        });
        drop(runtime);
        FRAME_CLOSED.set(true);
        (result, Some(owner))
    });
    drop(fixture);
    let restored = nix::sys::stat::umask(original);
    let after = COMPLETION_SAW_CLOSED.get();
    COMPLETION_OFFSET.set(None);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(after);
    assert_eq!(
        serde_json::from_slice::<Value>(&emitted.unwrap())
            .unwrap()
            .as_object()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(restored, nix::sys::stat::Mode::from_bits_truncate(0o027));
}

#[test]
fn existing_root_output_parent_is_admitted_without_creating_any_child() {
    let fixture = fixture::Fixture::new();
    let options = options(&fixture, false);
    let runtime = runtime();
    let (result, closes) = runtime.block_on(async {
        files::start_probe(None);
        let mut owner = owner();
        let mut state = State::new(&options);
        let result = state.parent(Path::new("/"), &mut owner);
        state.finish(&mut owner);
        (result, files::take_probe())
    });
    drop(runtime);
    drop(fixture);
    assert!(result.is_ok());
    assert!(closes.0 > 0 && closes.1 == 0);
}

#[test]
fn final_positive_fence_runs_after_the_actual_runtime_frame_and_umask_restore() {
    const ROLE: &str = "SC_FRESH_LIFECYCLE_WORKER";
    if std::env::var_os(ROLE).is_some() {
        return lifecycle_worker();
    }
    let program = std::env::current_exe().unwrap();
    let test = format!(
        "{}::final_positive_fence_runs_after_the_actual_runtime_frame_and_umask_restore",
        module_path!().split_once("::").unwrap().1
    );
    let runtime = runtime();
    let completion = runtime.block_on(async {
        let mut owner = owner();
        let body_end = owner.now() + Duration::from_secs(20);
        verify::process::run(
            &mut owner,
            verify::process::Invocation {
                program,
                arguments: vec![
                    "--exact".into(),
                    test.into(),
                    "--nocapture".into(),
                    "--test-threads=1".into(),
                ],
                environment: std::collections::BTreeMap::from([
                    ("PATH".into(), "/usr/bin:/bin".into()),
                    (ROLE.into(), "owned".into()),
                ]),
                body_end,
                settlement_end: body_end + Duration::from_secs(10),
            },
        )
        .await
    });
    drop(runtime);
    assert!(completion.observation.group_retired && completion.observation.reaped);
    assert!(
        completion.observation.stdin_closed
            && completion.observation.stdout_closed
            && completion.observation.stderr_closed
    );
    assert_eq!(completion.observation.exit_status, Some(0));
    assert!(completion.result.is_ok());
}

#[test]
fn fresh_bad_crc_retains_only_the_original_one_mebibyte_extraction_prefix() {
    use std::io::Write;
    let fixture = fixture::Fixture::new();
    let mut archive = rawzip::ZipArchiveWriter::new(Vec::new());
    for name in verify::zip::SELECTED {
        let payload = if name == "image.tar" {
            vec![b'x'; 1024 * 1024 + 73]
        } else {
            b"x".to_vec()
        };
        let (mut entry, config) = archive.new_file(name).start().unwrap();
        let mut writer = config.wrap(&mut entry);
        writer.write_all(&payload).unwrap();
        let (_, descriptor) = writer.finish().unwrap();
        entry.finish(descriptor).unwrap();
    }
    let mut bytes = archive.finish().unwrap();
    let first = bytes
        .windows(4)
        .position(|part| part == b"PK\x01\x02")
        .unwrap();
    bytes[first + 16] ^= 1; // Central-authoritative CRC, not payload or local-header changes.
    fixture::write(&fixture.home.join("input.zip"), &bytes, 0o600).unwrap();
    let mut selected = fixture.selection.clone();
    selected["artifactZipBytes"] = (bytes.len() as u64).into();
    selected["zipSha256"] = files::hex(&Sha256::digest(&bytes)).into();
    let metadata =
        verify::options::Metadata::decode(&serde_json::to_vec(&selected).unwrap()).unwrap();
    let runtime = runtime();
    let (result, cause, closes, length) = runtime.block_on(async {
        let mut owner = owner();
        files::start_probe(None);
        let mut input = None;
        let mut output = None;
        let mut zip = None;
        let mut members = std::array::from_fn(|_| None);
        let result = (|| {
            input = Some(Dir::open(&fixture.home, &mut owner)?);
            output = Some(
                input
                    .as_ref()
                    .unwrap()
                    .create_child(OsStr::new("extracted"), &mut owner)?,
            );
            zip = Some(Retained::open(
                input.as_ref().unwrap().clone(),
                "input.zip",
                &mut owner,
            )?);
            verify::zip::extract(
                zip.as_ref().unwrap(),
                output.as_ref().unwrap(),
                &mut members,
                &metadata,
                &mut owner,
            )
        })();
        if let Err(cause) = &result {
            owner.record(*cause);
        }
        for file in members.into_iter().flatten() {
            file.finish(&mut owner);
        }
        if let Some(zip) = zip {
            zip.finish(&mut owner);
        }
        if let Some(output) = output {
            output.finish(&mut owner);
        }
        if let Some(input) = input {
            input.finish(&mut owner);
        }
        let length = fs::metadata(fixture.home.join("extracted/image.tar")).map(|file| file.len());
        (result, owner.error(), files::take_probe(), length)
    });
    drop(runtime);
    drop(fixture);
    assert!(result.is_err());
    assert_eq!(cause, Some(Cause::Zip));
    assert!(closes.0 > 0 && closes.1 == 0);
    assert_eq!(length.unwrap(), 1024 * 1024);
}

#[test]
fn non_utf8_active_directory_returns_an_error_after_all_peers_close_without_panicking() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = fixture::Fixture::new();
    fs::remove_file(fixture.bin.join("curl")).unwrap();
    let before = fixture.snapshot();
    let invalid = fixture
        .root
        .join(OsString::from_vec(b"candidate-\xff".to_vec()));
    let raw_filesystem = match fs::rename(&fixture.candidate, &invalid) {
        Ok(()) => true,
        Err(error)
            if cfg!(target_os = "macos")
                && error.raw_os_error() == Some(rustix::io::Errno::ILSEQ.raw_os_error()) =>
        {
            false
        }
        Err(error) => panic!("owned byte-path fixture setup failed: {error}"),
    };
    let mut args = fixture.arguments(true);
    if raw_filesystem {
        *args.last_mut().unwrap() = invalid.as_os_str().to_owned();
    }
    let mut options = Options::parse(&args).unwrap().unwrap();
    options.environment = Some(fixture.environment());
    if !raw_filesystem {
        options.output_spelling = Some(invalid);
    }
    let runtime = runtime();
    files::start_probe(None);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runtime.block_on(async {
            let mut owner = owner();
            let result = execute(&options, &mut owner).await;
            (result, owner.error())
        })
    }));
    drop(runtime);
    let closes = files::take_probe();
    let restored = if raw_filesystem {
        fs::rename(PathBuf::from(args.last().unwrap()), &fixture.candidate)
    } else {
        Ok(())
    };
    let after = fixture.snapshot();
    let calls = fixture.read("argv");
    drop(fixture);
    restored.unwrap();
    assert!(result.is_ok());
    let (result, cause) = result.unwrap();
    assert!(matches!(result, Err(Cause::Input)));
    assert_eq!(cause, Some(Cause::Input));
    assert!(closes.0 > 0 && closes.1 == 0);
    assert_eq!(before.unwrap(), after.unwrap());
    assert_eq!(
        calls
            .unwrap()
            .windows(b"--call--".len())
            .filter(|bytes| *bytes == b"--call--")
            .count(),
        6
    );
}

#[test]
fn late_real_signals_after_runtime_restore_refuse_fresh_success() {
    use super::super::control::signal_tests::{self, Entry};
    let Some(expected) = signal_tests::worker(Entry::Fresh) else {
        return signal_tests::supervise(Entry::Fresh);
    };
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o027));
    let fixture = fixture::Fixture::new();
    fs::remove_file(fixture.bin.join("curl")).unwrap();
    let options = options(&fixture, true);
    let started = Instant::now();
    let mut facts = None;
    let code = lifecycle(started, options.whole, || {
        let mut owner = Owner::new(started, options.whole, options.reserve).unwrap();
        let runtime = runtime();
        let result = runtime.block_on(async {
            files::start_probe(None);
            let result = execute(&options, &mut owner).await;
            let passed = result.is_ok();
            let bytes = result.unwrap_or_default();
            let emitted = verify::stdout(&mut owner, &bytes);
            if let Err(cause) = emitted {
                owner.record(cause);
            }
            (
                passed && owner.check(Window::Final).is_ok(),
                bytes,
                files::take_probe(),
            )
        });
        drop(runtime);
        let passed = result.0;
        facts = Some(result);
        (passed, Some(owner))
    });
    drop(fixture);
    let restored = nix::sys::stat::umask(previous);
    signal_tests::assert_finished(
        code,
        expected,
        nix::sys::stat::Mode::from_bits_truncate(0o027),
    );
    let (passed, emitted, probes) = facts.unwrap();
    assert!(passed);
    let result: Value = serde_json::from_slice(&emitted).unwrap();
    assert_eq!(result.as_object().unwrap().len(), 4);
    assert!(probes.0 > 0 && probes.1 == 0);
    assert_eq!(restored, nix::sys::stat::Mode::from_bits_truncate(0o027));
}
