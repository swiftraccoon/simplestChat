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

/// Caddy's publishing step, run as GitHub runs it with a fake Buildx that
/// writes each recorded metadata text: a lowercase SHA-256 index digest of
/// one UTF-8 JSON object becomes the step output, the attested receipt and
/// the summary line; every other metadata stops the step with the fixed
/// words before it writes any of them.
#[tokio::test]
async fn the_caddy_step_records_only_a_well_formed_index_digest() {
    use sha2::{Digest, Sha256};
    let document = workflow(CADDY.0);
    let step = steps(&document, CADDY.1)
        .iter()
        .find(|step| step["id"] == "image")
        .unwrap();
    let root = super::ansible::root();
    let scratch = std::env::temp_dir().join(format!("caddy step {}", std::process::id()));
    std::fs::create_dir(&scratch).unwrap();
    std::fs::create_dir(scratch.join("bin")).unwrap();
    let docker = scratch.join("bin/docker");
    std::fs::write(
        &docker,
        concat!(
            "#!/bin/sh\n",
            "[ \"$1 $2\" = 'buildx create' ] && exit 0\n",
            "[ \"$1 $2\" = 'buildx build' ] || exit 1\n",
            "while [ \"$#\" -gt 1 ] && [ \"$1\" != --metadata-file ]; do shift; done\n",
            "[ \"$#\" -gt 1 ] && /bin/cp \"$CASE_METADATA\" \"$2\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&docker, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let digest = format!("sha256:{}", "0123456789abcdef".repeat(4));
    let pretty = format!(
        "{{\n  \"containerimage.descriptor\": {{\n    \"digest\": \"sha256:{}\"\n  }},\n  \
         \"containerimage.digest\": \"{digest}\"\n}}",
        "f".repeat(64)
    );
    let refused: Vec<Vec<u8>> = vec![
        b"{}".to_vec(),
        format!(
            "{{\"containerimage.digest\": \"{}\"}}",
            digest.to_uppercase()
        )
        .into_bytes(),
        format!("{{\"containerimage.digest\": \"{}\"}}", &digest[..70]).into_bytes(),
        format!("{{\"containerimage.digest\": \"{digest}\\n\"}}").into_bytes(),
        format!("{{\"containerimage.digest\": \"{digest}\\u0000\"}}").into_bytes(),
        format!("{{\"containerimage.digest\": \" {digest}\"}}").into_bytes(),
        format!("{{\"containerimage.digest\": [\"{digest}\"]}}").into_bytes(),
        format!("[{{\"containerimage.digest\": \"{digest}\"}}]").into_bytes(),
        format!("{{\"containerimage.digest\": \"{digest}\"}}{{}}").into_bytes(),
        format!("{{\"containerimage.digest\": \"{digest}\", \"name\": \"\u{0}\"}}").into_bytes(),
        [
            format!("{{\"containerimage.digest\": \"{digest}\", \"name\": \"").as_bytes(),
            b"\xff\"}",
        ]
        .concat(),
        Vec::new(),
    ];
    let accepted = [
        format!("{{\"containerimage.digest\": \"{digest}\"}}").into_bytes(),
        pretty.into_bytes(),
    ];
    let hex = |path: &str| {
        Sha256::digest(std::fs::read(root.join(path)).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let image = format!("ghcr.io/owned/caddy:2.11.7-{}@{digest}", "b".repeat(40));
    let receipt = format!(
        concat!(
            "{{\n  \"version\": \"2.11.7\",\n  \"revision\": \"{revision}\",\n",
            "  \"image\": \"{image}\",\n  \"platforms\": [\n    \"linux/amd64\",\n",
            "    \"linux/arm64\"\n  ],\n  \"recipeSha256\": \"{recipe}\",\n",
            "  \"caddyfileSha256\": \"{caddyfile}\"\n}}\n"
        ),
        revision = "b".repeat(40),
        image = image,
        recipe = hex("build/caddy-image.Dockerfile"),
        caddyfile = hex("Caddyfile"),
    );
    let mut outcomes = Vec::new();
    for (index, metadata) in refused.iter().chain(&accepted).enumerate() {
        let case = scratch.join(index.to_string());
        std::fs::create_dir(&case).unwrap();
        std::fs::write(case.join("metadata"), metadata).unwrap();
        let mut bash = std::process::Command::new("/bin/bash");
        bash.args(["--noprofile", "--norc", "-eo", "pipefail", "-c"])
            .arg(format!("exec 2>&1\n{}", script(step)))
            .current_dir(&root)
            .env_clear()
            .env(
                "PATH",
                format!(
                    "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                    scratch.join("bin").display()
                ),
            )
            .env("CASE_METADATA", case.join("metadata"))
            .env("RUNNER_TEMP", &case)
            .env("GITHUB_OUTPUT", case.join("output"))
            .env("GITHUB_STEP_SUMMARY", case.join("summary"))
            .env("GITHUB_SHA", "b".repeat(40))
            .env("IMAGE_NAME", "ghcr.io/owned/caddy")
            .env("IMAGE_TAG", format!("2.11.7-{}", "b".repeat(40)));
        let (status, printed) = simplestchat_toolkit::test_env::helper_with_signals(
            bash,
            std::time::Duration::from_secs(30),
            Some(64 * 1024),
            None,
        )
        .await
        .unwrap();
        let read = |name: &str| std::fs::read_to_string(case.join(name)).ok();
        outcomes.push((
            status,
            String::from_utf8(printed).unwrap(),
            read("output"),
            read("caddy-release.json"),
            read("summary"),
        ));
    }
    std::fs::remove_dir_all(&scratch).unwrap();
    let (refusals, successes) = outcomes.split_at(refused.len());
    for (index, (status, printed, output, receipt, summary)) in refusals.iter().enumerate() {
        assert_eq!(*status, 1, "refused metadata {index}");
        assert!(printed.ends_with("Caddy image index digest is missing or malformed\n"));
        assert!(output.is_none() && receipt.is_none() && summary.is_none());
    }
    for (status, printed, output, written, summary) in successes {
        assert_eq!((*status, printed.as_str()), (0, ""));
        assert_eq!(
            output.as_deref(),
            Some(format!("digest={digest}\n").as_str())
        );
        assert_eq!(written.as_deref(), Some(receipt.as_str()));
        assert_eq!(
            summary.as_deref(),
            Some(format!("Caddy 2.11.7: `{image}`\n").as_str())
        );
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
