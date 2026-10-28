//! Exact legacy proxy setup and independent modern-generation observations.
//! Call after natural CA preparation and before the positive cold launcher.
//! Historical bytes are a pinned fixture, never a new input trust authority.

use super::super::{Context, Error, Result, files, files::Original, protocol};
use super::{Fixture, policy};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    os::unix::fs::MetadataExt,
    path::Path,
};

const CONFIG: &str = "/etc/simplestchat-public";
const LEGACY: &[u8] = include_bytes!("proxy-legacy.Caddyfile");
// Repository commit 51e70de6a95a92ecc0c7ebc833619a7efc8b2f2d:Caddyfile.
const LEGACY_SHA: &str = "ea24dcb24b1285b4529e3aa70083550d25431c56cc3a762b8891a53f1306f3ab";
const ASSET_MOUNT: &str = concat!(
    "      - \"/srv/simplestchat-public/browser-assets:",
    "/run/simplestchat-browser-assets:ro\"\n"
);
fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(Error::Refused) }
}
fn scenario(ctx: &Context) -> Result<()> {
    require(ctx.request.is_host() && ctx.request.scenario == "host-proxy-upgrade")
}
fn legacy_compose(modern: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(modern).map_err(|_| Error::Refused)?;
    require(text.matches("\n  caddy:\n").count() == 1)?;
    require(text.matches(ASSET_MOUNT).count() == 1)?;
    require(text.matches("\n  migrate:\n").count() == 1)?;
    let caddy = text.split_once("\n  caddy:\n").ok_or(Error::Refused)?.1;
    let caddy = caddy.split_once("\n  migrate:\n").ok_or(Error::Refused)?.0;
    require(caddy.contains(ASSET_MOUNT))?;
    Ok(text.replacen(ASSET_MOUNT, "", 1).into_bytes())
}
fn data(ctx: &mut Context, name: &str) -> Result<Vec<u8>> {
    let path = Path::new(CONFIG).join(name);
    let at = ctx
        .originals
        .iter()
        .position(|o| o.path == path && !o.closed())
        .ok_or(Error::Refused)?;
    ctx.originals[at].data(1024 * 1024, &mut ctx.control)
}
fn config_sync(ctx: &mut Context) -> Result<()> {
    ctx.control.check()?;
    let before = fs::symlink_metadata(CONFIG).map_err(|_| Error::Io)?;
    require(
        before.is_dir()
            && before.uid() == 0
            && before.gid() == 0
            && before.mode() & 0o7777 == 0o700,
    )?;
    let file = File::from(
        rustix::fs::open(
            CONFIG,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| Error::Io)?,
    );
    let result = (|| {
        require(files::same(
            &before,
            &file.metadata().map_err(|_| Error::Io)?,
        ))?;
        file.sync_all().map_err(|_| Error::Io)?;
        require(files::same(
            &before,
            &fs::symlink_metadata(CONFIG).map_err(|_| Error::Io)?,
        ))?;
        ctx.control.check()
    })();
    if let Err(error) = result {
        ctx.control.record(error);
    }
    let closed = files::close(file, &mut ctx.control);
    result.and(closed)
}
fn publish(ctx: &mut Context, name: &str, bytes: &[u8], mode: u32) -> Result<()> {
    require(matches!(
        (name, mode),
        ("Caddyfile", 0o644)
            | ("Caddyfile.candidate", 0o600)
            | ("compose.public.yml", 0o600)
            | ("compose.public.yml.candidate", 0o600)
    ))?;
    let original = files::write_new(
        &Path::new(CONFIG).join(name),
        bytes,
        0,
        mode,
        &mut ctx.control,
    )?;
    ctx.originals.push(original);
    ctx.originals
        .last()
        .ok_or(Error::Close)?
        .require_parent(0, 0o700, &mut ctx.control)?;
    config_sync(ctx)
}
fn remove_generation(ctx: &mut Context, name: &str, expected: &[u8], mode: u32) -> Result<()> {
    require(matches!(
        (name, mode),
        ("Caddyfile", 0o644)
            | ("compose.public.yml", 0o600)
            | ("browser-assets-installed.json", 0o600)
    ))?;
    let path = Path::new(CONFIG).join(name);
    let ids = ctx
        .originals
        .iter()
        .enumerate()
        .filter(|(_, o)| o.path == path && !o.closed())
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    require(!ids.is_empty())?;
    // Admit the deletion owner while every old witness is still open, then
    // explicitly join it to each original. Equal bytes alone are insufficient.
    let mut deletion = Original::open(&path, expected.len() as u64, 0, mode, &mut ctx.control)?;
    let ready = (|| {
        deletion.require_parent(0, 0o700, &mut ctx.control)?;
        require(deletion.data(expected.len() as u64, &mut ctx.control)? == expected)?;
        for id in &ids {
            require(files::same(
                &ctx.originals[*id]
                    .file()?
                    .metadata()
                    .map_err(|_| Error::Io)?,
                &deletion.file()?.metadata().map_err(|_| Error::Io)?,
            ))?;
            require(ctx.originals[*id].data(expected.len() as u64, &mut ctx.control)? == expected)?;
        }
        for id in ids {
            let result = ctx.originals[id].finish(&mut ctx.control);
            if ctx.originals[id].closed() {
                ctx.host_retired.insert(id);
            }
            result?;
        }
        ctx.control.check()
    })();
    if let Err(error) = ready {
        ctx.control.record(error);
        let _ = deletion.finish(&mut ctx.control);
        return Err(error);
    }
    deletion.retire_exact(expected, &mut ctx.control)?;
    config_sync(ctx)
}

pub(in super::super) struct ProxyCandidate {
    pub(in super::super) environment: Vec<u8>,
    pub(in super::super) compose: Vec<u8>,
    pub(in super::super) images: serde_json::Value,
    caddy: Vec<u8>,
    images_original: Vec<u8>,
}
/// Save modern candidates, install real legacy inputs, and retain originals.
/// This runs before the cold launcher, so no live proxy has mismatched inputs.
fn prepare(ctx: &mut Context) -> Result<ProxyCandidate> {
    scenario(ctx)?;
    ctx.check()?;
    require(LEGACY.len() == 2324 && files::hex(&Sha256::digest(LEGACY)) == LEGACY_SHA)?;
    let environment = data(ctx, "app.env")?;
    let compose = data(ctx, "compose.public.yml")?;
    let caddy = data(ctx, "Caddyfile")?;
    require(caddy == *ctx.sources.get("Caddyfile").ok_or(Error::Refused)?)?;
    let installed = data(ctx, "browser-assets-installed.json")?;
    let receipt = protocol::decode(&installed)?;
    protocol::exact(&receipt, &["schemaVersion", "caddyfileSha256"])?;
    require(
        receipt["schemaVersion"] == 1
            && receipt["caddyfileSha256"] == files::hex(&Sha256::digest(&caddy)),
    )?;
    let images_original = data(ctx, "images.json")?;
    let images = protocol::decode(&images_original)?;
    let legacy = legacy_compose(&compose)?;
    publish(ctx, "Caddyfile.candidate", &caddy, 0o600)?;
    publish(ctx, "compose.public.yml.candidate", &compose, 0o600)?;
    remove_generation(ctx, "Caddyfile", &caddy, 0o644)?;
    publish(ctx, "Caddyfile", LEGACY, 0o644)?;
    remove_generation(ctx, "compose.public.yml", &compose, 0o600)?;
    publish(ctx, "compose.public.yml", &legacy, 0o600)?;
    remove_generation(ctx, "browser-assets-installed.json", &installed, 0o600)?;
    absent_receipt(ctx)?;
    ctx.check()?;
    Ok(ProxyCandidate {
        environment,
        compose,
        images,
        caddy,
        images_original,
    })
}
fn absent_receipt(ctx: &mut Context) -> Result<()> {
    scenario(ctx)?;
    match fs::symlink_metadata(Path::new(CONFIG).join("browser-assets-installed.json")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ctx.control.check(),
        Err(_) => Err(Error::Io),
        Ok(_) => Err(Error::Changed),
    }
}
/// Retire only active observers of all four generations that the host body
/// will replace. Prior setup retirees remain closed, indexed and explicitly
/// acknowledged; this scenario does not invoke standard retire_selection,
/// whose original contract assumes that every selected path is still active.
fn retire_inputs(ctx: &mut Context, candidate: &ProxyCandidate) -> Result<()> {
    scenario(ctx)?;
    let legacy = legacy_compose(&candidate.compose)?;
    for (name, expected) in [
        ("Caddyfile", LEGACY),
        ("compose.public.yml", legacy.as_slice()),
        ("app.env", candidate.environment.as_slice()),
        ("images.json", candidate.images_original.as_slice()),
    ] {
        let path = Path::new(CONFIG).join(name);
        let mut active = Vec::new();
        for (index, original) in ctx.originals.iter().enumerate() {
            if original.path != path {
                continue;
            }
            if original.closed() {
                require(ctx.host_retired.contains(&index))?;
            } else {
                require(!ctx.host_retired.contains(&index))?;
                active.push(index);
            }
        }
        require(!active.is_empty())?;
        for index in active {
            require(ctx.originals[index].data(1024 * 1024, &mut ctx.control)? == expected)?;
            let result = ctx.originals[index].finish(&mut ctx.control);
            if ctx.originals[index].closed() {
                ctx.host_retired.insert(index);
            }
            result?;
        }
    }
    absent_receipt(ctx)?;
    ctx.check()
}
fn installed(ctx: &mut Context, candidate: &ProxyCandidate) -> Result<()> {
    scenario(ctx)?;
    let current = Original::open(
        &Path::new(CONFIG).join("Caddyfile"),
        1024 * 1024,
        0,
        0o644,
        &mut ctx.control,
    )?;
    ctx.originals.push(current);
    ctx.originals
        .last()
        .ok_or(Error::Close)?
        .require_parent(0, 0o700, &mut ctx.control)?;
    require(
        ctx.originals
            .last()
            .ok_or(Error::Close)?
            .data(1024 * 1024, &mut ctx.control)?
            == candidate.caddy,
    )?;
    let receipt = Original::open(
        &Path::new(CONFIG).join("browser-assets-installed.json"),
        512,
        0,
        0o600,
        &mut ctx.control,
    )?;
    ctx.originals.push(receipt);
    ctx.originals
        .last()
        .ok_or(Error::Close)?
        .require_parent(0, 0o700, &mut ctx.control)?;
    let receipt = protocol::decode(
        &ctx.originals
            .last()
            .ok_or(Error::Close)?
            .data(512, &mut ctx.control)?,
    )?;
    protocol::exact(&receipt, &["schemaVersion", "caddyfileSha256"])?;
    require(
        receipt["schemaVersion"] == 1
            && receipt["caddyfileSha256"] == files::hex(&Sha256::digest(&candidate.caddy)),
    )?;
    ctx.check()
}

impl Fixture {
    /// Prepare legacy inputs after the temporary CA proxy has been removed,
    /// while the only remaining actor is the independently admitted database.
    pub(in super::super) fn proxy_prepare(&mut self, ctx: &mut Context) -> Result<ProxyCandidate> {
        scenario(ctx)?;
        let database = self.pg.as_ref().ok_or(Error::Refused)?;
        require(
            !self.host_proxy_legacy
                && self.app.is_none()
                && self.proxy.is_none()
                && self.materials.is_some()
                && self.actors == std::collections::BTreeSet::from([database.clone()]),
        )?;
        let candidate = prepare(ctx)?;
        self.host_proxy_legacy = true;
        Ok(candidate)
    }

    /// Check the actual cold-launcher proxy row before opening the host epoch.
    /// No fourth mount is synthesized to reuse the modern profile.
    pub(in super::super) fn proxy_baseline(
        &self,
        ctx: &mut Context,
        baseline: &super::super::host::Baseline,
    ) -> Result<()> {
        scenario(ctx)?;
        let labels = &baseline.proxy_row["Config"]["Labels"];
        require(
            self.host_proxy_legacy
                && self.proxy.as_deref() == Some(baseline.proxy.as_str())
                && baseline.caddy_image == self.caddy_image
                && baseline.proxy_row["State"]["Running"] == true
                && labels[policy::LABEL] == ctx.request.run_id
                && labels["com.docker.compose.project"] == policy::PROJECT
                && labels["com.docker.compose.service"] == "caddy"
                && labels["com.docker.compose.container-number"] == "1",
        )?;
        policy::legacy_proxy_profile(
            &baseline.proxy_row,
            &baseline.proxy,
            &self.caddy_image,
            &ctx.request.caddy_image,
            &self.bridge,
        )?;
        absent_receipt(ctx)
    }

    /// Settle only this scenario's active observers before the host body
    /// replaces them. Existing setup retirees keep their original indices.
    pub(in super::super) fn proxy_selected_retire(
        &mut self,
        ctx: &mut Context,
        candidate: &ProxyCandidate,
    ) -> Result<()> {
        scenario(ctx)?;
        require(self.host_proxy_legacy && self.app.is_some() && self.proxy.is_some())?;
        retire_inputs(ctx, candidate)?;
        self.host_proxy_legacy = false;
        Ok(())
    }

    /// Join actual final proxy identity and mounts with both published modern
    /// files and the private historical Caddyfile saved before interruption.
    pub(in super::super) fn proxy_verify(
        &self,
        ctx: &mut Context,
        candidate: &ProxyCandidate,
        baseline: &super::super::host::Baseline,
        proxy: &serde_json::Value,
        attempt: &Path,
    ) -> Result<()> {
        scenario(ctx)?;
        let id = policy::string(&proxy["Id"])?;
        require(
            !self.host_proxy_legacy
                && policy::digest(id)
                && id != baseline.proxy
                && proxy["Image"] == baseline.caddy_image
                && proxy["State"]["Running"] == true,
        )?;
        policy::proxy_profile(
            proxy,
            id,
            &self.caddy_image,
            &ctx.request.caddy_image,
            &self.bridge,
        )?;
        installed(ctx, candidate)?;
        let before = Original::open(
            &attempt.join("before-Caddyfile"),
            LEGACY.len() as u64,
            0,
            0o600,
            &mut ctx.control,
        )?;
        ctx.originals.push(before);
        let before = ctx.originals.last().ok_or(Error::Close)?;
        before.require_parent(0, 0o700, &mut ctx.control)?;
        require(before.data(LEGACY.len() as u64, &mut ctx.control)? == LEGACY)?;
        ctx.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_derivation_removes_only_the_single_caddy_mount() {
        let current = format!("services:\n  caddy:\n    volumes:\n{ASSET_MOUNT}\n  migrate:\n");
        assert_eq!(
            legacy_compose(current.as_bytes()).unwrap(),
            b"services:\n  caddy:\n    volumes:\n\n  migrate:\n"
        );
        for invalid in [
            current.replace(ASSET_MOUNT, ""),
            current.replace(ASSET_MOUNT, &format!("{ASSET_MOUNT}{ASSET_MOUNT}")),
            format!("services:\n  caddy:\n  migrate:\n{ASSET_MOUNT}"),
            format!("services:\n  migrate:\n{ASSET_MOUNT}\n  caddy:\n"),
        ] {
            assert!(legacy_compose(invalid.as_bytes()).is_err());
        }
    }
}
