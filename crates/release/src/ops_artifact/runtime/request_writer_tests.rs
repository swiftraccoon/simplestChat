//! The writer and the runtime reader agree, and every refusal precedes a write.

use super::super::request::Request;
use super::{Inputs, Refusal, write};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

/// An owned runner directory and an existing empty step output.
struct Fixture {
    root: PathBuf,
    output: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "ops-runtime-request-writer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let output = root.join("github-output");
        fs::write(&output, b"").unwrap();
        Self { root, output }
    }

    fn inputs<'a>(&'a self, revision: &'a str, artifact: &'a str) -> Inputs<'a> {
        Inputs {
            revision,
            run: "7",
            attempt: "2",
            artifact,
            runner_temp: &self.root,
            step_output: &self.output,
        }
    }

    fn request(&self) -> PathBuf {
        self.root.join("ops-runtime-request.json")
    }

    fn unpublished(&self) -> bool {
        fs::symlink_metadata(self.request()).is_err() && fs::read(&self.output).unwrap().is_empty()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn the_runtime_reader_accepts_the_written_request_under_its_published_digest() {
    let fixture = Fixture::new();
    let revision = "c".repeat(40);
    assert_eq!(write(&fixture.inputs(&revision, "42")), Ok(()));
    let bytes = fs::read(fixture.request()).unwrap();
    let published = fs::read_to_string(&fixture.output).unwrap();
    let digest = published
        .strip_prefix("sha256=")
        .and_then(|line| line.strip_suffix('\n'))
        .unwrap();
    assert_eq!(digest, super::super::super::sha(&bytes));
    let request = Request::decode(&bytes, digest).unwrap();
    assert_eq!(request.artifact_id, 42);
    assert_eq!(request.output, fixture.root.join("ops-runtime"));
    let identity = serde_json::to_value(request.selection.identity()).unwrap();
    assert_eq!(identity["revision"], revision.as_str());
    assert_eq!(identity["buildRunId"], 7);
    assert_eq!(identity["runAttempt"], 2);
    // A second write finds the leaf and publishes no second digest.
    assert_eq!(
        write(&fixture.inputs(&revision, "42")),
        Err(Refusal::Unwritten)
    );
    assert_eq!(fs::read(fixture.request()).unwrap(), bytes);
    assert_eq!(fs::read_to_string(&fixture.output).unwrap(), published);
}

#[test]
fn refusals_precede_every_write_in_the_reviewed_order() {
    let fixture = Fixture::new();
    let revision = "c".repeat(40);
    // The revision is judged first, then the identifiers, then the directory.
    let mut invalid = fixture.inputs("C", "0");
    (invalid.run, invalid.attempt) = ("0", "0");
    assert_eq!(write(&invalid), Err(Refusal::Revision));
    assert_eq!(
        write(&fixture.inputs(&revision, "0")),
        Err(Refusal::Identifier)
    );
    assert!(fixture.unpublished());
    for runner_temp in ["", "relative/runner"] {
        let mut inputs = fixture.inputs(&revision, "42");
        inputs.runner_temp = Path::new(runner_temp);
        assert_eq!(write(&inputs), Err(Refusal::Unwritten), "{runner_temp:?}");
        let mut invalid = fixture.inputs(&revision, "0");
        invalid.runner_temp = Path::new(runner_temp);
        assert_eq!(write(&invalid), Err(Refusal::Identifier), "{runner_temp:?}");
    }
    assert!(fixture.unpublished());
    assert!(!Path::new("ops-runtime-request.json").exists());
    assert!(!Path::new("relative").exists());
}

#[test]
fn a_missing_or_linked_step_output_is_reported_and_never_created() {
    let fixture = Fixture::new();
    let revision = "c".repeat(40);
    let absent = fixture.root.join("absent-output");
    let mut inputs = fixture.inputs(&revision, "42");
    inputs.step_output = &absent;
    assert_eq!(write(&inputs), Err(Refusal::Unwritten));
    assert!(fs::symlink_metadata(&absent).is_err());
    fs::remove_file(fixture.request()).unwrap();
    std::os::unix::fs::symlink(&fixture.output, &absent).unwrap();
    assert_eq!(write(&inputs), Err(Refusal::Unwritten));
    assert!(fs::read(&fixture.output).unwrap().is_empty());
}

#[test]
fn the_written_package_version_is_the_workspace_version() {
    // The request names the version this binary was built with; the crate
    // inherits it, so it is the checkout's one workspace version.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = |path: &Path| -> toml::Table {
        toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    };
    let workspace = manifest(&root.join("Cargo.toml"));
    assert_eq!(
        workspace["workspace"]["package"]["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    let package = manifest(&Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
    assert_eq!(
        package["package"]["version"]["workspace"].as_bool(),
        Some(true)
    );
}
