use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

// Keep this in sync with package.include in Cargo.toml. Meson's downloaded and
// extracted subprojects are deliberately excluded; only the authenticated wrap
// files and maintained package overlays are source inputs.
const PACKAGE_SOURCE_PATHS: &[&str] = &[
    "deps/libwebrtc",
    "fbs",
    "fuzzer/include",
    "fuzzer/src",
    "mocks/include",
    "mocks/src",
    "include",
    "scripts",
    "src",
    "subprojects/packagefiles",
    "test/include",
    "test/src",
    "build.rs",
    "Cargo.toml",
    "meson.build",
    "meson_options.txt",
    "tasks.py",
];

const GENERATED_SOURCE_PATHS: &[&str] = &["include/FBS", "scripts/node_modules"];

const RERUN_ENVIRONMENT: &[&str] = &[
    "AR",
    "CC",
    "CFLAGS",
    "CPPFLAGS",
    "CXX",
    "CXXFLAGS",
    "DOCS_RS",
    "LDFLAGS",
    "MACOSX_DEPLOYMENT_TARGET",
    "MESON",
    "MESON_ARGS",
    "MESON_VERSION",
    "NINJA_VERSION",
    "OPENSSL_DIR",
    "OPENSSL_STATIC",
    "PATH",
    "PIP_CERT",
    "PIP_CLIENT_CERT",
    "PIP_CONFIG_FILE",
    "PIP_EXTRA_INDEX_URL",
    "PIP_FIND_LINKS",
    "PIP_INDEX_URL",
    "PIP_NO_BINARY",
    "PIP_NO_INDEX",
    "PIP_ONLY_BINARY",
    "PIP_PREFER_BINARY",
    "PIP_PROXY",
    "PIP_REQUIRE_HASHES",
    "PIP_TRUSTED_HOST",
    "PKG_CONFIG_PATH",
    "PIP_CONSTRAINT",
    "PYTHON",
    "PYTHONPATH",
    "SDKROOT",
];

fn copy_source_entry(source_root: &Path, source: &Path, destination: &Path) {
    let relative = source
        .strip_prefix(source_root)
        .expect("native source entry escaped package root");
    if GENERATED_SOURCE_PATHS
        .iter()
        .any(|generated| relative == Path::new(generated))
    {
        return;
    }

    let metadata = fs::symlink_metadata(source).expect("Failed to inspect native source entry");
    if metadata.file_type().is_symlink() {
        panic!("Symlinks are not supported in native source snapshot: {relative:?}");
    }

    if metadata.is_dir() {
        fs::create_dir_all(destination).expect("Failed to create native source copy directory");

        for entry in fs::read_dir(source).expect("Failed to read native source directory") {
            let entry = entry.expect("Failed to read native source entry");
            copy_source_entry(
                source_root,
                &entry.path(),
                &destination.join(entry.file_name()),
            );
        }
    } else if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).expect("Failed to create native source file parent");
        }
        fs::copy(source, destination).expect("Failed to copy native source file");

        // Cargo registry and Nix sources may be read-only. Copied files must be
        // removable on the next build attempt, including on Windows.
        #[cfg(windows)]
        {
            let mut permissions = fs::metadata(destination)
                .expect("Failed to inspect copied native source file")
                .permissions();
            permissions.set_readonly(false);
            fs::set_permissions(destination, permissions)
                .expect("Failed to make copied native source file writable");
        }
    } else {
        panic!("Unsupported native source entry type: {relative:?}");
    }
}

fn copy_package_sources(source_root: &Path, destination_root: &Path) {
    fs::create_dir_all(destination_root).expect("Failed to create native source copy directory");

    for relative in PACKAGE_SOURCE_PATHS {
        let source = source_root.join(relative);
        copy_source_entry(source_root, &source, &destination_root.join(relative));
    }

    let subprojects = source_root.join("subprojects");
    for entry in fs::read_dir(&subprojects).expect("Failed to read native subprojects directory") {
        let entry = entry.expect("Failed to read native source entry");
        let file_type = entry
            .file_type()
            .expect("Failed to read native source entry type");
        if file_type.is_file() && entry.path().extension() == Some(OsStr::new("wrap")) {
            copy_source_entry(
                source_root,
                &entry.path(),
                &destination_root.join("subprojects").join(entry.file_name()),
            );
        }
    }
}

fn normalize_command(value: OsString, source_root: &Path) -> OsString {
    let path = Path::new(&value);
    if path.is_relative() && path.components().count() > 1 {
        source_root.join(path).into_os_string()
    } else {
        value
    }
}

fn resolve_path_environment(name: &str, source_root: &Path) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                source_root.join(path)
            }
        })
}

fn add_pip_constraint(command: &mut Command, pip_constraint: Option<&Path>) {
    if let Some(path) = pip_constraint {
        command.env("PIP_CONSTRAINT", path);
    }
}

fn main() {
    let source_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by Cargo"),
    );
    let docs_rs = env::var_os("DOCS_RS").is_some();

    // pkg-config emits rerun-if-env-changed directives. Once any rerun-if
    // directive is emitted, Cargo stops its default whole-package file scan,
    // so explicitly preserve that complete native-input boundary.
    println!("cargo:rerun-if-changed=.");
    for name in RERUN_ENVIRONMENT {
        println!("cargo:rerun-if-env-changed={name}");
    }

    // pip only sees the constraint path in its environment. Track the file too,
    // so changing constraints in place invalidates cached native build outputs.
    let pip_constraint = resolve_path_environment("PIP_CONSTRAINT", &source_dir);
    if let Some(path) = &pip_constraint {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    for name in ["PIP_CERT", "PIP_CLIENT_CERT", "PIP_CONFIG_FILE"] {
        if let Some(path) = resolve_path_environment(name, &source_dir) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    if !docs_rs {
        let path = pip_constraint.as_deref().unwrap_or_else(|| {
            panic!("PIP_CONSTRAINT must name the repository's pinned Python constraints file")
        });
        if !path.is_file() {
            panic!("PIP_CONSTRAINT is not a file: {}", path.display());
        }
    }

    // pkg-config tracks its search-path environment but not the files that it
    // resolves. The supported build contract sets OPENSSL_DIR to the immutable,
    // versioned static OpenSSL prefix; also track those concrete compile/link
    // inputs so an in-place security rebuild cannot reuse an older worker.
    let openssl_dir = resolve_path_environment("OPENSSL_DIR", &source_dir);
    if let Some(path) = &openssl_dir {
        for relative in [
            "include/openssl",
            "lib/pkgconfig/openssl.pc",
            "lib/libssl.a",
            "lib/libcrypto.a",
        ] {
            println!("cargo:rerun-if-changed={}", path.join(relative).display());
        }
    }

    // On Windows Rust always links against release version of MSVC runtime, thus requires
    // Release build here
    let build_type = if cfg!(all(debug_assertions, not(windows))) {
        "Debug"
    } else {
        "Release"
    };

    let out_dir = env::var("OUT_DIR").unwrap();
    let cargo_out_dir = PathBuf::from(&out_dir);
    let worker_source_dir = cargo_out_dir.join("worker-src");
    let worker_build_dir = format!("{}/worker-build", out_dir.replace('\\', "/"));
    if !docs_rs {
        // Every build-script attempt starts from a complete, writable source
        // snapshot and an empty native/tool cache. This makes interruptions and
        // compiler, flag, pkg-config, or wrap changes deterministic.
        for directory in [
            &worker_source_dir,
            &cargo_out_dir.join("worker-build"),
            &cargo_out_dir.join("build"),
            &cargo_out_dir.join("out"),
            &cargo_out_dir.join("pip_invoke"),
        ] {
            if directory.exists() {
                fs::remove_dir_all(directory).expect("Failed to remove stale native build input");
            }
        }
        for archive in [
            cargo_out_dir.join("libmediasoup-worker.a"),
            cargo_out_dir.join("mediasoup-worker.lib"),
        ] {
            if archive.exists() {
                fs::remove_file(archive).expect("Failed to remove stale native worker archive");
            }
        }
        copy_package_sources(&source_dir, &worker_source_dir);
    }

    let flatbuffers_source_dir = if docs_rs {
        source_dir.join("fbs")
    } else {
        worker_source_dir.join("fbs")
    };

    // Compile Rust flatbuffers
    let flatbuffers_declarations = planus_translation::translate_files(
        &fs::read_dir(flatbuffers_source_dir)
            .expect("Failed to read `fbs` directory")
            .filter_map(|maybe_entry| {
                maybe_entry
                    .map(|entry| {
                        let path = entry.path();
                        if path.extension() == Some("fbs".as_ref()) {
                            Some(path)
                        } else {
                            None
                        }
                    })
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()
            .expect("Failed to collect flatbuffers files"),
    )
    .expect("Failed to translate flatbuffers files");

    fs::write(
        format!("{out_dir}/fbs.rs"),
        planus_codegen::generate_rust(&flatbuffers_declarations)
            .expect("Failed to generate Rust code from flatbuffers"),
    )
    .expect("Failed to write generated Rust flatbuffers into fbs.rs");

    if docs_rs {
        // Skip everything when building docs on docs.rs
        return;
    }

    // Force forward slashes on Windows too so that is plays well with our tasks.py
    let mediasoup_out_dir = format!("{}/out", out_dir.replace('\\', "/"));
    let cxx = normalize_command(
        env::var_os("CXX").unwrap_or_else(|| OsString::from("c++")),
        &source_dir,
    );

    // Add C++ std lib
    #[cfg(target_os = "linux")]
    {
        let path = Command::new(&cxx)
            .arg("--print-file-name=libstdc++.a")
            .output()
            .expect("Failed to start")
            .stdout;
        println!(
            "cargo:rustc-link-search=native={}",
            String::from_utf8_lossy(&path)
                .trim()
                .strip_suffix("libstdc++.a")
                .expect("Failed to strip suffix"),
        );
        println!("cargo:rustc-link-lib=static=stdc++");
    }

    #[cfg(any(
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    {
        let path = Command::new(&cxx)
            .arg("--print-file-name=libc++.a")
            .output()
            .expect("Failed to start")
            .stdout;
        println!(
            "cargo:rustc-link-search=native={}",
            String::from_utf8_lossy(&path)
                .trim()
                .strip_suffix("libc++.a")
                .expect("Failed to strip suffix"),
        );
        println!("cargo:rustc-link-lib=static=c++");
    }

    #[cfg(target_os = "macos")]
    {
        let path = Command::new("xcrun")
            .arg("--show-sdk-path")
            .output()
            .expect("Failed to start")
            .stdout;

        let libpath = format!(
            "{}/usr/lib",
            String::from_utf8(path)
                .expect("Failed to decode path")
                .trim()
        );
        println!("cargo:rustc-link-search={libpath}");
        println!("cargo:rustc-link-lib=dylib=c++");
        println!("cargo:rustc-link-lib=dylib=c++abi");
    }

    // Install Python invoke package in custom folder
    let pip_invoke_dir = cargo_out_dir.join("pip_invoke");
    let python = normalize_command(
        env::var_os("PYTHON").unwrap_or_else(|| OsString::from("python3")),
        &source_dir,
    );
    let mut python_paths = vec![pip_invoke_dir.clone()];
    if let Some(original_pythonpath) = env::var_os("PYTHONPATH") {
        python_paths.extend(env::split_paths(&original_pythonpath).map(|path| {
            if path.is_absolute() {
                path
            } else {
                source_dir.join(path)
            }
        }));
    }
    let pythonpath = env::join_paths(python_paths).expect("PYTHONPATH contains an invalid path");

    let mut install_invoke = Command::new(&python);
    install_invoke
        .arg("-m")
        .arg("pip")
        .arg("install")
        .arg("--upgrade")
        .arg("--target")
        .arg(&pip_invoke_dir)
        .arg("invoke");
    add_pip_constraint(&mut install_invoke, pip_constraint.as_deref());
    if !install_invoke
        .spawn()
        .expect("Failed to start")
        .wait()
        .expect("Wasn't running")
        .success()
    {
        panic!("Failed to install Python invoke package")
    }

    // Build
    let mut build_worker = Command::new(&python);
    build_worker
        .arg("-m")
        .arg("invoke")
        .arg("libmediasoup-worker")
        .current_dir(&worker_source_dir)
        .env("PYTHONPATH", &pythonpath)
        .env("PYTHON", &python)
        .env("CXX", &cxx)
        .env("MEDIASOUP_OUT_DIR", &mediasoup_out_dir)
        .env("MEDIASOUP_BUILDTYPE", build_type)
        .env("BUILD_DIR", &worker_build_dir)
        // Force forward slashes on Windows too, otherwise Meson thinks path is not absolute 🤷
        .env("MEDIASOUP_INSTALL_DIR", out_dir.replace('\\', "/"));
    for name in ["AR", "CC", "MESON"] {
        if let Some(value) = env::var_os(name) {
            build_worker.env(name, normalize_command(value, &source_dir));
        }
    }
    add_pip_constraint(&mut build_worker, pip_constraint.as_deref());
    if !build_worker
        .spawn()
        .expect("Failed to start")
        .wait()
        .expect("Wasn't running")
        .success()
    {
        panic!("Failed to build libmediasoup-worker")
    }

    #[cfg(target_os = "windows")]
    {
        let dot_a = format!("{out_dir}/libmediasoup-worker.a");
        let dot_lib = format!("{out_dir}/mediasoup-worker.lib");

        // Meson builds `libmediasoup-worker.a` on Windows instead of `*.lib` file under MinGW
        if std::path::Path::new(&dot_a).exists() {
            std::fs::copy(&dot_a, &dot_lib).unwrap_or_else(|error| {
                panic!("Failed to copy static library from {dot_a} to {dot_lib}: {error}");
            });
        }

        // These are required by libuv on Windows
        println!("cargo:rustc-link-lib=psapi");
        println!("cargo:rustc-link-lib=user32");
        println!("cargo:rustc-link-lib=advapi32");
        println!("cargo:rustc-link-lib=iphlpapi");
        println!("cargo:rustc-link-lib=userenv");
        println!("cargo:rustc-link-lib=ws2_32");
        println!("cargo:rustc-link-lib=dbghelp");
        println!("cargo:rustc-link-lib=ole32");
        println!("cargo:rustc-link-lib=uuid");
        println!("cargo:rustc-link-lib=shell32");

        // These are required by OpenSSL on Windows
        println!("cargo:rustc-link-lib=ws2_32");
        println!("cargo:rustc-link-lib=gdi32");
        println!("cargo:rustc-link-lib=advapi32");
        println!("cargo:rustc-link-lib=crypt32");
        println!("cargo:rustc-link-lib=user32");
    }

    // The worker archive deliberately does not contain its own OpenSSL copy.
    // Keep this probe after mediasoup-worker so the static archives appear in
    // dependency order on linkers that still process archives left-to-right.
    println!("cargo:rustc-link-lib=static=mediasoup-worker");
    pkg_config::Config::new()
        // Do not use a plain >=3.5.8 check: CVE-2026-54874 is still present in
        // OpenSSL 3.6.0-3.6.3 and 4.0.0-4.0.1. Stay on the fixed 3.5 LTS line.
        .range_version("3.5.8".."3.6.0")
        .statik(true)
        .probe("openssl")
        .expect("static OpenSSL >= 3.5.8 and < 3.6.0 is required");
    println!("cargo:rustc-link-search=native={out_dir}");
}
