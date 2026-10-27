//! Fixed source roles imported from one exact shallow bundle; no interpreted helpers.

use super::{BUNDLE, Context, Error, INSTALLED, LIBRARY, ROOT, Result, files};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    time::Duration,
};

pub(super) const HELPERS: [&str; 15] = [
    "bounded_process.py",
    "release-public.py",
    "release_public.py",
    "release_artifact.py",
    "release_assets.py",
    "browser_assets.py",
    "release_json.py",
    "runtime_profile.py",
    "reboot-public.py",
    "reboot_public.py",
    "fetch-release.py",
    "release_fetch_receiver.py",
    "release_trust.py",
    "force_release.py",
    "security_vm_ops_guest.py",
];
pub(super) const MIGRATIONS: [&str; 36] = [
    "001_create_users.sql",
    "002_create_sessions.sql",
    "003_create_rooms.sql",
    "004_create_room_roles.sql",
    "005_create_room_states.sql",
    "006_add_session_token_index.sql",
    "007_add_webauthn_credential_id.sql",
    "008_canonicalize_user_emails.sql",
    "009_add_room_query_indexes.sql",
    "010_add_refresh_token_families.sql",
    "011_add_room_search_trigram_indexes.sql",
    "012_canonicalize_guest_ipv6_moderation_ips.sql",
    "013_community_accounts.sql",
    "014_create_room_reports.sql",
    "015_add_user_chat_style.sql",
    "016_constraints_and_indexes.sql",
    "017_moderation_history.sql",
    "018_user_chat_preferences.sql",
    "019_invites.sql",
    "020_invite_receipts_and_retention.sql",
    "021_invitation_secrets.sql",
    "022_require_current_sessions.sql",
    "023_independent_appearance.sql",
    "024_validate_independent_appearance.sql",
    "025_chat_history.sql",
    "026_validate_chat_moderation.sql",
    "027_chat_updates.sql",
    "028_chat_lookup_indexes.sql",
    "029_chat_reply_index.sql",
    "030_push_subscriptions.sql",
    "031_discovery.sql",
    "032_notification_preferences.sql",
    "033_attachments.sql",
    "034_room_image_version.sql",
    "035_role_numbering.sql",
    "036_validate_role_numbering.sql",
];
pub(super) const DIRECT_SOURCES: [&str; 8] = [
    "Caddyfile",
    "docker-compose.yml",
    "ops/ansible/files/deploy-public.sh",
    "ops/ansible/files/public-compose.sh",
    "ops/ansible/files/seed-public-room.py",
    "ops/ansible/files/seed_public_room.py",
    "ops/ansible/files/release_json.py",
    "ops/ansible/files/runtime_profile.py",
];
pub(super) const TEMPLATES: [&str; 8] = [
    "app.env",
    "migration.env",
    "proxy.env",
    "compose.yml",
    "pg_hba.conf",
    "init-database.sql",
    "runtime-grants.sql",
    "postgres-admin-password",
];

pub(super) fn guest_names() -> Vec<String> {
    let mut names = BTreeSet::new();
    names.extend(DIRECT_SOURCES.iter().map(|s| (*s).to_owned()));
    names.extend(
        TEMPLATES
            .iter()
            .map(|s| format!("ops/ansible/templates/public-{s}.j2")),
    );
    names.extend(MIGRATIONS.iter().map(|s| format!("migrations/{s}")));
    names.into_iter().collect()
}
pub(super) fn helper_names() -> Vec<String> {
    let mut names = HELPERS.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    names.sort();
    names
}
async fn git(ctx: &mut Context, role: &str, args: &[&str]) -> Result<Vec<u8>> {
    ctx.check()?;
    let mut command = vec!["git".into()];
    command.extend(args.iter().map(|v| (*v).to_owned()));
    let result = ctx
        .commands
        .call(&mut ctx.control, role, &command, Duration::from_secs(30))
        .await?;
    ctx.check()?;
    Ok(result)
}
pub(super) async fn prepare(ctx: &mut Context) -> Result<()> {
    ctx.control.checkpoint = "source-import";
    if !rustix::process::geteuid().is_root()
        || rustix::process::getegid().as_raw() != 0
        || rustix::system::uname().sysname() != c"Linux"
    {
        return Err(Error::Refused);
    }
    let marker = ctx.retain(Path::new("/etc/simplestchat-vm-fixture"), 128, 0, 0o600)?;
    if ctx.originals[marker].data(128, &mut ctx.control)?
        != format!("{}\n", ctx.request.run_id).as_bytes()
    {
        return Err(Error::Changed);
    }
    let bundle = ctx.retain(Path::new(BUNDLE), 320 * 1024 * 1024, 0, 0o600)?;
    if ctx.originals[bundle].sha256 != ctx.request.source_bundle.sha256
        || ctx.originals[bundle].bytes != ctx.request.source_bundle.bytes
    {
        return Err(Error::Changed);
    }
    for name in ["docker", "git", "nft", "tcpdump", "ip", "curl"] {
        ctx.commands.admit_lookup(name, &mut ctx.control)?;
    }
    if !ctx
        .commands
        .call(
            &mut ctx.control,
            "empty-daemon",
            &[
                "docker".into(),
                "--host".into(),
                "unix:///var/run/docker.sock".into(),
                "ps".into(),
                "--all".into(),
                "--quiet".into(),
            ],
            Duration::from_secs(15),
        )
        .await?
        .is_empty()
    {
        return Err(Error::Refused);
    }
    if !ctx
        .commands
        .call(
            &mut ctx.control,
            "empty-project-networks",
            &[
                "docker".into(),
                "--host".into(),
                "unix:///var/run/docker.sock".into(),
                "network".into(),
                "ls".into(),
                "--quiet".into(),
                "--filter".into(),
                "label=com.docker.compose.project=simplestchat-public".into(),
            ],
            Duration::from_secs(15),
        )
        .await?
        .is_empty()
    {
        return Err(Error::Refused);
    }
    ctx.control.check()?;
    fs::create_dir(&ctx.source_root).map_err(|_| Error::Io)?;
    fs::set_permissions(&ctx.source_root, fs::Permissions::from_mode(0o700))
        .map_err(|_| Error::Io)?;
    let root = ctx.source_root.to_str().ok_or(Error::Input)?.to_owned();
    let revision = ctx.request.revision.clone();
    git(ctx, "source-init", &["init", "--quiet", &root]).await?;
    let shallow = files::write_new(
        &ctx.source_root.join(".git/shallow"),
        format!("{revision}\n").as_bytes(),
        0,
        0o600,
        &mut ctx.control,
    )?;
    ctx.originals.push(shallow);
    git(
        ctx,
        "source-fetch",
        &[
            "-C",
            &root,
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            BUNDLE,
            "HEAD:refs/remotes/source/main",
        ],
    )
    .await?;
    git(
        ctx,
        "source-checkout",
        &["-C", &root, "checkout", "--quiet", "--detach", &revision],
    )
    .await?;
    if git(ctx, "source-head", &["-C", &root, "rev-parse", "HEAD"]).await?
        != format!("{revision}\n").as_bytes()
    {
        return Err(Error::Changed);
    }
    if git(
        ctx,
        "source-tree",
        &["-C", &root, "rev-parse", "HEAD^{tree}"],
    )
    .await?
        != format!("{}\n", ctx.request.source_tree).as_bytes()
    {
        return Err(Error::Changed);
    }
    if git(
        ctx,
        "source-count",
        &["-C", &root, "rev-list", "--count", "HEAD"],
    )
    .await?
        != b"1\n"
    {
        return Err(Error::Changed);
    }
    git(
        ctx,
        "source-fsck",
        &["-C", &root, "fsck", "--connectivity-only"],
    )
    .await?;
    if !git(
        ctx,
        "source-clean",
        &[
            "-C",
            &root,
            "status",
            "--porcelain",
            "--untracked-files=all",
        ],
    )
    .await?
    .is_empty()
    {
        return Err(Error::Changed);
    }
    for name in guest_names() {
        let path = ctx.source_root.join(&name);
        let mode = if name == "ops/ansible/files/seed-public-room.py" {
            0o700
        } else {
            0o600
        };
        let index = ctx.retain(&path, 1024 * 1024, 0, mode)?;
        if ctx.originals[index].sha256 != ctx.request.source_pins[&name] {
            return Err(Error::Changed);
        }
        let bytes = ctx.originals[index].data(1024 * 1024, &mut ctx.control)?;
        ctx.sources.insert(name, bytes);
    }
    for name in helper_names() {
        let path = if name == "security_vm_ops_guest.py" {
            Path::new(ROOT).join(&name)
        } else {
            Path::new(LIBRARY).join(&name)
        };
        let mode = if name == "security_vm_ops_guest.py" {
            0o600
        } else {
            0o644
        };
        let index = ctx.retain(&path, 1024 * 1024, 0, mode)?;
        if ctx.originals[index].sha256 != ctx.request.helper_pins[&name] {
            return Err(Error::Changed);
        }
    }
    let installed = ctx.retain(Path::new(INSTALLED), 256 * 1024 * 1024, 0, 0o755)?;
    if ctx.originals[installed].sha256 != ctx.request.binary.sha256
        || ctx.originals[installed].bytes != ctx.request.binary.bytes
    {
        return Err(Error::Changed);
    }
    ctx.commands
        .admit("ops", Path::new(INSTALLED), &mut ctx.control)?;
    ctx.admit_socket()?;
    ctx.check()
}
pub(super) async fn dirty(ctx: &mut Context) -> Result<()> {
    let path = ctx.source_root.join("README.md");
    ctx.control.check()?;
    let before = fs::symlink_metadata(&path).map_err(|_| Error::Io)?;
    if !before.is_file()
        || before.uid() != 0
        || before.gid() != 0
        || before.nlink() != 1
        || before.mode() & 0o7777 != 0o600
    {
        return Err(Error::Refused);
    }
    let mut file = OpenOptions::new()
        .append(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK)
                .bits() as i32,
        )
        .open(&path)
        .map_err(|_| Error::Io)?;
    let result = (|| {
        let held = file.metadata().map_err(|_| Error::Io)?;
        if !files::same(&held, &before) {
            return Err(Error::Changed);
        }
        ctx.control.check()?;
        file.write_all(b"\nOwned prerequisite dirty-source fixture.\n")
            .map_err(|_| Error::Io)?;
        file.sync_all().map_err(|_| Error::Io)?;
        ctx.control.check()
    })();
    if let Err(error) = result {
        ctx.control.record(error);
    }
    let closed = files::close(file, &mut ctx.control);
    result.and(closed)?;
    let root = ctx.source_root.to_str().ok_or(Error::Input)?.to_owned();
    if git(
        ctx,
        "source-deliberately-dirty",
        &[
            "-C",
            &root,
            "status",
            "--porcelain",
            "--untracked-files=all",
        ],
    )
    .await?
        != b" M README.md\n"
    {
        return Err(Error::Changed);
    }
    ctx.observed.set("dirtyReadmeObserved", true)
}
pub(super) fn socket_check(ctx: &mut Context) -> Result<()> {
    if let Some((path, before)) = &ctx.socket {
        let visible = fs::symlink_metadata(path).map_err(|_| Error::Changed)?;
        if fs::canonicalize("/var/run/docker.sock").map_err(|_| Error::Changed)? != *path
            || (
                visible.dev(),
                visible.ino(),
                visible.mode(),
                visible.uid(),
                visible.gid(),
            ) != (
                before.dev(),
                before.ino(),
                before.mode(),
                before.uid(),
                before.gid(),
            )
        {
            return Err(Error::Changed);
        }
    }
    ctx.control.check()
}
