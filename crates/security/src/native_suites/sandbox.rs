//! The offline sandbox a suite runs in, the image it is made of, and what a
//! build and a replay run in it.

use super::{
    FAMILIES, MODES,
    budget::{Budget, MAX_WORKERS},
    hexadecimal as small_hex,
};
use crate::{Fault, Result, require};

/// The label that binds an image to the digest of its inputs.
pub const INPUT_LABEL: &str = "org.simplestchat.native-security.inputs";
/// The label that marks a sandbox as a run's own.
pub const RUN_LABEL: &str = "org.simplestchat.native-security.run";
/// The program each suite builds and runs, by the suite's name.
pub const TARGETS: [(&str, &str); 3] = [
    ("asan", "mediasoup-worker-test-asan-address"),
    ("ubsan", "mediasoup-worker-test-asan-undefined"),
    ("replay", "mediasoup-worker-fuzzer"),
];
/// The Meson of the image's own tools.
const MESON: &str = "/opt/native-tools/pip_meson_ninja/bin/meson";
/// The variables of the image's environment that a build and its programs
/// may see, in the order the reference names them.
const ALLOWED: [&str; 18] = [
    "PATH",
    "OPENSSL_DIR",
    "OPENSSL_STATIC",
    "PKG_CONFIG_PATH",
    "CC",
    "CXX",
    "PYTHON",
    "PYTHONNOUSERSITE",
    "PYTHONDONTWRITEBYTECODE",
    "PYTHONPATH",
    "PIP_CONFIG_FILE",
    "PIP_INDEX_URL",
    "PIP_CONSTRAINT",
    "MEDIASOUP_OUT_DIR",
    "MEDIASOUP_INSTALL_DIR",
    "BUILD_DIR",
    "MEDIASOUP_BUILD_JOBS",
    "ASAN_SYMBOLIZER_PATH",
];

/// The architecture of a host whose machine is `machine`, as an image names
/// it; a sanitizer result under emulation is no native one.
pub fn native_architecture(machine: &str) -> Result<&'static str> {
    match machine {
        "x86_64" | "amd64" => Ok("amd64"),
        "aarch64" | "arm64" => Ok("arm64"),
        _ => Err(Fault("native_host_architecture_unsupported")),
    }
}

/// The full content identity `value` that an engine printed, as
/// `sha256:` and sixty-four small hexadecimal digits.
pub fn canonical_image_id(value: &str) -> Result<String> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    require(small_hex(digest, 64), "invalid_engine_image_id")?;
    Ok(format!("sha256:{digest}"))
}

/// Whether `image` is an immutable local image identity.
pub fn immutable(image: &str) -> bool {
    image
        .strip_prefix("sha256:")
        .is_some_and(|digest| small_hex(digest, 64))
}

/// The arguments that create the run `run_id`'s sandbox of the image
/// `image` built from the inputs `digest`, on `architecture`, within
/// `budget`: no host path, published port, network, privilege or unbounded
/// storage; two gibibytes for each compiler and one for Meson, the
/// generator and the programs. The image is the last argument.
pub fn sandbox_args(
    image: &str,
    run_id: &str,
    digest: &str,
    budget: Budget,
    architecture: &str,
) -> Result<Vec<String>> {
    require(immutable(image), "immutable_image_required")?;
    require(small_hex(run_id, 32), "run_identity")?;
    let memory = budget.workers * 2048 + 1024;
    let enough = budget.query_ram_mib >= i64::try_from(budget.workers * 2048).unwrap_or(i64::MAX);
    require(enough, "native_insufficient_memory")?;
    let workers = budget.workers;
    Ok(vec![
        "create".to_owned(),
        format!("--platform=linux/{architecture}"),
        "--rm".to_owned(),
        "--name".to_owned(),
        format!("simplestchat-native-security-{run_id}"),
        "--label".to_owned(),
        format!("{RUN_LABEL}={run_id}"),
        "--label".to_owned(),
        format!("{INPUT_LABEL}={digest}"),
        "--network=none".to_owned(),
        "--log-driver=none".to_owned(),
        "--read-only".to_owned(),
        "--cap-drop=ALL".to_owned(),
        "--security-opt=no-new-privileges:true".to_owned(),
        "--user=65532:65532".to_owned(),
        format!("--cpus={workers}"),
        format!("--memory={memory}m"),
        format!("--memory-swap={memory}m"),
        format!("--env=MEDIASOUP_BUILD_JOBS={workers}"),
        "--pids-limit=256".to_owned(),
        "--ulimit=core=0:0".to_owned(),
        // Meson runs compiler checks and the built tests here; a tmpfs is
        // not executable unless it says so.
        "--tmpfs=/work:rw,exec,nosuid,nodev,size=5g,mode=1777".to_owned(),
        "--tmpfs=/tmp:rw,noexec,nosuid,nodev,size=256m,mode=1777".to_owned(),
        image.to_owned(),
    ])
}

/// The program the suite `mode` builds and runs.
pub fn target(mode: &str) -> Result<&'static str> {
    let found = TARGETS.iter().find(|(name, _)| *name == mode);
    found
        .map(|(_, target)| *target)
        .ok_or(Fault("native_compile_options"))
}

/// The four phases of the build of the suite `mode` with `workers`
/// compilers, in their order: the upstream sanitizer targets, unchanged,
/// and a bounded generator too.
pub fn compile_commands(mode: &str, workers: u64) -> Result<Vec<(&'static str, Vec<String>)>> {
    let known = MODES.contains(&mode) && (1..=MAX_WORKERS).contains(&workers);
    require(known, "native_compile_options")?;
    let target = target(mode)?;
    let sanitizer = if mode == "ubsan" {
        "undefined"
    } else {
        "address"
    };
    let built = if mode == "replay" {
        "-Dms_build_fuzzer=true"
    } else {
        "-Dms_build_tests=true"
    };
    let words = |words: &[&str]| {
        words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>()
    };
    let jobs = workers.to_string();
    Ok(vec![
        (
            "configure",
            words(&[
                MESON,
                "setup",
                "--prefix",
                "/work/install",
                "--bindir",
                "",
                "--libdir",
                "",
                "--buildtype",
                "release",
                "-Db_ndebug=true",
                "--wrap-mode=nodownload",
                built,
                &format!("-Db_sanitize={sanitizer}"),
                "-Db_lundef=false",
                "/work/build",
            ]),
        ),
        (
            "generator",
            words(&[
                MESON,
                "compile",
                "-C",
                "/work/build",
                "-j",
                &jobs,
                "flatbuffers-generator",
            ]),
        ),
        (
            "compile",
            words(&[MESON, "compile", "-C", "/work/build", "-j", &jobs, target]),
        ),
        (
            "install",
            words(&[
                MESON,
                "install",
                "-C",
                "/work/build",
                "--no-rebuild",
                "--tags",
                target,
            ]),
        ),
    ])
}

/// The environment of a build or a program: only what the image prepared,
/// as `ambient` says it, the fixed Meson, Ninja and sanitizer settings, and
/// exactly one selected parser family.
pub fn worker_environment(
    ambient: &dyn Fn(&str) -> Option<String>,
    family: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let mut environment: Vec<(String, String)> = ALLOWED
        .iter()
        .filter_map(|name| ambient(name).map(|value| ((*name).to_owned(), value)))
        .collect();
    let fixed = [
        ("MESON_ARGS", "--wrap-mode=nodownload".to_owned()),
        (
            "NINJA",
            "/opt/native-tools/pip_meson_ninja/bin/ninja".to_owned(),
        ),
        (
            "ASAN_OPTIONS",
            concat!(
                "halt_on_error=1:print_stacktrace=1:detect_leaks=1:symbolize=1:",
                "detect_stack_use_after_return=1:strict_init_order=1:",
                "check_initialization_order=1:detect_container_overflow=1"
            )
            .to_owned(),
        ),
        (
            "UBSAN_OPTIONS",
            "halt_on_error=1:print_stacktrace=1".to_owned(),
        ),
    ];
    for (name, value) in fixed {
        environment.retain(|(held, _)| held != name);
        environment.push((name.to_owned(), value));
    }
    if let Some(family) = family {
        require(FAMILIES.contains(&family), "corpus_family")?;
        environment.push((
            format!("MS_FUZZ_{}", family.to_ascii_uppercase()),
            "1".to_owned(),
        ));
    }
    Ok(environment)
}

/// The command line that runs one reviewed input `path` once through the
/// fuzzer target `program`, with no mutation directory or generated corpus.
pub fn replay_args(program: &str, path: &str) -> Vec<String> {
    [
        program,
        "-timeout=5",
        "-rss_limit_mb=2048",
        "-max_len=65536",
        "-seed=1",
        "-error_exitcode=77",
        "-timeout_exitcode=78",
        path,
    ]
    .map(str::to_owned)
    .to_vec()
}
