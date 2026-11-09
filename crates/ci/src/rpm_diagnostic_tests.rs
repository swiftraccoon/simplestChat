//! Inert custody/CLI-contract tests; the fake does not claim cryptographic proof.

use super::*;
use std::{
    os::unix::fs::{DirBuilderExt, symlink},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "sc-rpm-diagnostic-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

const PEM: &[u8] = b"-----BEGIN CERTIFICATE-----\nFIXTURE\n-----END CERTIFICATE-----\n";
fn environment(root: &Path) -> Environment {
    [
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_REPOSITORY", "swiftraccoon/simplestChat"),
        ("GITHUB_EVENT_NAME", "push"),
        ("GITHUB_REF", "refs/heads/main"),
        ("RUNNER_ENVIRONMENT", "github-hosted"),
        ("RUNNER_OS", "Linux"),
        ("RUNNER_ARCH", "X64"),
        ("GITHUB_RUN_ID", "123"),
        ("GITHUB_RUN_ATTEMPT", "1"),
        ("GITHUB_SHA", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .chain([
        ("RUNNER_TEMP".into(), root.as_os_str().to_os_string()),
        (
            "GITHUB_OUTPUT".into(),
            root.join("github-output").into_os_string(),
        ),
        (
            "RPM_DIAGNOSTIC_RECIPIENT".into(),
            OsString::from(std::str::from_utf8(PEM).unwrap()),
        ),
    ])
    .collect()
}
fn context(root: &Path) -> Context {
    Context::new(
        &[
            "--root".into(),
            root.join("production-security").display().to_string(),
            "--output".into(),
            root.join("encrypted-rpm-diagnostic").display().to_string(),
        ],
        &environment(root),
        "Linux",
    )
    .unwrap()
}
fn input(context: &Context) -> PathBuf {
    let input = context.root.join(INPUT);
    fs::create_dir_all(input.parent().unwrap()).unwrap();
    fs::write(&input, b"PRIVATE INERT DATABASE BYTES").unwrap();
    input
}
#[test]
fn selectors_are_exact_hosted_opt_in_and_never_accept_untrusted_contexts() {
    let fixture = Fixture::new();
    let root = fs::canonicalize(&fixture.0).unwrap();
    let context = context(&root);
    let args = [
        "--root".into(),
        context.root.display().to_string(),
        "--output".into(),
        context.output.display().to_string(),
    ];
    for (key, value) in [
        ("GITHUB_ACTIONS", "false"),
        ("GITHUB_REPOSITORY", "other/repository"),
        ("GITHUB_EVENT_NAME", "pull_request"),
        ("GITHUB_REF", "refs/heads/other"),
        ("RUNNER_ENVIRONMENT", "self-hosted"),
        ("RUNNER_OS", "macOS"),
        ("RUNNER_ARCH", "ARM64"),
        ("ACT", "true"),
        ("GITHUB_RUN_ID", "01"),
        ("GITHUB_RUN_ATTEMPT", "0"),
        ("GITHUB_SHA", "unsafe\nvalue"),
        ("RPM_DIAGNOSTIC_RECIPIENT", ""),
    ] {
        let mut env = environment(&root);
        env.insert(key.into(), value.into());
        assert!(Context::new(&args, &env, "Linux").is_err(), "{key}");
    }
    assert!(Context::new(&args, &environment(&root), "Darwin").is_err());
    let mut env = environment(&root);
    env.insert("RPM_DIAGNOSTIC_RECIPIENT".into(), "x".repeat(16385).into());
    assert!(Context::new(&args, &env, "Linux").is_err());
    let mut wrong = args.clone();
    wrong[1] = root.display().to_string();
    assert!(Context::new(&wrong, &environment(&root), "Linux").is_err());
}
#[derive(Default)]
struct Fake {
    calls: usize,
    fail: bool,
    noncanonical: bool,
    mutate: Option<PathBuf>,
}
impl Crypto for Fake {
    async fn run(
        &mut self,
        args: &[OsString],
        destination: &File,
        cap: Duration,
        control: &Control<'_>,
    ) -> Result<()> {
        control.check()?;
        self.calls += 1;
        if args[0] == "x509" {
            assert_eq!(cap, Duration::from_secs(5));
            assert_eq!(
                args[3..6],
                [OsString::from("-outform"), "PEM".into(), "-out".into()]
            );
            let bytes = if self.noncanonical {
                b"DIFFERENT CERTIFICATE".as_slice()
            } else {
                PEM
            };
            io(destination.write_at(bytes, 0))?;
        } else {
            assert_eq!(cap, Duration::from_secs(30));
            assert_eq!(
                &args[..7],
                [
                    "cms",
                    "-encrypt",
                    "-binary",
                    "-aes-256-gcm",
                    "-outform",
                    "DER",
                    "-in"
                ]
                .map(OsString::from)
            );
            assert_eq!(
                &args[12..],
                [
                    "-keyopt",
                    "rsa_padding_mode:oaep",
                    "-keyopt",
                    "rsa_oaep_md:sha256"
                ]
                .map(OsString::from)
            );
            io(destination.write_at(b"INERT CIPHERTEXT", 0))?;
            if let Some(path) = &self.mutate {
                io(fs::write(path, b"CHANGED"))?;
            }
            if self.fail {
                return Err(failure());
            }
        }
        Ok(())
    }
}
fn exercise(context: &Context, fake: &mut Fake) -> Result<bool> {
    let mut signals = CompletionSignals::new().unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let control = Control {
        signals: &signals,
        body: end - Duration::from_secs(5),
        end,
    };
    let result = runtime.block_on(operation(context, fake, &control));
    drop(runtime);
    assert!(signals.finish().unwrap().is_none());
    result
}
#[test]
fn same_operation_retains_only_ciphertext_and_safe_manifest_after_all_checks() {
    let fixture = Fixture::new();
    let context = context(&fs::canonicalize(&fixture.0).unwrap());
    input(&context);
    let mut fake = Fake::default();
    assert!(exercise(&context, &mut fake).unwrap());
    assert_eq!(fake.calls, 2);
    let mut names: Vec<_> = fs::read_dir(&context.output)
        .unwrap()
        .map(|row| row.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["manifest.json", "rpmdb.cms.der"]);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(context.output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.as_object().unwrap().len(), 9);
    assert_eq!(manifest["revision"], context.revision);
    assert_eq!(manifest["runId"], 123);
    assert_eq!(manifest["runAttempt"], 1);
    assert_eq!(
        manifest["inputBytes"],
        b"PRIVATE INERT DATABASE BYTES".len()
    );
    assert!(
        !fs::read_to_string(context.output.join("manifest.json"))
            .unwrap()
            .contains("PRIVATE INERT")
    );
    assert_eq!(fs::metadata(&context.output).unwrap().mode() & 0o777, 0o700);
    for name in names {
        assert_eq!(
            fs::metadata(context.output.join(name)).unwrap().mode() & 0o777,
            0o600
        );
    }
}
#[test]
fn missing_input_is_empty_and_refusals_never_publish_partial_cipher_or_manifest() {
    for mode in [
        "missing",
        "symlink",
        "large",
        "canonical",
        "bundle",
        "child",
        "mutated",
    ] {
        let fixture = Fixture::new();
        let mut context = context(&fs::canonicalize(&fixture.0).unwrap());
        let mut fake = Fake::default();
        if mode != "missing" {
            let path = input(&context);
            match mode {
                "symlink" => {
                    fs::remove_file(&path).unwrap();
                    symlink("/dev/null", path).unwrap();
                }
                "large" => {
                    File::options()
                        .write(true)
                        .open(path)
                        .unwrap()
                        .set_len(MAXIMUM + 1)
                        .unwrap();
                }
                "canonical" => fake.noncanonical = true,
                "bundle" => context.certificate.extend_from_slice(
                    b"-----BEGIN PRIVATE KEY-----\nFIXTURE\n-----END PRIVATE KEY-----\n",
                ),
                "child" => fake.fail = true,
                "mutated" => fake.mutate = Some(path),
                _ => unreachable!(),
            }
        }
        let result = exercise(&context, &mut fake);
        if mode == "missing" {
            assert!(!result.unwrap());
        } else {
            assert!(result.is_err());
        }
        assert!(!context.output.join("rpmdb.cms.der").exists());
        assert!(!context.output.join("manifest.json").exists());
        assert!(!context.github_output.exists());
    }
}
#[test]
fn held_original_refuses_replaced_leaf_and_parent_without_closing_foreign_entries() {
    for parent in [false, true] {
        let fixture = Fixture::new();
        let context = context(&fs::canonicalize(&fixture.0).unwrap());
        let path = input(&context);
        let mut source = Source::open(&path).unwrap().unwrap();
        if parent {
            let directory = path.parent().unwrap();
            fs::rename(directory, directory.with_extension("held")).unwrap();
            fs::create_dir(directory).unwrap();
        } else {
            fs::rename(&path, path.with_extension("held")).unwrap();
        }
        fs::write(&path, b"FOREIGN").unwrap();
        assert!(source.check().is_err());
        assert!(source.finish().is_err());
        assert!(source.file.is_none() && source.parent.is_none());
        assert_eq!(fs::read(path).unwrap(), b"FOREIGN");
    }
}

#[test]
fn completed_output_stamp_cannot_change_between_hash_admission_and_final_close() {
    let fixture = Fixture::new();
    let path = fs::canonicalize(&fixture.0)
        .unwrap()
        .join("encrypted-rpm-diagnostic");
    let mut output = Artifacts::create(&path).unwrap();
    output.add("rpmdb.cms.der", b"SEALED INERT BYTES").unwrap();
    fs::write(path.join("rpmdb.cms.der"), b"CHANGED").unwrap();
    assert!(output.finish(true).is_err());
    assert!(output.directory.is_none() && output.files.is_empty());
}

#[test]
fn terminal_observation_cannot_borrow_the_later_whole_body_deadline() {
    let mut signals = CompletionSignals::new().unwrap();
    let now = Instant::now();
    let control = Control {
        signals: &signals,
        body: now + Duration::from_secs(20),
        end: now + Duration::from_secs(30),
    };
    assert!(control.check().is_ok());
    assert!(terminal(now, &control).is_err());
    assert!(terminal(now + Duration::from_secs(5), &control).is_ok());
    assert!(signals.finish().unwrap().is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn actual_native_cms_uses_held_fds_and_refuses_bundles_nonzero_and_tampered_tag() {
    fn bytes(file: &File, maximum: u64) -> Vec<u8> {
        let length = file.metadata().unwrap().len();
        assert!(length > 0 && length <= maximum);
        let mut bytes = vec![0; usize::try_from(length).unwrap()];
        file.read_exact_at(&mut bytes, 0).unwrap();
        bytes
    }
    struct Counted {
        native: Native,
        calls: usize,
    }
    impl Crypto for Counted {
        async fn run(
            &mut self,
            args: &[OsString],
            output: &File,
            cap: Duration,
            control: &Control<'_>,
        ) -> Result<()> {
            self.calls += 1;
            self.native.run(args, output, cap, control).await
        }
    }
    let fixture = Fixture::new();
    let root = fs::canonicalize(&fixture.0).unwrap();
    let mut context = context(&root);
    let source = input(&context);
    let mut materials = Artifacts::create(&root.join("crypto-materials")).unwrap();
    let key = materials.add("private-key.pem", &[]).unwrap();
    let certificate = materials.add("certificate.pem", &[]).unwrap();
    let plaintext = materials.add("plaintext", &[]).unwrap();
    let corrupt = materials.add("corrupt.der", &[]).unwrap();
    let rejected = materials.add("untrusted-plaintext", &[]).unwrap();
    let mut signals = CompletionSignals::new().unwrap();
    let end = Instant::now() + Duration::from_secs(50);
    let control = Control {
        signals: &signals,
        body: end - Duration::from_secs(10),
        end,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut native = Native {
            program: "/usr/bin/openssl".into(),
        };
        native
            .run(
                &[
                    "req".into(),
                    "-x509".into(),
                    "-newkey".into(),
                    "rsa:2048".into(),
                    "-noenc".into(),
                    "-days".into(),
                    "1".into(),
                    "-subj".into(),
                    "/CN=Owned inert RPM diagnostic test".into(),
                    "-keyout".into(),
                    descriptor(materials.file(key)),
                    "-out".into(),
                    descriptor(materials.file(certificate)),
                ],
                materials.file(certificate),
                Duration::from_secs(5),
                &control,
            )
            .await
            .unwrap();
        context.certificate = bytes(materials.file(certificate), CERTIFICATE);
        assert!(operation(&context, &mut native, &control).await.unwrap());
        let encrypted = context.output.join("rpmdb.cms.der");
        let decrypt = |input: OsString, output: OsString| {
            vec![
                "cms".into(),
                "-decrypt".into(),
                "-binary".into(),
                "-inform".into(),
                "DER".into(),
                "-in".into(),
                input,
                "-recip".into(),
                descriptor(materials.file(certificate)),
                "-inkey".into(),
                descriptor(materials.file(key)),
                "-out".into(),
                output,
            ]
        };
        native
            .run(
                &decrypt(
                    encrypted.as_os_str().to_owned(),
                    descriptor(materials.file(plaintext)),
                ),
                materials.file(plaintext),
                Duration::from_secs(5),
                &control,
            )
            .await
            .unwrap();
        assert_eq!(
            bytes(materials.file(plaintext), MAXIMUM),
            fs::read(&source).unwrap()
        );
        let mut damaged = fs::read(&encrypted).unwrap();
        *damaged.last_mut().unwrap() ^= 1;
        materials.file(corrupt).write_all_at(&damaged, 0).unwrap();
        assert!(
            native
                .run(
                    &decrypt(
                        descriptor(materials.file(corrupt)),
                        descriptor(materials.file(rejected))
                    ),
                    materials.file(rejected),
                    Duration::from_secs(5),
                    &control
                )
                .await
                .is_err()
        );
        assert!(control.check().is_ok());
        assert!(
            native
                .run(
                    &["not-an-openssl-command".into()],
                    materials.file(rejected),
                    Duration::from_secs(5),
                    &control
                )
                .await
                .is_err()
        );
        for suffix in [
            context.certificate.clone(),
            bytes(materials.file(key), CERTIFICATE),
        ] {
            let other = Fixture::new();
            let mut bundled = self::context(&fs::canonicalize(&other.0).unwrap());
            input(&bundled);
            bundled.certificate = context.certificate.clone();
            bundled.certificate.extend_from_slice(&suffix);
            let mut counted = Counted {
                native: Native {
                    program: "/usr/bin/openssl".into(),
                },
                calls: 0,
            };
            assert!(operation(&bundled, &mut counted, &control).await.is_err());
            assert_eq!(counted.calls, 1);
            assert!(!bundled.output.join("rpmdb.cms.der").exists());
            assert!(!bundled.output.join("manifest.json").exists());
        }
    });
    drop(runtime);
    let closed = materials.finish(false);
    let signal = signals.finish();
    assert!(closed.is_ok());
    assert!(signal.unwrap().is_none());
    assert!(Instant::now() < end);
}
