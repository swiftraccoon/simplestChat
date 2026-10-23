//! Whole five-phase owner with tiny PRIVATE material selections and fake stages.
//! Real source/registry/producer adapters are separately tested. These synthetic
//! bytes are never compiled/executed and cannot replace the production catalog.

use super::super::{archive_tests, fixture};
use super::*;
use crate::ci::{
    command::{Program, SeparateLogs},
    ops_artifact::verify::tools::{Captures, Seal},
};
use serde_json::json;
use std::{fs, io::Write, os::unix::fs::PermissionsExt};

const CRATE: &[u8] = b"tiny original crate bytes";
const DEB: &[u8] = b"tiny original deb bytes";
fn lock() -> Vec<u8> {
    format!(
        concat!(
            "version = 4\n[[package]]\nname = \"inert\"\nversion = \"1.0.0\"\n",
            "source = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
            "checksum = \"{}\"\n"
        ),
        super::super::super::sha(CRATE)
    )
    .into_bytes()
}
fn mkdir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn context(root: &Path) -> Context {
    Context {
        repository: "swiftraccoon/simplestChat",
        revision: "a".repeat(40),
        package_version: env!("CARGO_PKG_VERSION"),
        protocol_version: 1,
        target: super::super::context::TARGET,
        build_run_id: 123,
        run_attempt: 2,
        workspace: root.into(),
    }
}
fn request(path: &Path, selected: &str) -> Value {
    let bytes = fs::read(path).unwrap();
    assert_eq!(super::super::super::sha(&bytes), selected);
    serde_json::from_slice(&bytes).unwrap()
}
fn tiny_rust(artifact: &super::super::catalog::Artifact) -> Vec<u8> {
    let files = match artifact.component.as_str() {
        "rustc" => vec![
            ("bin/rustc".to_owned(), b"inert rustc".to_vec()),
            (
                format!("lib/rustlib/{}/bin/rust-lld", super::super::context::HOST),
                b"inert lld".to_vec(),
            ),
        ],
        "cargo" => vec![("bin/cargo".to_owned(), b"inert cargo".to_vec())],
        name => vec![(
            format!(
                "lib/rustlib/{}/lib/inert.rlib",
                name.strip_prefix("rust-std-").unwrap()
            ),
            b"inert std".to_vec(),
        )],
    };
    let manifest = files
        .iter()
        .map(|(name, _)| format!("file:{name}\n"))
        .collect::<String>();
    let mut entries = vec![
        (
            format!("{}/components", artifact.archive_root),
            format!("{}\n", artifact.component).into_bytes(),
        ),
        (
            format!("{}/rust-installer-version", artifact.archive_root),
            b"3\n".to_vec(),
        ),
        (
            format!(
                "{}/{}/manifest.in",
                artifact.archive_root, artifact.component
            ),
            manifest.into_bytes(),
        ),
    ];
    entries.extend(files.into_iter().map(|(name, bytes)| {
        (
            format!("{}/{}/{name}", artifact.archive_root, artifact.component),
            bytes,
        )
    }));
    archive_tests::gzip(&archive_tests::tar_files(&entries))
}
fn tiny_deb_tar() -> Vec<u8> {
    let mut files = vec![(
        "usr/bin/x86_64-linux-gnu-gcc-13".to_owned(),
        b"inert gcc".to_vec(),
    )];
    files.extend(
        ["as", "ld", "ar", "nm", "ranlib", "strip"]
            .iter()
            .map(|name| {
                (
                    format!("usr/bin/x86_64-linux-gnu-{name}"),
                    b"inert binutils".to_vec(),
                )
            }),
    );
    files.extend(
        ["cc1", "collect2", "crtbeginS.o", "crtendS.o", "libgcc.a"]
            .iter()
            .map(|name| {
                (
                    format!("usr/lib/gcc/x86_64-linux-gnu/13/{name}"),
                    b"inert gcc role".to_vec(),
                )
            }),
    );
    files.extend(
        ["Scrt1.o", "crti.o", "crtn.o", "libc.so"]
            .iter()
            .map(|name| {
                (
                    format!("usr/lib/x86_64-linux-gnu/{name}"),
                    b"inert crt".to_vec(),
                )
            }),
    );
    archive_tests::tar_files(&files)
}
fn catalog() -> (Catalog, BTreeMap<String, Vec<u8>>) {
    let compiled = Catalog::load().unwrap();
    let mut artifacts = Vec::new();
    let mut downloads = BTreeMap::new();
    let mut has_deb = false;
    for mut artifact in compiled.artifacts {
        let bytes = if artifact.format == Format::RustGzip {
            tiny_rust(&artifact)
        } else {
            if has_deb {
                continue;
            }
            has_deb = true;
            DEB.to_vec()
        };
        artifact.sha256 = super::super::super::sha(&bytes);
        artifact.bytes = bytes.len() as u64;
        downloads.insert(artifact.url.clone(), bytes);
        artifacts.push(artifact);
    }
    downloads.insert(
        "https://static.crates.io/crates/inert/inert-1.0.0.crate".into(),
        CRATE.to_vec(),
    );
    let total = artifacts.iter().map(|artifact| artifact.bytes).sum();
    let catalog = Catalog {
        identity: super::super::catalog::Identity {
            profile: "ops-hosted-kit-v1",
            sha256: super::super::super::sha(b"PRIVATE synthetic fixture catalog"),
            bytes: 1,
            archives: 5,
            archive_bytes: total,
        },
        artifacts,
    };
    (catalog, downloads)
}
struct FakeStages {
    calls: Vec<&'static str>,
    producer_failure: Option<u8>,
}
impl Stages for FakeStages {
    async fn source<E: Executor, C: super::super::super::Clock>(
        &mut self,
        path: &Path,
        selected: &str,
        budget: &Budget,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Vec<u8>> {
        self.calls.push("source");
        let request = request(path, selected);
        let output = Path::new(request["output"].as_str().unwrap());
        mkdir(&output.join("source"));
        fs::write(output.join("source/Cargo.lock"), lock()).unwrap();
        fs::write(
            output.join("source/Cargo.toml"),
            b"[workspace]\nmembers=[]\n",
        )
        .unwrap();
        let identity = Box::pin(materials::measure(
            &output.join("source"),
            budget,
            executor,
            deadline,
        ))
        .await?;
        provenance::encoded(
            &json!({"schemaVersion":1,"kind":"ops-source-export-observation",
            "selection":request["selection"],
            "commit":request["selection"]["revision"],"source":identity}),
        )
    }
    async fn registry<E: Executor, C: super::super::super::Clock>(
        &mut self,
        path: &Path,
        selected: &str,
        budget: &Budget,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Vec<u8>> {
        self.calls.push("registry");
        let request = request(path, selected);
        let archives = Path::new(request["archivesRoot"].as_str().unwrap());
        assert_eq!(fs::read(archives.join("inert-1.0.0.crate")).unwrap(), CRATE);
        let output = Path::new(request["output"].as_str().unwrap());
        mkdir(&output.join("registry/inert-1.0.0"));
        fs::write(
            output.join("registry/inert-1.0.0/Cargo.toml"),
            b"[package]\nname='inert'\nversion='1.0.0'\n",
        )
        .unwrap();
        let identity = Box::pin(materials::measure(
            &output.join("registry"),
            budget,
            executor,
            deadline,
        ))
        .await?;
        provenance::encoded(
            &json!({"schemaVersion":1,"kind":"ops-offline-registry-observation",
            "registry":identity,"lockSha256":request["lock"]["sha256"]}),
        )
    }
    async fn producer<E: Executor, C: super::super::super::Clock>(
        &mut self,
        path: &Path,
        selected: &str,
        budget: &Budget,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Vec<u8>> {
        self.calls.push("producer");
        let request = request(path, selected);
        let source = Box::pin(materials::measure(
            Path::new(request["source"]["path"].as_str().unwrap()),
            budget,
            executor,
            deadline,
        ))
        .await?;
        let kit = Box::pin(materials::measure(
            Path::new(request["builderKit"]["path"].as_str().unwrap()),
            budget,
            executor,
            deadline,
        ))
        .await?;
        assert_eq!(source.sha256, request["source"]["sha256"]);
        assert_eq!(kit.sha256, request["builderKit"]["sha256"]);
        if let Some(code) = self.producer_failure {
            return Err(failure(code));
        }
        let output = Path::new(request["output"].as_str().unwrap());
        mkdir(output);
        let binary = super::super::super::fixture::binary();
        let binary_path = output.join(format!(
            "target/{}/release/simplestchat-ops",
            super::super::context::TARGET
        ));
        mkdir(binary_path.parent().unwrap());
        fs::write(&binary_path, &binary).unwrap();
        let documents = [
            (
                "producer-materials.json",
                json!({"schemaVersion":1,"source":source,"builderKit":kit}),
            ),
            (
                "producer-outcome.json",
                json!({"schemaVersion":1,"kind":"ops-cargo-observation","commandStatus":0,
                "cargoAndProbeCliGroupsSettled":true,"sourceConfigAndToolOwnersClosed":true}),
            ),
            ("producer-recipe.json", json!({"schemaVersion":1})),
            ("producer-resolution.json", json!({"schemaVersion":1})),
            ("producer-toolchain.json", json!({"schemaVersion":1})),
        ];
        let mut hashes = BTreeMap::new();
        for (name, value) in documents {
            let bytes = provenance::encoded(&value)?;
            hashes.insert(name, super::super::super::sha(&bytes));
            fs::write(output.join(name), bytes).unwrap();
        }
        let mut manifest = request["selection"].clone();
        let object = manifest.as_object_mut().unwrap();
        object.insert("schemaVersion".into(), 1.into());
        object.insert("artifactType".into(), "simplestchat-ops".into());
        object.insert(
            "binary".into(),
            json!({"name":"simplestchat-ops",
            "sha256":super::super::super::sha(&binary),"bytes":binary.len()}),
        );
        object.insert(
            "inputDigests".into(),
            json!({"source-tree/frame-v1":source.sha256,"builder-kit/frame-v1":kit.sha256}),
        );
        object.insert(
            "evidenceDigests".into(),
            serde_json::to_value(hashes).unwrap(),
        );
        fs::write(
            output.join("ops-release.json"),
            provenance::encoded(&manifest)?,
        )
        .unwrap();
        provenance::encoded(&json!({"schemaVersion":1,"kind":"ops-unsigned-candidate-observation"}))
    }
}
struct FakeCli {
    downloads: BTreeMap<String, Vec<u8>>,
    calls: Vec<String>,
    fail: Option<&'static str>,
    clock: Arc<fixture::Clock>,
    replayed: Vec<u8>,
    clean: bool,
}
async fn finish_logs(
    logs: Arc<SeparateLogs>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
) -> Result<Captures> {
    let logs = Arc::try_unwrap(logs).map_err(|_| failure(1))?;
    let a = logs.stdout.writer()?;
    let b = logs.stderr.writer()?;
    let wrote = tokio::task::spawn_blocking(move || {
        let mut a = a;
        let mut b = b;
        let primary = a
            .write_all(&stdout)
            .and(b.write_all(&stderr))
            .map_err(|_| failure(1));
        let closed_a = nix::unistd::close(a).map_err(|_| failure(1));
        let closed_b = nix::unistd::close(b).map_err(|_| failure(1));
        primary.and(closed_a).and(closed_b)
    })
    .await
    .map_err(|_| failure(1))?;
    let stdout = logs.stdout.finish().await;
    let stderr = logs.stderr.finish().await;
    wrote?;
    let stdout = stdout?;
    let stderr = stderr?;
    if !stdout.complete || !stderr.complete {
        return Err(failure(1));
    }
    Ok(Captures {
        stdout: Seal {
            sha256: stdout.sha256,
            bytes: stdout.bytes,
            metadata: stdout.metadata,
        },
        stderr: Seal {
            sha256: stderr.sha256,
            bytes: stderr.bytes,
            metadata: stderr.metadata,
        },
    })
}
impl Executor for FakeCli {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let proof = command
            .hosted_ops
            .clone()
            .expect("only fixed material/probe roles");
        proof.begin(&command)?;
        let (directory, _) = proof.context(command.program, &command.arguments)?;
        let mut stderr = Vec::new();
        let (kind, stdout) = match command.program {
            Program::OpsMaterialCurl => {
                let url = command.arguments.last().unwrap().to_str().unwrap();
                self.calls.push(format!("curl:{url}"));
                stderr = b"OPS_HTTP_STATUS:200:200\n".to_vec();
                ("curl", self.downloads.get(url).unwrap().clone())
            }
            Program::OpsDpkgDeb => {
                self.calls.push("dpkg".into());
                let input =
                    Arc::try_unwrap(command.stdin.take().unwrap()).map_err(|_| failure(1))?;
                use std::io::Read;
                let mut reader = &input;
                let mut original = Vec::new();
                let read = reader.read_to_end(&mut original);
                let closed = nix::unistd::close(input);
                read.unwrap();
                closed.unwrap();
                assert_eq!(original, DEB);
                ("dpkg", tiny_deb_tar())
            }
            Program::OpsHostCc => {
                let argument = command.arguments[0].to_str().unwrap();
                self.calls.push(format!("cc:{argument}"));
                let stdout = if argument == "--version" {
                    b"inert gcc 13.3.0\n".to_vec()
                } else if argument == "-print-sysroot" {
                    format!("{}\n", directory.join("host-tools/sysroot").display()).into_bytes()
                } else if argument == "-print-search-dirs" {
                    b"install: owned\nprograms: owned\nlibraries: owned\n".to_vec()
                } else if let Some(name) = argument.strip_prefix("-print-prog-name=") {
                    let relative = if ["cc1", "collect2"].contains(&name) {
                        format!("usr/lib/gcc/x86_64-linux-gnu/13/{name}")
                    } else {
                        format!("usr/bin/{name}")
                    };
                    format!(
                        "{}\n",
                        directory
                            .join("host-tools/sysroot")
                            .join(relative)
                            .display()
                    )
                    .into_bytes()
                } else {
                    let name = argument.strip_prefix("-print-file-name=").unwrap();
                    let prefix = if ["crtbeginS.o", "crtendS.o", "libgcc.a"].contains(&name) {
                        "usr/lib/gcc/x86_64-linux-gnu/13"
                    } else {
                        "usr/lib/x86_64-linux-gnu"
                    };
                    format!(
                        "{}\n",
                        directory
                            .join("host-tools/sysroot")
                            .join(prefix)
                            .join(name)
                            .display()
                    )
                    .into_bytes()
                };
                ("cc", stdout)
            }
            _ => panic!("unexpected CLI role"),
        };
        let seals = finish_logs(command.separate_logs.take().unwrap(), stdout, stderr).await?;
        proof.observed_io(Some(seals));
        if self.fail == Some(kind) {
            Err(failure(42))
        } else {
            Ok(Vec::new())
        }
    }
    async fn ready(&mut self) -> Result<()> {
        Ok(())
    }
    async fn replay(&mut self, bytes: &[u8], _: bool, _: Instant) -> Result<()> {
        self.replayed = bytes.to_vec();
        if self.fail == Some("late-clock") {
            self.clock.advance(plan::ENTRY);
        }
        if self.fail == Some("late-signal") {
            Err(failure(143))
        } else {
            Ok(())
        }
    }
    fn cleanup_passed(&self) -> bool {
        self.clean
    }
}

struct DiagnosticSink {
    bytes: Vec<u8>,
    budget: Budget,
    broken: bool,
    terminals: usize,
}
impl Write for DiagnosticSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes
            .windows(b"event=finished".len())
            .any(|part| part == b"event=finished")
        {
            assert_eq!(
                self.budget.count(),
                0,
                "no terminal result before owned closes"
            );
            self.terminals += 1;
        }
        self.bytes.extend_from_slice(bytes);
        if self.broken {
            Err(std::io::Error::other("PRIVATE diagnostic write refusal"))
        } else {
            Ok(bytes.len())
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn operation(
    fail: Option<&'static str>,
) -> (
    fixture::Fixture,
    Vec<&'static str>,
    FakeCli,
    Result<()>,
    Vec<u8>,
) {
    operation_with_sink(fail, false).await
}
async fn operation_with_sink(
    fail: Option<&'static str>,
    broken: bool,
) -> (
    fixture::Fixture,
    Vec<&'static str>,
    FakeCli,
    Result<()>,
    Vec<u8>,
) {
    let fixture = fixture::Fixture::new();
    let workspace = fixture.root.join("checkout");
    mkdir(&workspace);
    let tools = fixture.root.join("tools");
    mkdir(&tools);
    for name in ["git", "curl", "dpkg-deb"] {
        let path = tools.join(name);
        fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let captured = Environment::from([("PATH".into(), tools.into_os_string())]);
    let context = context(&workspace);
    let budget = Budget::default();
    let clock = Arc::new(fixture::Clock::new());
    let plan = Plan::new(clock.clone());
    let (catalog, downloads) = catalog();
    let mut cli = FakeCli {
        downloads,
        calls: Vec::new(),
        fail,
        clock,
        replayed: Vec::new(),
        clean: fail != Some("cleanup"),
    };
    let mut stages = FakeStages {
        calls: Vec::new(),
        producer_failure: match fail {
            Some("producer") => Some(42),
            Some("producer-deadline") => Some(124),
            Some("producer-int") => Some(130),
            Some("producer-term") => Some(143),
            _ => None,
        },
    };
    let run = OwnedRun::local(&fixture.root.join("result"), &budget).unwrap();
    let mut diagnostic = DiagnosticSink {
        bytes: Vec::new(),
        budget: budget.clone(),
        broken,
        terminals: 0,
    };
    let mut progress = Progress::new(&plan, &mut diagnostic);
    let produced = Box::pin(produce_with(
        &context,
        &run,
        &captured,
        &mut progress,
        &mut cli,
        catalog,
        &mut stages,
    ))
    .await;
    assert!(!progress.finished());
    let closed = run.finish();
    let result = completion(produced, closed, &plan, &mut cli, &budget).await;
    assert_eq!(budget.count(), 0);
    let (attempted, fallback) = budget.close_counts();
    assert!(attempted > 0);
    assert_eq!(fallback, 0);
    progress.finish(&result);
    assert_eq!(diagnostic.terminals, 1);
    (fixture, stages.calls, cli, result, diagnostic.bytes)
}
#[tokio::test]
async fn whole_private_fixture_reaches_all_five_phases_and_nine_evidence_publication() {
    let (fixture, stages, cli, result, _) = Box::pin(operation(None)).await;
    result.unwrap();
    assert_eq!(stages, ["source", "registry", "producer"]);
    assert_eq!(cli.calls.len(), 21);
    assert!(cli.calls[..6].iter().all(|call| call.starts_with("curl:")));
    assert_eq!(cli.calls[6], "dpkg");
    assert!(cli.calls[7..].iter().all(|call| call.starts_with("cc:")));
    let observation: Value = serde_json::from_slice(&cli.replayed).unwrap();
    assert_eq!(observation["artifact"]["evidenceCount"], 9);
    assert_eq!(
        fs::read_dir(fixture.root.join("result/unsigned"))
            .unwrap()
            .count(),
        14
    );
    let index: Value = serde_json::from_slice(
        &fs::read(fixture.root.join("result/unsigned/ops-evidence.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        index["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["logicalName"].as_str().unwrap())
            .collect::<Vec<_>>(),
        provenance::NAMES
    );
}
#[tokio::test]
async fn whole_private_fixture_retains_failure_and_blocks_later_stages_or_positive_replay() {
    for fail in [
        "curl",
        "dpkg",
        "cc",
        "producer",
        "cleanup",
        "late-clock",
        "late-signal",
    ] {
        let (fixture, stages, cli, result, _) = Box::pin(operation(Some(fail))).await;
        super::super::fixture::expect_error(
            result,
            match fail {
                "late-clock" => 124,
                "late-signal" => 143,
                "cleanup" => 1,
                _ => 42,
            },
        );
        if ["curl", "dpkg", "cc", "producer"].contains(&fail) {
            assert!(!fixture.root.join("result/unsigned").exists());
            assert!(cli.replayed.is_empty());
        }
        if fail == "curl" {
            assert_eq!(stages, ["source"]);
            assert_eq!(cli.calls.len(), 1);
        }
        if ["dpkg", "cc"].contains(&fail) {
            assert_eq!(stages, ["source", "registry"]);
        }
        if ["late-clock", "late-signal"].contains(&fail) {
            assert!(fixture.root.join("result/unsigned").exists());
            assert!(!cli.replayed.is_empty());
        }
    }
}

#[tokio::test]
async fn diagnostics_keep_stage_primary_and_late_completion_errors_after_all_owned_closes() {
    for broken in [false, true] {
        for (fail, code, phase) in [
            ("producer", 42, "producer"),
            ("producer-deadline", 124, "producer"),
            ("producer-int", 130, "producer"),
            ("producer-term", 143, "producer"),
            ("cleanup", 1, "finalization"),
            ("late-clock", 124, "finalization"),
            ("late-signal", 143, "finalization"),
        ] {
            let (fixture, _, _, result, diagnostics) =
                Box::pin(operation_with_sink(Some(fail), broken)).await;
            super::super::fixture::expect_error(result, code);
            let text = std::str::from_utf8(&diagnostics).unwrap();
            assert!(text.lines().last().unwrap().contains(&format!(
                "event=finished status={code} refusedPhase={phase} "
            )));
            assert!(!text.contains("PRIVATE"));
            assert!(!text.contains(fixture.root.to_str().unwrap()));
            assert!(!text.contains("https://"));
            assert!(diagnostics.len() <= super::super::progress::MAX_BYTES);
        }
        let (_, stages, _, result, diagnostics) = Box::pin(operation_with_sink(None, broken)).await;
        result.unwrap();
        assert_eq!(stages, ["source", "registry", "producer"]);
        let text = std::str::from_utf8(&diagnostics).unwrap();
        assert!(
            text.lines()
                .last()
                .unwrap()
                .contains("event=finished status=0 refusedPhase=none ")
        );
    }
}
