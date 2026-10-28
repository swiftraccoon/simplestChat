//! Fixed fixture generations. No production path or caller is selected here.
use super::super::{
    CONFIG, Context, Error, PUBLIC, Result,
    files::{self, Original},
};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    path::Path,
    time::Duration,
};

fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(Error::Refused) }
}
fn directory(ctx: &mut Context, path: &Path) -> Result<()> {
    ctx.control.check()?;
    match fs::symlink_metadata(path) {
        Ok(meta) => require(
            meta.is_dir() && meta.uid() == 0 && meta.gid() == 0 && meta.mode() & 0o7777 == 0o700,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            super::super::create_directory(path, 0, 0o700, &mut ctx.control)
        }
        Err(_) => Err(Error::Io),
    }
}
fn copy_payload(ctx: &mut Context, name: &str, expected: (&str, u64)) -> Result<()> {
    let source = Path::new(super::super::PRIVATE)
        .join("server-input")
        .join(name);
    let input = ctx
        .originals
        .iter()
        .position(|v| v.path == source)
        .ok_or(Error::Input)?;
    ctx.originals[input].check(&mut ctx.control)?;
    require(ctx.originals[input].sha256 == expected.0 && ctx.originals[input].bytes == expected.1)?;
    let target = Path::new(PUBLIC)
        .join("releases")
        .join(&ctx.request.revision)
        .join(name);
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32)
        .open(&target)
        .map_err(|_| Error::Io)?;
    let result = (|| {
        let mut offset = 0u64;
        let mut block = [0u8; 65536];
        loop {
            ctx.control.check()?;
            ctx.originals[input].check_metadata(&mut ctx.control)?;
            let count = ctx.originals[input]
                .file()?
                .read_at(&mut block, offset)
                .map_err(|_| Error::Io)?;
            if count == 0 {
                break;
            }
            offset = offset.checked_add(count as u64).ok_or(Error::Bounds)?;
            require(offset <= expected.1)?;
            output.write_all(&block[..count]).map_err(|_| Error::Io)?;
            ctx.control.check()?;
        }
        require(offset == expected.1)?;
        output.sync_all().map_err(|_| Error::Io)?;
        ctx.originals[input].check(&mut ctx.control)
    })();
    if let Err(error) = result {
        ctx.control.record(error);
    }
    let closed = files::close(output, &mut ctx.control);
    result.and(closed)?;
    let original = Original::open(&target, expected.1, 0, 0o600, &mut ctx.control)?;
    ctx.originals.push(original);
    let observed = ctx.originals.last().ok_or(Error::Close)?;
    require(observed.sha256 == expected.0 && observed.bytes == expected.1)
}
pub(super) fn distribution(ctx: &mut Context) -> Result<()> {
    directory(ctx, &Path::new(PUBLIC).join("releases"))?;
    directory(
        ctx,
        &Path::new(PUBLIC)
            .join("releases")
            .join(&ctx.request.revision),
    )?;
    let manifest = (
        ctx.request.manifest.sha256.clone(),
        ctx.request.manifest.bytes,
    );
    let archive = (
        ctx.request.archive.sha256.clone(),
        ctx.request.archive.bytes,
    );
    copy_payload(ctx, "release.json", (&manifest.0, manifest.1))?;
    copy_payload(ctx, "image.tar", (&archive.0, archive.1))?;
    ctx.check()
}

pub(super) struct Selection {
    pub environment: Vec<u8>,
    pub compose: Vec<u8>,
    pub images: Value,
}
fn original_data(ctx: &mut Context, name: &str) -> Result<Vec<u8>> {
    let path = Path::new(CONFIG).join(name);
    let input = ctx
        .originals
        .iter()
        .position(|v| v.path == path)
        .ok_or(Error::Input)?;
    ctx.originals[input].data(1024 * 1024, &mut ctx.control)
}
pub(super) fn candidate(ctx: &mut Context) -> Result<Selection> {
    let environment =
        String::from_utf8(original_data(ctx, "app.env")?).map_err(|_| Error::Refused)?;
    require(
        environment
            .lines()
            .filter(|l| *l == "MAX_ROOMS=175")
            .count()
            == 1,
    )?;
    let selected = environment
        .lines()
        .map(|line| {
            if line == "MAX_ROOMS=175" {
                "MAX_ROOMS=176"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut original = files::write_new(
        &Path::new(CONFIG).join("app.env.candidate"),
        selected.as_bytes(),
        0,
        0o600,
        &mut ctx.control,
    )?;
    original.finish(&mut ctx.control)?;
    let compose = original_data(ctx, "compose.public.yml")?;
    let images = super::super::protocol::decode(&original_data(ctx, "images.json")?)?;
    Ok(Selection {
        environment: selected.into_bytes(),
        compose,
        images,
    })
}
pub(super) fn retire_selection(ctx: &mut Context) -> Result<()> {
    let paths = ["app.env", "compose.public.yml", "images.json"].map(|n| Path::new(CONFIG).join(n));
    for path in paths {
        let indices = ctx
            .originals
            .iter()
            .enumerate()
            .filter(|(_, v)| v.path == path)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        require(!indices.is_empty())?;
        for index in indices {
            let result = ctx.originals[index].finish(&mut ctx.control);
            if ctx.originals[index].closed() {
                ctx.host_retired.insert(index);
            }
            result?;
        }
    }
    ctx.check()
}
pub(super) fn selected(ctx: &mut Context, expected: &Selection) -> Result<()> {
    for (name, wanted) in [
        ("app.env", &expected.environment),
        ("compose.public.yml", &expected.compose),
    ] {
        let original = Original::open(
            &Path::new(CONFIG).join(name),
            1024 * 1024,
            0,
            0o600,
            &mut ctx.control,
        )?;
        ctx.originals.push(original);
        require(
            ctx.originals
                .last()
                .ok_or(Error::Close)?
                .data(1024 * 1024, &mut ctx.control)?
                == *wanted,
        )?;
    }
    let original = Original::open(
        &Path::new(CONFIG).join("images.json"),
        1024 * 1024,
        0,
        0o600,
        &mut ctx.control,
    )?;
    ctx.originals.push(original);
    require(
        super::super::protocol::decode(
            &ctx.originals
                .last()
                .ok_or(Error::Close)?
                .data(1024 * 1024, &mut ctx.control)?,
        )? == expected.images,
    )?;
    match fs::symlink_metadata(Path::new(CONFIG).join("app.env.candidate")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ctx.check(),
        _ => Err(Error::Refused),
    }
}

pub(super) async fn trust_install(ctx: &mut Context, root: &[u8]) -> Result<()> {
    let hosts = Path::new("/etc/hosts");
    let mut original = Original::open(hosts, 65536, 0, 0o644, &mut ctx.control)?;
    let before = original.file()?.metadata().map_err(|_| Error::Io)?;
    let prior = original.data(65536, &mut ctx.control)?;
    require(
        !prior
            .windows(b"maintain.localhost".len())
            .any(|v| v == b"maintain.localhost"),
    )?;
    original.finish(&mut ctx.control)?;
    let mut file = OpenOptions::new()
        .append(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC).bits() as i32)
        .open(hosts)
        .map_err(|_| Error::Io)?;
    let result = (|| {
        require(files::same(
            &before,
            &file.metadata().map_err(|_| Error::Io)?,
        ))?;
        file.write_all(b"\n127.0.0.1 maintain.localhost\n")
            .map_err(|_| Error::Io)?;
        file.sync_all().map_err(|_| Error::Io)?;
        ctx.control.check()
    })();
    if let Err(error) = result {
        ctx.control.record(error);
    }
    let closed = files::close(file, &mut ctx.control);
    result.and(closed)?;
    ctx.originals
        .push(Original::open(hosts, 65536, 0, 0o644, &mut ctx.control)?);
    let certificate = Path::new("/usr/local/share/ca-certificates/simplestchat-maintain.crt");
    ctx.originals.push(files::write_new(
        certificate,
        root,
        0,
        0o644,
        &mut ctx.control,
    )?);
    ctx.commands
        .admit_lookup("update-ca-certificates", &mut ctx.control)?;
    ctx.commands
        .call(
            &mut ctx.control,
            "host-trust-install",
            &["update-ca-certificates".into()],
            Duration::from_secs(30),
        )
        .await?;
    // Caddy's fixed private route returns404 before any backend exists. A
    // successful curl still proves natural CA verification and loopback DNS.
    let private = ctx
        .commands
        .call(
            &mut ctx.control,
            "host-trust-private",
            &[
                "curl".into(),
                "--disable".into(),
                "--noproxy".into(),
                "*".into(),
                "--proto".into(),
                "=https".into(),
                "--silent".into(),
                "--show-error".into(),
                "--max-time".into(),
                "2".into(),
                "--output".into(),
                "/dev/null".into(),
                "--write-out".into(),
                "%{http_code}".into(),
                "https://maintain.localhost/metrics".into(),
            ],
            Duration::from_secs(5),
        )
        .await?;
    require(private == b"404")?;
    ctx.check()
}
pub(super) async fn trust_ready(ctx: &mut Context) -> Result<()> {
    let ready = ctx
        .commands
        .call(
            &mut ctx.control,
            "host-trust-ready",
            &[
                "curl".into(),
                "--disable".into(),
                "--noproxy".into(),
                "*".into(),
                "--proto".into(),
                "=https".into(),
                "--fail".into(),
                "--silent".into(),
                "--show-error".into(),
                "--max-time".into(),
                "2".into(),
                "https://maintain.localhost/ready".into(),
            ],
            Duration::from_secs(5),
        )
        .await?;
    require(super::super::protocol::decode(&ready)?["status"] == "ready")?;
    ctx.check()
}

pub(super) fn attempt(report: &Value) -> Result<std::path::PathBuf> {
    let path = Path::new(report["evidence"].as_str().ok_or(Error::Refused)?);
    let suffix = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("release."))
        .ok_or(Error::Refused)?;
    require(
        path.parent() == Some(Path::new(PUBLIC).join("results").as_path())
            && super::super::protocol::digest(suffix, 32),
    )?;
    Ok(path.into())
}
pub(super) fn read(ctx: &mut Context, path: &Path, maximum: u64) -> Result<Vec<u8>> {
    ctx.originals
        .push(Original::open(path, maximum, 0, 0o600, &mut ctx.control)?);
    ctx.originals
        .last()
        .ok_or(Error::Close)?
        .data(maximum, &mut ctx.control)
}
