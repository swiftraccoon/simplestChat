//! Every workflow image build whose context carries the repository's
//! `.dockerignore` runs the checksum-pinned Buildx, installed earlier in the
//! same job, instead of the plugin the runner image happens to ship.

use super::{checkout_file, lacks, script, steps, uses, workflow};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

const NAME: &str = "Install checksum-pinned Buildx";
const INSTALL: &str = "cargo run --locked -q -p simplestchat-ci -- install-buildx";
/// The one reviewed builder that never reads the repository's ignore file.
const CADDY: (&str, &str) = ("caddy-image.yml", "publish");
const CADDY_IGNORE: &str = "build/caddy-image.Dockerfile.dockerignore";

/// Whether a step's command builds an image: the production and load-test
/// image owners, the release builder without a tested `--image-id`, or a
/// direct engine build.
fn builds(command: &str) -> bool {
    let engine = regex::Regex::new(r"\b(?:docker|podman)\s+(?:buildx\s+|image\s+)?build\b");
    command.contains("simplestchat-ci -- production-image build")
        || command.contains("simplestchat-ci -- load-generator-image")
        || command.contains(" release build") && !command.contains("--image-id")
        || engine.unwrap().is_match(command)
}

/// Every workflow file name, in order.
fn workflows() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let mut names = std::fs::read_dir(root.join(".github/workflows"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".yml") || name.ends_with(".yaml"))
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// Each job with steps that builds an image, with its steps and the position
/// of its first build.
fn builders() -> Vec<(String, String, Vec<Value>, usize)> {
    let mut found = Vec::new();
    for name in workflows() {
        let document = workflow(&name);
        let Some(jobs) = document["jobs"].as_object() else {
            continue;
        };
        for (job, body) in jobs {
            if body.get("steps").is_none() {
                continue;
            }
            let steps = steps(&document, job).to_vec();
            if let Some(first) = steps.iter().position(|step| builds(script(step))) {
                found.push((name.clone(), job.clone(), steps, first));
            }
        }
    }
    found
}

/// The builders are exactly the reviewed jobs; each one except Caddy installs
/// the pinned Buildx unconditionally, exactly once, before its first build.
#[test]
fn every_repository_context_build_installs_the_pinned_buildx_first() {
    let builders = builders();
    let jobs = builders
        .iter()
        .map(|(name, job, _, _)| (name.as_str(), job.as_str()))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        jobs,
        [
            ("caddy-image.yml", "publish"),
            ("ci.yml", "deployment"),
            ("load-generator-image.yml", "image"),
            ("release-artifact.yml", "artifact"),
        ]
        .into()
    );
    for (name, job, steps, first) in &builders {
        let installs = steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step["name"] == NAME || script(step).contains("install-buildx"))
            .collect::<Vec<_>>();
        if (name.as_str(), job.as_str()) == CADDY {
            assert!(installs.is_empty(), "{name} {job}");
            continue;
        }
        let [(position, step)] = installs[..] else {
            panic!("{name} {job}: exactly one pinned Buildx installation");
        };
        assert!(
            position < *first,
            "{name} {job}: installed before the first build"
        );
        assert_eq!(step["name"], NAME, "{name} {job}");
        assert_eq!(step["run"], INSTALL, "{name} {job}");
        assert_eq!(step["timeout-minutes"], 3, "{name} {job}");
        assert_eq!(step["shell"], "bash", "{name} {job}");
        assert!(
            lacks(step, &["if", "continue-on-error", "env"]),
            "{name} {job}"
        );
    }
}

/// No workflow step replaces the plugin through a Docker-published action.
#[test]
fn no_workflow_installs_buildx_through_a_floating_action() {
    for name in workflows() {
        let document = workflow(&name);
        let Some(jobs) = document["jobs"].as_object() else {
            continue;
        };
        for (job, body) in jobs {
            if body.get("steps").is_some() {
                for step in steps(&document, job) {
                    assert!(!uses(step).starts_with("docker/"), "{name} {job}");
                }
            }
        }
    }
}

/// Caddy's only build names its own recipe, so BuildKit reads that recipe's
/// ignore file, which admits only the proxy configuration, never the
/// repository's 500-pattern `.dockerignore`.
#[test]
fn the_caddy_build_reads_only_its_recipe_ignore_file() {
    let document = workflow(CADDY.0);
    let builds = steps(&document, CADDY.1)
        .iter()
        .filter(|step| builds(script(step)))
        .collect::<Vec<_>>();
    let [build] = builds[..] else {
        panic!("exactly one Caddy build step");
    };
    assert!(script(build).contains("--file build/caddy-image.Dockerfile \\\n"));
    let ignore = checkout_file(CADDY_IGNORE);
    assert!(ignore.starts_with("**\n"));
    assert!(ignore.lines().count() < 16);
}
