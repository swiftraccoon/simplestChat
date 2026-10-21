//! Exact source and environment inputs for reusable CI successes.

use super::{Context, Scope, inputs, io, require};
use anyhow::Result;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn matches(pattern: &str, text: &str) -> bool {
    Regex::new(pattern)
        .expect("fixed CI input pattern")
        .is_match(text)
}

fn runner(context: &Context) -> Result<String> {
    if context.env("ACT") == "true" {
        let image = context.env("LOCAL_CI_RUNNER_IMAGE");
        require(
            matches(r"\A[a-z0-9./_-]+@sha256:[a-f0-9]{64}\z", image),
            "ci_cache_local_image",
        )?;
        Ok(format!("local:{image}"))
    } else {
        let image = context.env("ImageOS");
        let version = context.env("ImageVersion");
        require(
            matches(r"\A[a-zA-Z0-9._-]{1,100}\z", image)
                && matches(r"\A[a-zA-Z0-9._-]{1,100}\z", version),
            "ci_cache_hosted_image",
        )?;
        Ok(format!("hosted:{image}:{version}"))
    }
}

fn backend_environment(context: &Context) -> Result<BTreeMap<String, String>> {
    let mut environment = context
        .environment
        .iter()
        .filter(|(name, _)| {
            inputs::BACKEND_ENVIRONMENT.contains(&name.as_str())
                || ["CARGO_BUILD_", "CARGO_PROFILE_", "CARGO_TARGET_"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    let target = context
        .environment
        .get("CARGO_TARGET_DIR")
        .map_or_else(|| context.root.join("target"), PathBuf::from);
    require(
        io::resolve(&target)? == context.root.join("target"),
        "ci_backend_target_directory",
    )?;
    let cargo_home = if context.env("CARGO_HOME").is_empty() {
        let home = context
            .environment
            .get("HOME")
            .map(|home| PathBuf::from(if home.is_empty() { "/" } else { home }))
            .or_else(std::env::home_dir)
            .ok_or_else(|| anyhow::anyhow!("ci_cache_home"))?;
        home.join(".cargo").to_string_lossy().into_owned()
    } else {
        context.env("CARGO_HOME").to_owned()
    };
    environment.insert("CARGO_HOME".to_owned(), cargo_home);
    environment.insert(
        "CARGO_INCREMENTAL".to_owned(),
        context
            .environment
            .get("CARGO_INCREMENTAL")
            .cloned()
            .unwrap_or_else(|| "0".to_owned()),
    );
    environment.insert(
        "workspace".to_owned(),
        context.root.to_string_lossy().into_owned(),
    );
    Ok(environment)
}

/// Strict source-region projection of the existing browser build prefix. YAML
/// aliases, repeated boundaries and global/job settings after it fail closed.
pub(super) fn backend_workflow(path: &Path) -> Result<Vec<u8>> {
    let bytes = io::bounded(path, 1024 * 1024)?;
    let text = std::str::from_utf8(&bytes)?;
    require(
        text.matches("\njobs:\n").count() == 1,
        "ci_backend_workflow_jobs",
    )?;
    let (global, jobs) = text
        .split_once("\njobs:\n")
        .expect("validated jobs boundary");
    require(!matches(r"(?m)^[^\s#]", jobs), "ci_backend_workflow_global")?;
    require(
        jobs.matches("\n  browser:\n").count() == 1,
        "ci_backend_workflow_browser",
    )?;
    let browser = jobs
        .split_once("\n  browser:\n")
        .expect("validated browser boundary")
        .1;
    let next = Regex::new(r"(?m)^  [a-z][a-z0-9-]*:\s*$").expect("fixed job pattern");
    let browser = next
        .find(browser)
        .map_or(browser, |entry| &browser[..entry.start()]);
    const BOUNDARY: &str = "      - name: Migrate disposable test database with the real server\n";
    require(
        browser.matches(BOUNDARY).count() == 1,
        "ci_backend_workflow_boundary",
    )?;
    let (prefix, runtime) = browser
        .split_once(BOUNDARY)
        .expect("validated runtime boundary");
    require(
        !matches(r"(?m)^    [^\s#]", runtime),
        "ci_backend_workflow_late_job",
    )?;
    for marker in [
        "    steps:\n",
        "      - name: Build server and load generator\n",
        "      - name: Save the exact backend success\n",
    ] {
        require(
            prefix.matches(marker).count() == 1,
            "ci_backend_workflow_build",
        )?;
    }
    let selected = format!("{global}\njobs:\n  browser:\n{prefix}");
    require(
        !matches(r"(?:^|[\s\[{},:])[&*][\w-]+", &selected),
        "ci_backend_workflow_alias",
    )?;
    Ok(selected.into_bytes())
}

pub(super) fn key(context: &Context, scope: Scope) -> Result<String> {
    let trust = if context.trusted() {
        "main"
    } else {
        "untrusted"
    };
    let identity = BTreeMap::from([
        ("scope".to_owned(), scope.name().to_owned()),
        ("trust".to_owned(), trust.to_owned()),
        ("system".to_owned(), context.system.clone()),
        ("architecture".to_owned(), context.architecture.clone()),
        ("runner".to_owned(), runner(context)?),
    ]);
    let mut hashed = Sha256::new();
    hashed.update(io::json_object(&identity).as_bytes());
    if scope == Scope::Backend {
        hashed.update(io::json_object(&backend_environment(context)?).as_bytes());
    }
    let tracked = context.tracked(true)?;
    let mut count = 0;
    for entry in tracked
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let separator = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| anyhow::anyhow!("ci_cache_tracked_type"))?;
        let (metadata, name_field) = entry.split_at(separator);
        let raw_name = &name_field[1..];
        let metadata = std::str::from_utf8(metadata)?;
        let words = metadata.split_whitespace().collect::<Vec<_>>();
        let [mode, _, stage] = words.as_slice() else {
            anyhow::bail!("ci_cache_tracked_type");
        };
        require(
            *stage == "0" && matches!(*mode, "100644" | "100755"),
            "ci_cache_tracked_type",
        )?;
        let name = std::str::from_utf8(raw_name)?;
        if !inputs::selected(scope, name) {
            continue;
        }
        let path = context.root.join(name);
        require(
            !path.is_symlink() && path.is_file(),
            "ci_cache_source_missing",
        )?;
        let content = if scope == Scope::Backend && name == ".github/workflows/ci.yml" {
            io::sha256(&backend_workflow(&path)?)
        } else {
            io::digest(&path)?
        };
        hashed.update(raw_name);
        hashed.update(b"\0");
        hashed.update(mode.as_bytes());
        hashed.update(b"\0");
        hashed.update(content.as_bytes());
        count += 1;
    }
    require(count > 0, "ci_cache_empty_inputs")?;
    Ok(format!(
        "ci-verified-v1-{}-{trust}-{}",
        scope.name(),
        io::hex(&hashed.finalize())
    ))
}
