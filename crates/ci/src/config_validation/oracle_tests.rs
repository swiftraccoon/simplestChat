//! Execute the retained shell with an owned fake Docker, never a daemon/image.

use super::*;
use sha2::{Digest, Sha256};
use std::process::Command as Process;
#[path = "reference_tests.rs"]
mod reference;

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn original_shell_argv_status_and_fixed_environment_match_both_typed_create_plans() {
    assert_eq!(
        reference::SOURCE_COMMIT,
        "166af83deaa7c073e134e19fc30ae365ce0c4af3"
    );
    assert_eq!(reference::WORKFLOW_SHA256.len(), 64);
    for (kind, shell, expected_hash) in [
        (
            Kind::Promtool,
            reference::PROMTOOL,
            reference::PROMTOOL_SHA256,
        ),
        (Kind::Caddy, reference::CADDY, reference::CADDY_SHA256),
    ] {
        assert_eq!(digest(shell.as_bytes()), expected_hash);
        for status in [0, 1, 42, 125] {
            let fixture = Fixture::new(kind);
            let tool = fixture.root.join("docker");
            fs::write(
                &tool,
                concat!(
                    "#!/usr/bin/python3\nimport os,sys,json\nfrom pathlib import Path\n",
                    "root=Path(os.environ['FIXTURE_ROOT'])\n",
                    "(root/'old-command.json').write_text(json.dumps({'argv':sys.argv[1:],",
                    "'secret':os.environ.get('TRUSTED_PROXY_SECRET'),'cwd':str(Path.cwd())}))\n",
                    "sys.exit(int(os.environ['FAKE_STATUS']))\n"
                ),
            )
            .unwrap();
            fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
            let result = Process::new("/bin/bash")
                .args(["-euo", "pipefail", "-c", shell])
                .current_dir(&fixture.root)
                .env_clear()
                .envs(&fixture.context.environment)
                .env("PATH", &fixture.root)
                .env("FIXTURE_ROOT", &fixture.root)
                .env("FAKE_STATUS", status.to_string())
                .output()
                .unwrap();
            assert_eq!(result.status.code(), Some(status));
            let recorded: Value =
                serde_json::from_slice(&fs::read(fixture.root.join("old-command.json")).unwrap())
                    .unwrap();
            assert_eq!(recorded["cwd"], fixture.root.to_str().unwrap());
            assert_eq!(recorded["secret"], plan::PROXY_SECRET);
            let old = recorded["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(&old[..2], ["run", "--rm"]);
            let new = Fake::strings(&fixture.context.create(
                ENDPOINT,
                "owned-name",
                "owned-token",
                IMAGE,
            ));
            assert_eq!(&new[..4], ["--host", ENDPOINT, "container", "create"]);
            let remaining = new[8..]
                .iter()
                .map(|arg| {
                    if arg == IMAGE {
                        kind.image().into()
                    } else {
                        arg.clone()
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(&old[2..], remaining);
        }
    }
}
