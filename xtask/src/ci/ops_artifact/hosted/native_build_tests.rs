//! Inert build inputs and actual mature-format parsing; no target program runs.
use super::{
    archive::Graph, files::Tree, fixture, kit, native_build as native, plan, tools::Tools,
};
use crate::{
    ci::{
        command::{Command, SeparateLogs},
        ops_artifact::{
            namespace::Budget,
            verify::tools::{Captures, Seal},
        },
        process::Executor,
    },
    tooling::{Environment, Failure, Result},
};
use std::{
    ffi::OsStr,
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::Path,
    sync::Arc,
    time::Duration,
};

pub(super) fn config(prefix: &Path) -> Vec<u8> {
    format!("our %config = (\n    \"libdir\" => \"lib\",\n    \"prefix\" => \"{}\",\n    \"target\" => \"linux-x86_64\",\n    \"version\" => \"4.1.0\"\n);\nour %disabled = (\n    \"dso\" => \"option\",\n    \"engine\" => \"option\",\n    \"module\" => \"option\",\n    \"shared\" => \"option\",\n    \"tests\" => \"option\"\n);\n",prefix.display()).into_bytes()
}
#[test]
fn generated_config_requires_actual_explicit_disabled_profile_and_target_identity() {
    let prefix = Path::new("/owned/openssl-prefix");
    let original = config(prefix);
    assert_eq!(
        native::config(&original, prefix).unwrap(),
        serde_json::json!({"shared":true,"dso":true,"module":true,"engine":true})
    );
    let text = std::str::from_utf8(&original).unwrap();
    for changed in [
        text.replace("\"shared\" => \"option\"", "\"shared\" => \"cascade\""),
        text.replace("\"module\" => \"option\",\n", ""),
        text.replace(
            "\"dso\" => \"option\",",
            "\"dso\" => \"option\",\n    \"dso\" => \"option\",",
        ),
        text.replace("linux-x86_64", "linux-aarch64"),
        text.replace("4.1.0", "4.0.0"),
        text.replace("/owned/openssl-prefix", "/unowned/prefix"),
        text.replace("\"libdir\" => \"lib\"", "\"libdir\" => \"lib64\""),
        text.replace("\"tests\" => \"option\"", "\"tests\" => \"default\""),
        text.replace("\"engine\" => \"option\"", "\"engine\" => do { 1 }"),
        format!("{text}{text}"),
    ] {
        fixture::expect_error(native::config(changed.as_bytes(), prefix), 1);
    }
}

#[test]
fn fixed_build_commands_keep_out_of_tree_sources_two_jobs_and_cleared_native_environment() {
    let root = Path::new("/owned/kit");
    let work = Path::new("/owned/native-work");
    let steps = native::arguments(work, root);
    assert_eq!(steps.len(), 6);
    assert_eq!(
        steps[0].args[0].as_os_str(),
        work.join("musl/configure").as_os_str()
    );
    assert_eq!(
        steps[3].args[0].as_os_str(),
        work.join("openssl/Configure").as_os_str()
    );
    assert_eq!(steps[0].directory, work.join("build-musl"));
    assert_eq!(steps[3].directory, work.join("build-openssl"));
    let expected_include = format!("-I{}", root.join(kit::LINUX_INCLUDE).display());
    assert_eq!(
        steps[3]
            .args
            .iter()
            .filter(|arg| arg.as_encoded_bytes().starts_with(b"-I"))
            .collect::<Vec<_>>(),
        [&std::ffi::OsString::from(expected_include)]
    );
    assert_eq!(
        steps
            .iter()
            .map(|step| step.maximum.as_secs())
            .collect::<Vec<_>>(),
        [60, 240, 45, 60, 300, 60]
    );
    assert!(!steps[3].args.iter().any(|arg| arg == "no-secure-memory"));
    for step in &steps {
        assert!(
            !step
                .args
                .iter()
                .any(|arg| arg == "-j" || arg == "-j8" || arg == "test")
        );
        assert!(step.maximum <= Duration::from_secs(300));
    }
    for option in ["no-shared", "no-dso", "no-module", "no-engine", "no-tests"] {
        assert!(steps[3].args.iter().any(|arg| arg == option));
    }
    for musl in [true, false] {
        let env = native::environment(root, work, musl).unwrap();
        assert_eq!(
            env[OsStr::new("CC")].as_os_str(),
            if musl {
                root.join("host-tools/bin/cc")
            } else {
                work.join("musl-prefix/bin/musl-gcc")
            }
            .as_os_str()
        );
        assert_eq!(
            env[OsStr::new("REALGCC")].as_os_str(),
            root.join("host-tools/bin/cc").as_os_str()
        );
        assert_eq!(
            env[OsStr::new("AR")].as_os_str(),
            root.join("host-tools/sysroot/usr/bin/ar").as_os_str()
        );
        for path in std::env::split_paths(&env[OsStr::new("PATH")]) {
            assert!(path.starts_with(root));
        }
        for path in std::env::split_paths(&env[OsStr::new("PERL5LIB")]) {
            assert!(path.starts_with(root));
        }
        assert!(!env.contains_key(OsStr::new("LD_PRELOAD")));
        assert!(!env.contains_key(OsStr::new("PERL5OPT")));
        assert!(!env.contains_key(OsStr::new("CFLAGS")));
    }
}

#[test]
fn linux_uapi_projection_is_complete_nofollow_and_excludes_gnu_libc_headers() {
    for case in [
        "ok",
        "linux",
        "asm-generic",
        "asm",
        "missing-mman",
        "empty-mman",
        "symlink-mman",
    ] {
        let fixture = fixture::Fixture::new();
        let budget = Budget::default();
        let mut tree = Tree::create(&fixture.root.join("kit"), &budget).unwrap();
        for name in [
            "host-tools/sysroot/usr/bin/x86_64-linux-gnu-gcc-13",
            "toolchain/bin/cargo",
            "toolchain/bin/rustc",
            "toolchain/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld",
        ] {
            tree.write(name, b"inert executable admission role\n", true)
                .unwrap();
        }
        tree.directory("toolchain/lib/rustlib/x86_64-unknown-linux-musl/lib")
            .unwrap();
        for name in ["as", "ld", "ar", "nm", "ranlib", "strip"] {
            tree.write(
                &format!("host-tools/sysroot/usr/bin/x86_64-linux-gnu-{name}"),
                b"inert binutils admission role\n",
                true,
            )
            .unwrap();
        }
        for name in ["dash", "mawk"] {
            tree.write(
                &format!("host-tools/sysroot/usr/bin/{name}"),
                b"inert shell/awk admission role\n",
                true,
            )
            .unwrap();
        }
        for (source, subtree, bytes) in [
            ("linux/mman.h", "linux", b"inert kernel mman\n".as_slice()),
            (
                "linux/nested/feature.h",
                "linux",
                b"inert nested kernel header\n".as_slice(),
            ),
            (
                "asm-generic/mman-common.h",
                "asm-generic",
                b"inert generic header\n".as_slice(),
            ),
            (
                "x86_64-linux-gnu/asm/mman.h",
                "asm",
                b"inert architecture header\n".as_slice(),
            ),
        ] {
            if case == subtree || (case == "missing-mman" && source == "linux/mman.h") {
                continue;
            }
            tree.write(
                &format!("host-tools/sysroot/usr/include/{source}"),
                if case == "empty-mman" && source == "linux/mman.h" {
                    b""
                } else {
                    bytes
                },
                false,
            )
            .unwrap();
        }
        for name in [
            "stdio.h",
            "features.h",
            "sys/mman.h",
            "bits/libc-header-start.h",
            "x86_64-linux-gnu/bits/wordsize.h",
            "linux-other/unrelated.h",
        ] {
            tree.write(
                &format!("host-tools/sysroot/usr/include/{name}"),
                b"must stay outside the UAPI projection\n",
                false,
            )
            .unwrap();
        }
        if case == "symlink-mman" {
            let source = tree
                .run
                .path
                .join("host-tools/sysroot/usr/include/linux/mman.h");
            fs::remove_file(&source).unwrap();
            symlink("../stdio.h", &source).unwrap();
        }
        let result = kit::complete(&mut tree, &mut Graph::default(), &mut || Ok(()));
        if case == "ok" {
            result.unwrap();
            let projected = tree
                .records
                .keys()
                .filter(|name| name.starts_with(&format!("{}/", kit::LINUX_INCLUDE)))
                .map(|name| name.strip_prefix(kit::LINUX_INCLUDE).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                projected,
                [
                    "/asm-generic/mman-common.h",
                    "/asm/mman.h",
                    "/linux/mman.h",
                    "/linux/nested/feature.h",
                ]
            );
            for (source, destination) in [
                ("linux/mman.h", "linux/mman.h"),
                ("asm-generic/mman-common.h", "asm-generic/mman-common.h"),
                ("x86_64-linux-gnu/asm/mman.h", "asm/mman.h"),
            ] {
                let original = &tree.records[&format!("host-tools/sysroot/usr/include/{source}")];
                let projected = &tree.records[&format!("{}/{destination}", kit::LINUX_INCLUDE)];
                assert_eq!(projected.sha256, original.sha256);
                assert_eq!(projected.bytes, original.bytes);
                assert_ne!(projected.stamp.ino(), original.stamp.ino());
                assert_eq!(projected.stamp.mode() & 0o7777, 0o600);
            }
        } else {
            fixture::expect_error(result, 1);
        }
        if case == "symlink-mman" {
            fixture::expect_error(tree.finish(), 1);
        } else {
            tree.finish().unwrap();
        }
        assert_eq!(budget.count(), 0);
        assert_eq!(budget.close_counts().1, 0);
    }
}

/// Fixed ELF64LE relocatable header with no sections. It is parsed by object;
/// these fixture bytes are never executed or treated as a compiled artifact.
pub(super) fn elf() -> Vec<u8> {
    let mut bytes = vec![0; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&1_u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&62_u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
    bytes[52..54].copy_from_slice(&64_u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
    bytes[58..60].copy_from_slice(&64_u16.to_le_bytes());
    bytes
}
pub(super) fn archive(data: &[u8]) -> Vec<u8> {
    let mut bytes = b"!<arch>\n".to_vec();
    bytes.extend(
        format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            "fixture.o/",
            0,
            0,
            0,
            "100644",
            data.len()
        )
        .as_bytes(),
    );
    bytes.extend(data);
    if !data.len().is_multiple_of(2) {
        bytes.push(b'\n');
    }
    bytes
}
#[test]
fn actual_archive_and_elf_parsers_refuse_wrong_target_thin_and_nonobjects() {
    let original = elf();
    native::elf(&original).unwrap();
    assert_eq!(native::archive(&archive(&original), false).unwrap(), 1);
    assert_eq!(native::archive(b"!<arch>\n", true).unwrap(), 0);
    fixture::expect_error(native::archive(b"!<arch>\n", false), 1);
    fixture::expect_error(native::archive(b"!<thin>\n", true), 1);
    fixture::expect_error(native::archive(&archive(b"not an ELF object"), false), 1);
    for (offset, value) in [(4, 1), (5, 2), (16, 2), (18, 183)] {
        let mut changed = original.clone();
        changed[offset] = value;
        fixture::expect_error(native::elf(&changed), 1);
        fixture::expect_error(native::archive(&archive(&changed), false), 1);
    }
    fixture::expect_error(native::elf(&original[..60]), 1);
    let mut truncated = archive(&original);
    truncated.pop();
    fixture::expect_error(native::archive(&truncated, false), 1);
}

#[test]
fn generated_headers_are_sorted_nofollow_and_all_selected_originals_close_on_refusal() {
    for case in [
        "ok",
        "symlink",
        "directory-link",
        "hardlink",
        "fifo",
        "executable",
        "writable",
        "interrupt",
        "extra",
    ] {
        let fixture = fixture::Fixture::new();
        let budget = Budget::default();
        let tree = Tree::create(&fixture.root.join("native-work"), &budget).unwrap();
        let include = tree.run.path.join("musl-prefix/include");
        fs::create_dir_all(include.join("bits")).unwrap();
        fs::write(include.join("a.h"), b"header a").unwrap();
        fs::write(include.join("bits/z.h"), b"header z").unwrap();
        let leaf = include.join("a.h");
        match case {
            "symlink" => {
                fs::remove_file(&leaf).unwrap();
                symlink("bits/z.h", &leaf).unwrap();
            }
            "directory-link" => {
                symlink("bits", include.join("linked")).unwrap();
            }
            "hardlink" => fs::hard_link(&leaf, include.join("hard.h")).unwrap(),
            "fifo" => {
                fs::remove_file(&leaf).unwrap();
                nix::unistd::mkfifo(&leaf, nix::sys::stat::Mode::from_bits_truncate(0o600))
                    .unwrap();
            }
            "executable" => fs::set_permissions(&leaf, fs::Permissions::from_mode(0o755)).unwrap(),
            "writable" => fs::set_permissions(&leaf, fs::Permissions::from_mode(0o666)).unwrap(),
            "extra" => fs::write(include.join("unreviewed.txt"), b"other").unwrap(),
            _ => {}
        }
        let generated = native::Generated { tree: &tree };
        let paths = generated.headers("musl-prefix/include", &mut || Ok(()));
        if case == "ok" {
            assert_eq!(
                paths.unwrap(),
                ["musl-prefix/include/a.h", "musl-prefix/include/bits/z.h"]
            );
            assert_eq!(
                generated
                    .data("musl-prefix/include/a.h", 64, &mut || Ok(()))
                    .unwrap(),
                b"header a"
            );
            fixture::expect_error(
                generated.data("musl-prefix/include/a.h", 2, &mut || Ok(())),
                1,
            );
        } else if case == "interrupt" {
            fixture::expect_error(
                generated.data("musl-prefix/include/a.h", 64, &mut || {
                    Err(super::failure(143))
                }),
                143,
            );
        } else if matches!(case, "hardlink" | "executable" | "writable") {
            fixture::expect_error(
                generated.data("musl-prefix/include/a.h", 64, &mut || Ok(())),
                1,
            );
        } else {
            fixture::expect_error(paths, 1);
        }
        tree.finish_build().unwrap();
        assert_eq!(budget.count(), 0);
        assert_eq!(budget.close_counts().1, 0);
    }
}

#[test]
fn native_inventory_frames_names_sizes_hashes_and_fixed_import_modes() {
    let fixture = fixture::Fixture::new();
    let budget = Budget::default();
    let mut tree = Tree::create(&fixture.root.join("kit"), &budget).unwrap();
    tree.write("native/musl/include/a.h", b"a", false).unwrap();
    let initial = native::inventory(&tree).unwrap();
    assert_eq!(initial["files"], 1);
    assert_eq!(initial["bytes"], 1);
    assert_eq!(initial["mode"], "0600");
    tree.write("native/openssl/include/b.h", b"bc", false)
        .unwrap();
    let changed = native::inventory(&tree).unwrap();
    assert_ne!(initial["sha256"], changed["sha256"]);
    assert_eq!(changed["bytes"], 3);
    tree.write("native/forbidden-executable", b"inert", true)
        .unwrap();
    fixture::expect_error(native::inventory(&tree), 1);
    tree.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

struct FailedBuild {
    calls: usize,
    ready: usize,
    source: std::path::PathBuf,
}
impl Executor for FailedBuild {
    async fn ready(&mut self) -> Result<()> {
        self.ready += 1;
        Ok(())
    }
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        self.calls += 1;
        let proof = command.hosted_ops.as_ref().unwrap().clone();
        proof.begin(&command)?;
        let SeparateLogs { stdout, stderr } =
            Arc::try_unwrap(command.separate_logs.take().unwrap()).unwrap();
        let mut writer = stdout.writer()?;
        writer.write_all(b"actual failed inert build\n").unwrap();
        nix::unistd::close(writer).unwrap();
        let (out, err) = tokio::join!(stdout.finish(), stderr.finish());
        let out = out?;
        let err = err?;
        assert!(out.complete && err.complete);
        proof.observed_io(Some(Captures {
            stdout: Seal {
                sha256: out.sha256,
                bytes: out.bytes,
                metadata: out.metadata,
            },
            stderr: Seal {
                sha256: err.sha256,
                bytes: err.bytes,
                metadata: err.metadata,
            },
        }));
        fs::write(&self.source, b"changed source").unwrap();
        Err(Failure::child_exit(23))
    }
}
#[tokio::test]
async fn failed_build_preserves_child_status_seals_both_logs_checks_sources_and_stops_dispatch() {
    let fixture = fixture::Fixture::new();
    let budget = Budget::default();
    let mut work = Tree::create(&fixture.root.join("native-work"), &budget).unwrap();
    work.write("musl/configure", b"inert source", false)
        .unwrap();
    let mut kit = Tree::create(&fixture.root.join("kit"), &budget).unwrap();
    for name in ["dash", "perl", "make"] {
        kit.write(
            &format!("host-tools/sysroot/usr/bin/{name}"),
            b"inert executable",
            true,
        )
        .unwrap();
    }
    let bins = fixture.root.join("bootstrap");
    fs::create_dir(&bins).unwrap();
    for name in ["git", "curl", "dpkg-deb"] {
        let path = bins.join(name);
        fs::write(&path, b"inert executable").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let env = Environment::from([("PATH".into(), bins.into_os_string())]);
    let mut tools = Tools::select(&fixture.root, &env, &budget).unwrap();
    tools.admit_native(&kit.run.path).unwrap();
    let mut downloads = Tree::create(&fixture.root.join("downloads"), &budget).unwrap();
    let clock = Arc::new(fixture::Clock::new());
    let plan = plan::Plan::new(clock);
    let end = plan.now().unwrap() + Duration::from_secs(540);
    let mut executor = FailedBuild {
        calls: 0,
        ready: 0,
        source: work.run.path.join("musl/configure"),
    };
    let mut next = 0;
    let catalog = super::catalog::Catalog::load().unwrap();
    let request = native::NativeBuild {
        catalog: &catalog,
        work: &work,
        tools: &tools,
        plan: &plan,
        end,
    };
    fixture::expect_error(
        native::build(request, &mut kit, &mut downloads, &mut executor, &mut next).await,
        23,
    );
    assert_eq!(executor.calls, 1);
    assert!(executor.ready >= 3);
    assert_eq!(next, 1);
    assert_eq!(downloads.records.len(), 2);
    fixture::expect_error(work.finish_build(), 1);
    downloads.finish().unwrap();
    kit.finish().unwrap();
    tools.finish().unwrap();
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts().1, 0);
}
