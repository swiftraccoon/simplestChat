//! All mutation and rejection cases from the unchanged Python projection method.
use super::{BUILD, source};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
#[test]
fn current_workflow_projection_preserves_all_six_changes_and_six_refusals() {
    let path = std::env::temp_dir().join(format!(
        "ci-workflow-projection-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    let file = path.join("ci.yml");
    let original = source();
    fs::write(&file, &original).unwrap();
    let project = || crate::ci::fingerprint::backend_workflow(&file);
    let before = project().unwrap();
    for (from, to, changed) in [
        (
            "name: Build production container",
            "name: Build the production image",
            false,
        ),
        ("test:stress", "test:stress-extra", false),
        (BUILD, "cargo build --locked --bins", true),
        ("OPENSSL_STATIC: \"1\"", "OPENSSL_STATIC: \"0\"", true),
        ("target/openssl-4.1.0", "target/openssl-4.1.1", true),
        (
            "cache-key: browser-backend-v1",
            "cache-key: different-build",
            true,
        ),
    ] {
        assert!(original.contains(from), "{from}");
        fs::write(&file, original.replace(from, to)).unwrap();
        assert_eq!(project().unwrap() != before, changed, "{from}");
    }
    for altered in [
        format!("{original}\nenv:\n  RUSTFLAGS: changed\n"),
        original.replace("  browser:\n", "  browser: &shared\n"),
        original.replace("    env:\n", "    env: *shared\n"),
        original.replace("Save the exact backend success", "renamed boundary"),
        original.replace(
            "  web:\n",
            "    defaults:\n      run:\n        shell: custom\n\n  web:\n",
        ),
        original.replace("  web:\n", "    container: changed\n\n  web:\n"),
    ] {
        assert_ne!(altered, original);
        fs::write(&file, altered).unwrap();
        let error = project().unwrap_err();
        assert!(error.to_string().contains("ci_backend_workflow"));
    }
    fs::remove_file(file).unwrap();
    fs::remove_dir(path).unwrap();
}
