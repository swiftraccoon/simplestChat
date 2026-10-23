//! Original source bindings are checked before every actual reference execution.
use super::{Result, fail, files};
use std::{fs::File, io::Read, path::Path};

pub(super) fn check() -> Result<()> {
    for (name, bytes, length, expected) in [
        (
            "reference/build/release_container_fixture.py",
            include_bytes!("reference/build/release_container_fixture.py").as_slice(),
            10153,
            "072ef7e7851dc2d2232694d3f7f7224876391a4a8052977adce04309b0de24e8",
        ),
        (
            "reference/build/release_container_harness.py",
            include_bytes!("reference/build/release_container_harness.py").as_slice(),
            62342,
            "4db1d1165fb05c09cbf677d7d4412611c539ce09aa89b17f0ec881c697814e20",
        ),
        (
            "reference/build/release_http_policy.py",
            include_bytes!("reference/build/release_http_policy.py").as_slice(),
            13723,
            "36bd22927f0d72d8f4c89fb6274a7e13fcd4b700e423a17d5f32e5c130a99a5c",
        ),
        (
            "reference/ops/ansible/files/bounded_process.py",
            include_bytes!("reference/ops/ansible/files/bounded_process.py").as_slice(),
            9296,
            "0b6005c7bc6ee101120fd129bf563c1ce4d8cbb3d9cda0e2d8fdae552f9d25e7",
        ),
        (
            "reference/ops/ansible/files/release_artifact.py",
            include_bytes!("reference/ops/ansible/files/release_artifact.py").as_slice(),
            15101,
            "d6219ad2f6db7c45a8b4e5728437e1c6ef8aff58d3936c059fd52eea7aef1511",
        ),
        (
            "reference/ops/ansible/files/release_assets.py",
            include_bytes!("reference/ops/ansible/files/release_assets.py").as_slice(),
            18557,
            "314c91956f13e72a867ac624c8027bf9c5fff740e953255dd5fbf2d87b6e72f2",
        ),
        (
            "reference/ops/ansible/files/release_json.py",
            include_bytes!("reference/ops/ansible/files/release_json.py").as_slice(),
            3622,
            "0012aa8c99115d409a9955c11b675fab6683dfa91b5c633f75bde5ee67df7e97",
        ),
        (
            "reference/ops/ansible/files/release_public.py",
            include_bytes!("reference/ops/ansible/files/release_public.py").as_slice(),
            76115,
            "4d9a712aabee6432c98b2ebdd2b5a40c989deef5e0e16aa2c7c1dbd7284cb420",
        ),
        (
            "reference/ops/ansible/files/runtime_profile.py",
            include_bytes!("reference/ops/ansible/files/runtime_profile.py").as_slice(),
            7136,
            "22f9b098a6b19be111291c467e2a34a5c7c0f53a6aa3691f71390ddf8bd85965",
        ),
        (
            "reference/ops/ansible/tests/test_release_container_harness.py",
            include_bytes!(concat!(
                "reference/ops/ansible/tests/",
                "test_release_container_harness.py"
            ))
            .as_slice(),
            80195,
            "1adf14b6bfc31e1826ceafdc3f0bf864a75ee7752d5876dcdc12c5dcf3a45b23",
        ),
        (
            "reference/ops/ansible/tests/test_release_http_policy.py",
            include_bytes!("reference/ops/ansible/tests/test_release_http_policy.py").as_slice(),
            19984,
            "b07c4b14340625de326ca5db54ab4112a17847a1d33bf8d804386e80ee6808ab",
        ),
        (
            "reference/ops/ansible/tests/test_support.py",
            include_bytes!("reference/ops/ansible/tests/test_support.py").as_slice(),
            2574,
            "ae56c5e0254594f61c0ff242c5bb55d642f36d3c9b9bb4ddf366a00d56e10ce7",
        ),
        (
            "reference/security/http-policy.json",
            include_bytes!("reference/security/http-policy.json").as_slice(),
            1040,
            "31ae348a622e48ee9e06ccad100ebe58b05243782d5a45b841b4815685f3bd21",
        ),
    ] {
        if bytes.len() != length || crate::ci::io::sha256(bytes) != expected {
            return Err(fail(1, "oracle_source"));
        }
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/ci/release_http_policy")
            .join(name);
        let mut source = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| fail(1, "oracle_source"))?;
        let checked = (|| {
            let before = source.metadata().map_err(|_| fail(1, "oracle_source"))?;
            if !before.is_file() || before.len() != length as u64 {
                return Err(fail(1, "oracle_source"));
            }
            let mut observed = Vec::new();
            Read::by_ref(&mut source)
                .take(length as u64 + 1)
                .read_to_end(&mut observed)
                .map_err(|_| fail(1, "oracle_source"))?;
            if observed != bytes
                || !files::same(
                    &before,
                    &source.metadata().map_err(|_| fail(1, "oracle_source"))?,
                )
            {
                return Err(fail(1, "oracle_source"));
            }
            Ok(())
        })();
        let closed = files::close(source);
        checked.and(closed)?;
    }
    Ok(())
}

#[test]
fn original_local_sources_are_exact_pre_port_bytes() {
    check().unwrap();
}
