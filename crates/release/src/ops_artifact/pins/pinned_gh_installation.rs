//! Every runtime proof workflow installs its GitHub CLI from the tool lock
//! with `simplestchat-security tools`, into one directory of the runner's
//! own, and resolves the verified executable there before any step is given a
//! credential.

use super::{checkout_file, steps, workflow};
use serde_json::Value;

const WORKFLOWS: [&str; 8] = [
    "maintenance-host-runtime.yml",
    "maintenance-launcher-runtime.yml",
    "ops-runtime.yml",
    "release-backup-runtime.yml",
    "release-deploy-runtime.yml",
    "release-fetch-runtime.yml",
    "release-receive-runtime.yml",
    "release-stage-runtime.yml",
];
const TOOLS: &str = "cargo run --locked -q -p simplestchat-security -- tools";

/// Whether a step's environment names a GitHub credential.
fn credentialed(step: &Value) -> bool {
    step.get("env")
        .and_then(Value::as_object)
        .is_some_and(|env| {
            env.keys()
                .any(|name| matches!(name.as_str(), "GH_TOKEN" | "GITHUB_TOKEN"))
        })
}

/// The quoted values of every `--directory` option in a script.
fn directories(script: &str) -> Vec<&str> {
    script
        .split("--directory \"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect()
}

#[test]
fn every_runtime_proof_installs_its_gh_from_the_lock_before_any_credential() {
    for name in WORKFLOWS {
        let text = checkout_file(&format!(".github/workflows/{name}"));
        assert!(!text.contains("security_tools.py"), "{name}");
        assert_eq!(text.matches("simplestchat-security").count(), 2, "{name}");
        let workflow = workflow(name);
        let mut installed = 0;
        for job in workflow["jobs"].as_object().unwrap().keys() {
            let steps = steps(&workflow, job);
            let exposed = steps.iter().position(credentialed);
            for (index, step) in steps.iter().enumerate() {
                let script = step.get("run").and_then(Value::as_str).unwrap_or("");
                if !script.contains("simplestchat-security") {
                    continue;
                }
                installed += 1;
                let install = format!("{TOOLS} install --tools gh");
                let resolve = format!("=\"$({TOOLS} path gh");
                let (at, then) = (script.find(&install), script.find(&resolve));
                assert!(at.is_some() && at < then, "{name}");
                // Nothing but the directory follows either command's words.
                for command in [&install, &resolve] {
                    let rest = script.split(command.as_str()).nth(1).unwrap_or("");
                    let rest = rest.trim_start_matches([' ', '\\', '\n']);
                    assert!(rest.starts_with("--directory \""), "{name}");
                }
                let [first, second] = directories(script)[..] else {
                    panic!("{name}: one directory option for each command");
                };
                assert_eq!(first, second, "{name}");
                let own = first.strip_prefix("$RUNNER_TEMP/").unwrap_or("");
                assert!(
                    own.ends_with("-gh")
                        && own
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'-'),
                    "{name}: {first}"
                );
                assert!(
                    script.contains(&format!("\" = \"{first}/bin/gh\"")),
                    "{name}: the resolved path is compared with the installed one"
                );
                assert!(!credentialed(step), "{name}");
                assert!(exposed.is_none_or(|at| index < at), "{name}");
            }
        }
        assert_eq!(installed, 1, "{name}");
    }
}
