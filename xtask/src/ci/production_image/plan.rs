//! Literal engine and Git operations. All dynamic values occupy argv entries.

use super::options::Context;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

pub(super) const ARM_DOCKERFILE: &str = concat!(
    "FROM docker.io/moby/buildkit@sha256:",
    "3ad6bb9bc8c78c0069d03247adb9a59b3b43d68e55353e876e558b888c6c1768\n",
    "RUN test -x /usr/bin/buildkit-qemu-x86_64 && rm /usr/bin/buildkit-qemu-x86_64\n"
);

pub(super) const VERIFY: &str = r#"test "$(id -u)" = 10001
test "$(id -g)" = 10001
test "$(stat -c %u /app/simplestChat)" = 0
test "$(stat -c %u /app/web/dist/index.html)" = 0
test "$(stat -c %u /app/migrations)" = 0
test -x /app/simplestChat
test -s /app/web/dist/index.html
test ! -e /app/load_test
actual_migrations="$(find /app/migrations -maxdepth 1 -type f -name "*.sql" | wc -l)"
test "${actual_migrations}" -eq "$#"
for migration do
  test -s "/app/migrations/${migration}"
done
"#;

pub(super) use crate::ci::command::{Command, Program};

pub(super) fn arm(directory: &Path) -> Command {
    let mut command = Command::capture(
        Program::Docker,
        &[
            "buildx",
            "build",
            "--builder",
            "default",
            "--platform",
            "linux/arm64",
            "--load",
            "--iidfile",
        ],
    );
    command.arguments.extend([
        directory.join("image.iid").into_os_string(),
        directory.into(),
    ]);
    command.timeout = Duration::from_secs(180);
    command.grace = Duration::from_secs(10);
    command.capture = None;
    command
}

pub(super) fn create(name: &str, image: Option<&str>) -> Command {
    let mut command = Command::capture(
        Program::Docker,
        &[
            "buildx",
            "create",
            "--name",
            name,
            "--driver",
            "docker-container",
        ],
    );
    if let Some(image) = image {
        command
            .arguments
            .extend(["--driver-opt".into(), format!("image={image}").into()]);
    }
    command.arguments.push("--use".into());
    command
}

pub(super) fn remove(name: &str) -> Command {
    let mut command = Command::capture(Program::Docker, &["buildx", "rm", "--force", name]);
    command.cleanup = true;
    command
}

pub(super) fn build(context: &Context, name: &str, cache_from: bool) -> Command {
    let mut command = Command::capture(
        Program::Docker,
        &[
            "buildx",
            "build",
            "--builder",
            name,
            "--pull",
            "--load",
            "--target",
            "production",
            "--platform",
            "linux/amd64",
            "--provenance=false",
            "--sbom=false",
        ],
    );
    let cache = context.temporary.join("buildx-cache");
    if cache_from {
        command.arguments.extend([
            "--cache-from".into(),
            format!("type=local,src={}", cache.display()).into(),
        ]);
    }
    if !context.cache_hit {
        command.arguments.extend([
            "--cache-to".into(),
            format!("type=local,dest={}-next,mode=max", cache.display()).into(),
        ]);
    }
    command.arguments.extend([
        "--build-arg".into(),
        format!("SOURCE_REVISION={}", context.revision).into(),
        "--label".into(),
        format!("org.opencontainers.image.revision={}", context.revision).into(),
        "--iidfile".into(),
        context.temporary.join("production.iid").into_os_string(),
        "--tag".into(),
        context.image.clone().into(),
        ".".into(),
    ]);
    command.timeout = Duration::from_secs(3000);
    command.grace = Duration::from_secs(120);
    command.capture = None;
    command
}

pub(super) const OWNER_LABEL: &str = "com.simplestchat.ci.production-verification";

pub(super) fn verify(image: &str, migrations: &[String], name: &str) -> Command {
    let mut command = Command::capture(
        Program::Docker,
        &[
            "container",
            "create",
            "--name",
            name,
            "--label",
            &format!("{OWNER_LABEL}={name}"),
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--entrypoint",
            "/bin/sh",
            image,
            "-eu",
            "-c",
            VERIFY,
            "verify-image",
        ],
    );
    command
        .arguments
        .extend(migrations.iter().map(OsString::from));
    command.timeout = Duration::from_secs(120);
    command
}
