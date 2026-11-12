//! The disposable-host release integration in CI's `deployment` job, carried
//! over from the Python workflow tests. Every named step is the job's only
//! step with that name, and each pin inspects the workflow offline.

use super::{checkout_file, lacks, run, script, steps, unique, uses, workflow};
use serde_json::{Value, json};

const COMPOSE_TOOL: &str = "Install checksum-pinned production Compose";
const PREPARE: &str = "Install isolated release-container checks";
const PREPARE_START: &str = "Start the isolated release-container controller build";
const INTEGRATION: &str = "App-only release and rollback on a disposable host";
const CONTROLLER: &str = "Prepare the source-built native release controller";
const INSTALLED: &str = "Install the source-built Ops executable where a prepared host keeps it";
const HARNESS: &str = "build/release_container_harness.py";
const ROLLOVER_SCRIPT: &str = "web/e2e/release-assets.cjs";
const RETENTION: &str = "Preserve sanitized release-container report";
const LOCAL_DIAGNOSTICS: &str = "Preserve private local release-container diagnostics";
const EXPORT: &str = "Export the tested immutable production image";
const RELEASE: &str = "Retain the verified production release";
const BUILD: &str = "Build production container";
const VERIFY: &str = "Verify production image contents and user";
const SMOKE: &str = "Production startup, migration and database-backed API smoke";
const GUEST: &str = "Verify the guest checker against pinned Compose JSON";
const TOOLCHAIN_ACTION: &str = "dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d";
const CACHE_PIN: &str = "55cc8345863c7cc4c66a329aec7e433d2d1c52a9";
const ROLLOVER: &str = "${{ runner.temp }}/rollover-playwright";
const LOCAL_ONLY: &str = "${{ always() && env.ACT == 'true' }}";
const RUN_COMMAND: &str = concat!(
    "\"$RUNNER_TEMP/release-http-policy-build/debug/simplestchat-ci\" release-checks \\\n",
    "  --http-policy-binary \"$RUNNER_TEMP/release-http-policy-build/debug/simplestchat-ci\"\n"
);

/// The deployment job and its ordered steps.
struct Deployment {
    ci: Value,
}

impl Deployment {
    fn read() -> Self {
        Self {
            ci: workflow("ci.yml"),
        }
    }

    fn job(&self) -> &Value {
        &self.ci["jobs"]["deployment"]
    }

    fn steps(&self) -> &[Value] {
        steps(&self.ci, "deployment")
    }

    /// Exactly one step with `name`.
    fn step(&self, name: &str) -> &Value {
        unique(self.steps(), name)
    }

    /// The position of the one step with `name`.
    fn at(&self, name: &str) -> usize {
        position(self.steps(), self.step(name))
    }

    /// Whether the named steps appear in exactly this order.
    fn ordered(&self, names: &[&str]) -> bool {
        let positions = names.iter().map(|name| self.at(name)).collect::<Vec<_>>();
        positions.is_sorted()
    }
}

/// The position of one borrowed step among `steps`.
fn position(steps: &[Value], wanted: &Value) -> usize {
    let found = steps.iter().position(|step| std::ptr::eq(step, wanted));
    found.unwrap()
}

fn deployment_run(name: &str) -> String {
    run(Deployment::read().step(name)).to_owned()
}

/// From `test_existing_image_is_built_and_smoked_once_before_release_integration`
/// and `test_one_revision_labeled_immutable_image_feeds_all_smokes_and_export`,
/// which both pinned the build command.
#[test]
fn the_production_image_is_built_through_the_ci_binary() {
    assert_eq!(
        deployment_run(BUILD),
        "cargo run --locked -q -p simplestchat-ci -- production-image build"
    );
}

/// From `test_one_revision_labeled_immutable_image_feeds_all_smokes_and_export`.
#[test]
fn the_production_image_is_verified_and_smoked_through_the_ci_binary() {
    assert_eq!(
        deployment_run(VERIFY),
        "cargo run --locked -q -p simplestchat-ci -- production-image verify"
    );
    assert_eq!(
        deployment_run(SMOKE),
        "cargo run --locked -q -p simplestchat-ci -- container-smoke"
    );
}

/// From `test_optional_http_owner_is_built_once_with_explicit_system_tls_and_separate_target`.
#[test]
fn the_release_http_policy_binary_is_the_feature_enabled_ci_build() {
    assert!(
        deployment_run(GUEST)
            .contains("cargo build --locked -p simplestchat-ci --features release-http-policy")
    );
}

/// From `test_local_failed_fixture_uses_the_compiled_private_evidence_policy`.
#[test]
fn local_release_container_diagnostics_copy_through_the_ci_binary() {
    assert_eq!(
        deployment_run(LOCAL_DIAGNOSTICS),
        "cargo run --locked -q -p simplestchat-ci -- copy-evidence release-container"
    );
}

/// From `test_fresh_host_and_job_deadline_include_the_existing_build`: the
/// fixture uses the runner's own Docker daemon.
#[test]
fn fresh_host_and_job_deadline_include_the_existing_build() {
    let deployment = Deployment::read();
    let job = deployment.job();
    assert_eq!(job["runs-on"], "ubuntu-24.04");
    assert_eq!(job["timeout-minutes"], 95);
    assert!(lacks(job, &["container", "services", "continue-on-error"]));
    assert_eq!(job["env"]["PRODUCTION_IMAGE"], "simplestchat-ci:production");
}

/// The release integration runs the Ops executable this job built, by the
/// path a signed release installs it at on a prepared host: root's, mode
/// 0755, installed once, unconditionally, after it is built and before the
/// integration. The harness names that path and no interpreter-run release
/// helper.
#[test]
fn the_release_integration_runs_the_installed_source_built_ops_executable() {
    let deployment = Deployment::read();
    assert!(deployment.ordered(&[CONTROLLER, BUILD, INSTALLED, INTEGRATION]));
    assert_eq!(deployment.at(INSTALLED) + 1, deployment.at(INTEGRATION));
    let built = concat!(
        "cargo build --locked --release -p simplestchat-ops --bin simplestchat-ops \\\n",
        "  --target-dir \"$RUNNER_TEMP/release-builder-build\"",
    );
    assert!(script(deployment.step(CONTROLLER)).contains(built));
    let installed = deployment.step(INSTALLED);
    assert_eq!(
        installed["run"],
        concat!(
            "sudo install -D -o root -g root -m 0755 \\\n",
            "  \"$RUNNER_TEMP/release-builder-build/release/simplestchat-ops\" \\\n",
            "  /usr/local/libexec/simplestchat-public/simplestchat-ops\n",
        )
    );
    assert_eq!(installed["timeout-minutes"], 1);
    assert!(lacks(
        installed,
        &["if", "continue-on-error", "env", "shell"]
    ));
    let harness = checkout_file(HARNESS);
    let path = "OPS = \"/usr/local/libexec/simplestchat-public/simplestchat-ops\"\n";
    assert_eq!(harness.matches(path).count(), 1);
    // Staging admits a whole budget of at most 600 seconds; a deployment
    // needs more than the time it reserves for its one rollback.
    for line in [
        "FINAL = (\"--finalization-seconds\", \"60\")\n",
        "        budget = \"600\" if action == \"stage\" else \"900\"",
        "        argv = [OPS, \"release\", action, \"--revision\", candidate[\"revision\"], ",
        "candidate[\"revision\"], \"--timeout-seconds\"]\n",
        "self.commands.run([*argv, budget, *FINAL], timeout=240, success=False)\n",
    ] {
        assert_eq!(harness.matches(line).count(), 1, "{line}");
    }
    assert!(!harness.contains("release-public.py"));
    // The proxy's configuration is root's and readable, as a prepared host
    // keeps it: the deployment admits no other owner for that file.
    let proxy = "        (CONFIG / \"Caddyfile\").chmod(0o644)";
    assert_eq!(harness.matches(proxy).count(), 1);
    assert!(!harness.contains("os.chown(CONFIG / \"Caddyfile\""));
    // The browser check's own two deployments are the same executable.
    let script = checkout_file(ROLLOVER_SCRIPT);
    for line in [
        "const OPS = '/usr/local/libexec/simplestchat-public/simplestchat-ops';\n",
        "const DEPLOY_BUDGET = ['--timeout-seconds', '900', '--finalization-seconds', '60'];\n",
        "      execute(OPS, ['release', 'deploy', '--revision', revision, ...DEPLOY_BUDGET], {\n",
    ] {
        assert_eq!(script.matches(line).count(), 1, "{line}");
    }
    assert!(!script.contains("python") && !script.contains("release-public"));
}

/// From `test_existing_image_is_built_and_smoked_once_before_release_integration`.
#[test]
fn existing_image_is_built_and_smoked_once_before_release_integration() {
    let deployment = Deployment::read();
    assert!(deployment.ordered(&[
        COMPOSE_TOOL,
        "Validate Compose rendering",
        BUILD,
        VERIFY,
        SMOKE,
        EXPORT,
        PREPARE,
        INTEGRATION,
        RELEASE,
        RETENTION,
    ]));
    let builds = deployment
        .steps()
        .iter()
        .filter(|step| script(step).contains("production-image build"))
        .collect::<Vec<_>>();
    let [build] = builds[..] else {
        panic!("exactly one step builds the production image");
    };
    assert!(std::ptr::eq(build, deployment.step(BUILD)));
}

/// From `test_one_revision_labeled_immutable_image_feeds_all_smokes_and_export`:
/// no later step replaces the checked immutable image ID.
#[test]
fn one_revision_labeled_immutable_image_feeds_all_smokes_and_export() {
    let deployment = Deployment::read();
    assert_eq!(deployment.step(BUILD)["shell"], "bash");
    for step in &deployment.steps()[deployment.at(BUILD) + 1..] {
        let replaced = step
            .get("env")
            .is_some_and(|env| env.as_object().unwrap().contains_key("PRODUCTION_IMAGE"));
        assert!(!replaced);
    }
    let smoke = deployment.step(SMOKE);
    assert_eq!(smoke["timeout-minutes"], 5);
    assert_eq!(
        smoke["env"]["CONTAINER_TEST_ARTIFACTS"],
        "${{ runner.temp }}/container-smoke"
    );
    assert!(lacks(smoke, &["if"]));
    assert!(
        checkout_file("build/test-container.sh")
            .contains("${PRODUCTION_IMAGE:-simplestchat-ci:production}")
    );
    assert_eq!(deployment.step(INTEGRATION)["run"], RUN_COMMAND);
}

/// From `test_image_policy_has_pinned_rust_on_hosted_and_disposable_ci`: the
/// compiled policy runs in both contexts without changing action scope.
#[test]
fn image_policy_has_pinned_rust_on_hosted_and_disposable_ci() {
    let deployment = Deployment::read();
    let installer = &deployment.steps()[1];
    assert_eq!(
        installer["name"],
        "Install the pinned Rust toolchain for CI image policy"
    );
    assert!(lacks(installer, &["if"]));
    assert_eq!(installer["uses"], TOOLCHAIN_ACTION);
    assert_eq!(installer["with"]["toolchain"], "1.99.0");
    let database = &steps(&deployment.ci, "image-database")[1];
    assert_eq!(
        database["name"],
        "Install the pinned Rust toolchain for CI database cache policy"
    );
    assert!(lacks(database, &["if"]));
    assert_eq!(database["uses"], installer["uses"]);
    assert_eq!(database["with"], installer["with"]);
}

/// From `test_push_export_never_rebuilds_and_publication_waits_for_required_checks`:
/// pull-request image policy inspects the canonical export too, and the export
/// runs before any root fixture state exists.
#[test]
fn push_export_never_rebuilds_and_publication_waits_for_required_checks() {
    let deployment = Deployment::read();
    assert_eq!(deployment.ci["on"]["push"]["branches"], json!(["main"]));
    let export = deployment.step(EXPORT);
    assert!(lacks(export, &["if", "continue-on-error"]));
    assert_eq!(export["timeout-minutes"], 6);
    assert_eq!(
        export["run"],
        concat!(
            "\"$RUNNER_TEMP/release-builder-build/release/simplestchat-ops\" release build \\\n",
            "  --image-id \"${PRODUCTION_IMAGE}\" \\\n",
            "  --timeout-seconds 300 --output \"${RUNNER_TEMP}/simplestchat-release\"\n"
        )
    );
    assert!(!run(export).contains("sudo"));
    let release = deployment.step(RELEASE);
    assert_eq!(
        release["if"],
        "${{ success() && github.event_name == 'push' && !env.ACT }}"
    );
    let pinned = regex::Regex::new(r"^actions/upload-artifact@[a-f0-9]{40}$").unwrap();
    assert!(pinned.is_match(uses(release)));
    assert_eq!(
        release["with"],
        json!({
            "name": "simplestchat-unsigned-${{ github.sha }}",
            "path": "${{ runner.temp }}/simplestchat-release",
            "if-no-files-found": "error",
            "compression-level": 0,
            "retention-days": 14,
        })
    );
    let scan = "Scan the same exported production image";
    assert!(deployment.ordered(&[EXPORT, INTEGRATION, scan, RELEASE]));
    let image_check = deployment.step(scan);
    assert!(lacks(image_check, &["if", "continue-on-error"]));
    for text in [
        "build/check-security.sh image \"${PRODUCTION_IMAGE}\"",
        "--artifact-dir \"${RUNNER_TEMP}/simplestchat-release\"",
        "--image-tools \"${RUNNER_TEMP}/release-checks/tools\"",
    ] {
        assert!(run(image_check).contains(text), "{text}");
    }
    assert!(lacks(release, &["continue-on-error"]));
}

/// From `test_compose_is_verified_before_install_and_matches_both_execution_users`.
/// Pin selection, verified bytes, the privilege and TLS arguments, both
/// version checks and failure ownership are covered by the installer's tests.
#[test]
fn compose_is_verified_before_install_and_matches_both_execution_users() {
    let deployment = Deployment::read();
    let step = deployment.step(COMPOSE_TOOL);
    assert_eq!(step["timeout-minutes"], 3);
    assert_eq!(step["shell"], "bash");
    assert!(lacks(step, &["continue-on-error", "if"]));
    assert_eq!(
        step["run"],
        "cargo run --locked -q -p simplestchat-ci -- install-compose"
    );
    assert!(deployment.at(COMPOSE_TOOL) < deployment.at("Validate Compose rendering"));
    assert!(deployment.at(COMPOSE_TOOL) < deployment.at(BUILD));
}

/// From `test_guest_compose_contract_runs_with_required_pinned_renderer`: the
/// JSON contract regression cannot silently skip in production-image CI, and
/// the existing job deadline bounds cold feature builds.
#[test]
fn guest_compose_contract_runs_with_required_pinned_renderer() {
    let deployment = Deployment::read();
    let step = deployment.step(GUEST);
    assert!(lacks(step, &["if", "continue-on-error", "timeout-minutes"]));
    assert_eq!(step["env"]["VM_COMPOSE_REQUIRED"], "1");
    for text in [
        "\"${RUNNER_TEMP}/release-container-controller/bin/python\" -B -m unittest discover",
        "-s ops/ansible/tests -p test_security_vm_guest.py -k ComposeSerializationTests -v",
    ] {
        assert!(run(step).contains(text), "{text}");
    }
    assert!(deployment.at(COMPOSE_TOOL) < deployment.at(GUEST));
    assert!(deployment.at(PREPARE) < deployment.at(GUEST));
    assert!(deployment.at(GUEST) < deployment.at(INTEGRATION));
}

/// From `test_optional_http_owner_is_built_once_with_explicit_system_tls_and_separate_target`:
/// a later default build cannot replace the selected feature-enabled executable.
#[test]
fn optional_http_owner_is_built_once_with_explicit_system_tls_and_separate_target() {
    let deployment = Deployment::read();
    let script = run(deployment.step(GUEST));
    assert!(script.contains("sudo apt-get install -y libssl-dev pkg-config"));
    assert!(script.contains("--target-dir \"$RUNNER_TEMP/release-http-policy-build\""));
    for variable in [
        "OPENSSL_DIR",
        "OPENSSL_STATIC",
        "PKG_CONFIG_PATH",
        "AWS_LC_SYS_SYSTEM_DIR",
        "AWS_LC_SYS_STATIC",
        "AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK",
    ] {
        assert!(script.contains(&format!("-u {variable}")), "{variable}");
    }
    assert!(
        script.find("-k ComposeSerializationTests -v").unwrap()
            < script.find("cargo build").unwrap()
    );
    assert!(!RUN_COMMAND.contains("cargo run"));
    assert_eq!(deployment.step(INTEGRATION)["run"], RUN_COMMAND);
}

/// From `test_dependencies_use_existing_pinned_controller_requirements_in_an_isolated_venv`:
/// the controller build overlaps the image build instead of following it. The
/// ordered package and environment commands are covered by the owner's tests.
#[test]
fn dependencies_use_existing_pinned_controller_requirements_in_an_isolated_venv() {
    let deployment = Deployment::read();
    let start = deployment.step(PREPARE_START);
    assert_eq!(
        start["run"],
        "cargo run --locked -q -p simplestchat-ci -- release-controller start"
    );
    assert!(deployment.at(PREPARE_START) < deployment.at(BUILD));
    let prepare = deployment.step(PREPARE);
    assert_eq!(prepare["timeout-minutes"], 5);
    assert_eq!(
        prepare["run"],
        "cargo run --locked -q -p simplestchat-ci -- release-controller wait"
    );
    for step in [start, prepare] {
        assert!(lacks(step, &["continue-on-error", "if"]));
    }
}

/// From `test_controller_setup_follows_validation_and_still_overlaps_image_work`:
/// host package setup starts after validation, and its later handoff stays
/// required.
#[test]
fn controller_setup_follows_validation_and_still_overlaps_image_work() {
    let deployment = Deployment::read();
    assert!(deployment.ordered(&[
        COMPOSE_TOOL,
        "Validate Compose rendering",
        "Validate Caddyfile",
        "Preserve caddy validation metadata",
        PREPARE_START,
        BUILD,
        PREPARE,
        INTEGRATION,
    ]));
    for name in ["Validate Caddyfile", PREPARE_START, BUILD, PREPARE] {
        assert!(
            lacks(deployment.step(name), &["if", "continue-on-error"]),
            "{name}"
        );
    }
    assert_eq!(
        deployment.step("Preserve caddy validation metadata")["if"],
        "${{ always() && !env.ACT }}"
    );
}

/// From `test_root_disposable_opt_in_is_explicit_bounded_and_preserves_failure`:
/// a required release check is never skipped, and no deployment secret or
/// helper override enters the fixture.
#[test]
fn root_disposable_opt_in_is_explicit_bounded_and_preserves_failure() {
    let deployment = Deployment::read();
    let integration = deployment.step(INTEGRATION);
    assert_eq!(integration["run"], RUN_COMMAND);
    assert_eq!(integration["timeout-minutes"], 20);
    assert!(lacks(integration, &["continue-on-error", "if"]));
    assert_eq!(
        integration["env"],
        json!({"PLAYWRIGHT_BROWSERS_PATH": ROLLOVER})
    );
}

/// From `test_pinned_rollover_browser_runs_after_image_build_without_an_extra_app_build`:
/// only browser download bytes are cached; pinned installation stays
/// unconditional.
#[test]
fn pinned_rollover_browser_runs_after_image_build_without_an_extra_app_build() {
    let deployment = Deployment::read();
    let steps = deployment.steps();
    let setups = steps
        .iter()
        .filter(|step| uses(step).starts_with("actions/setup-node@"))
        .collect::<Vec<_>>();
    let [setup] = setups[..] else {
        panic!("exactly one Node setup");
    };
    assert_eq!(
        setup["uses"],
        "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020"
    );
    assert_eq!(
        setup["with"],
        json!({
            "node-version": "26.10.0",
            "cache": "npm",
            "cache-dependency-path": "web/e2e/package-lock.json",
        })
    );
    let npm = deployment.step("Install checksum-pinned npm");
    assert!(
        run(npm).contains("node build/install-npm.mjs \"$RUNNER_TEMP/simplestchat-npm-12.2.0\"")
    );
    let restore = deployment.step("Restore pinned rollover Chromium download bytes");
    let tooling = deployment.step("Install pinned rollover browser tooling");
    let save = deployment.step("Save pinned rollover Chromium download bytes");
    assert_eq!(restore["id"], "rollover-chromium");
    assert_eq!(
        restore["uses"],
        format!("actions/cache/restore@{CACHE_PIN}")
    );
    assert_eq!(
        restore["with"],
        json!({
            "path": ROLLOVER,
            "key": concat!(
                "rollover-chromium-v1-${{ runner.os }}-${{ runner.arch }}-",
                "${{ hashFiles('web/e2e/package-lock.json') }}"
            ),
        })
    );
    assert_eq!(tooling["timeout-minutes"], 10);
    assert_eq!(
        tooling["env"],
        json!({"PLAYWRIGHT_BROWSERS_PATH": ROLLOVER})
    );
    assert_eq!(deployment.step(INTEGRATION)["env"], tooling["env"]);
    let command = run(tooling);
    assert!(command.contains("npm --prefix web/e2e ci --ignore-scripts\n"));
    assert!(
        command.contains("npm --prefix web/e2e exec -- playwright install --with-deps chromium\n")
    );
    assert_eq!(
        save["if"],
        "steps.rollover-chromium.outputs.cache-hit != 'true'"
    );
    assert_eq!(save["uses"], format!("actions/cache/save@{CACHE_PIN}"));
    assert_eq!(
        save["with"],
        json!({
            "path": ROLLOVER,
            "key": "${{ steps.rollover-chromium.outputs.cache-primary-key }}",
        })
    );
    let order = [
        deployment.step(BUILD),
        setup,
        npm,
        restore,
        tooling,
        save,
        deployment.step(INTEGRATION),
    ];
    assert!(order.map(|step| position(steps, step)).is_sorted());
    for step in [setup, npm, tooling] {
        assert!(lacks(step, &["if", "continue-on-error"]));
    }
    assert!(
        steps
            .iter()
            .all(|step| !script(step).contains("npm --prefix web run build"))
    );
}

/// From `test_completed_layers_are_saved_before_later_checks_can_fail`: a
/// successful build survives fixture failures without saving partial layers.
#[test]
fn completed_layers_are_saved_before_later_checks_can_fail() {
    let deployment = Deployment::read();
    let restore = deployment.step("Restore image layer cache");
    let save = deployment.step("Save completed image layer cache");
    assert_eq!(
        restore["uses"],
        format!("actions/cache/restore@{CACHE_PIN}")
    );
    assert_eq!(restore["id"], "production-cache");
    assert_eq!(save["uses"], format!("actions/cache/save@{CACHE_PIN}"));
    assert_eq!(
        save["if"],
        "${{ success() && steps.production-cache.outputs.cache-hit != 'true' }}"
    );
    assert!(restore["with"]["path"].is_string());
    assert_eq!(
        save["with"],
        json!({
            "path": restore["with"]["path"],
            "key": "${{ steps.production-cache.outputs.cache-primary-key }}",
        })
    );
    let saved = deployment.at("Save completed image layer cache");
    assert_eq!(saved, deployment.at(BUILD) + 1);
    assert!(saved < deployment.at(VERIFY));
    assert!(saved < deployment.at(INTEGRATION));
    for step in [restore, save] {
        assert!(lacks(step, &["continue-on-error"]));
    }
}

/// From `test_local_failed_fixture_uses_the_compiled_private_evidence_policy`:
/// diagnostics are kept after failures; the copy's bytes and permissions are
/// covered by the evidence owner's tests.
#[test]
fn local_failed_fixture_uses_the_compiled_private_evidence_policy() {
    let deployment = Deployment::read();
    let retention = deployment.step(LOCAL_DIAGNOSTICS);
    assert_eq!(retention["if"], LOCAL_ONLY);
    assert_eq!(
        deployment.at(LOCAL_DIAGNOSTICS),
        deployment.at(INTEGRATION) + 1
    );
    assert_eq!(retention["shell"], "bash");
    assert!(lacks(retention, &["env", "continue-on-error"]));
}

/// From `test_pinned_compiler_precedes_image_policy_and_all_guarded_local_evidence`:
/// image policy always compiles, and local-only evidence copies keep their
/// guards.
#[test]
fn pinned_compiler_precedes_image_policy_and_all_guarded_local_evidence() {
    let ci = workflow("ci.yml");
    for (job, copies) in [
        ("image-database", &["image-database"][..]),
        ("deployment", &["release-checks", "release-container"][..]),
    ] {
        let steps = steps(&ci, job);
        let bootstrap = &steps[1];
        assert_eq!(bootstrap["uses"], TOOLCHAIN_ACTION, "{job}");
        assert_eq!(bootstrap["with"], json!({"toolchain": "1.99.0"}), "{job}");
        let scope = [
            "if",
            "env",
            "permissions",
            "environment",
            "continue-on-error",
        ];
        assert!(lacks(bootstrap, &scope), "{job}");
        let mut kinds = Vec::new();
        for (index, step) in steps.iter().enumerate() {
            if !script(step).contains("copy-evidence ") {
                continue;
            }
            let kind = run(step).rsplit(' ').next().unwrap();
            kinds.push(kind);
            assert_eq!(step["if"], LOCAL_ONLY, "{kind}");
            assert_eq!(
                step["run"],
                format!("cargo run --locked -q -p simplestchat-ci -- copy-evidence {kind}")
            );
            assert!(lacks(step, &["env", "continue-on-error"]), "{kind}");
            assert!(index > 1, "{kind}");
        }
        kinds.sort_unstable();
        assert_eq!(kinds, copies, "{job}");
    }
}

/// From `test_read_only_permissions_pinned_actions_and_no_remote_deployment`:
/// first-party GitHub actions only, pinned by commit, beside the one fixed
/// toolchain bootstrap.
#[test]
fn read_only_permissions_pinned_actions_and_no_remote_deployment() {
    let deployment = Deployment::read();
    assert_eq!(deployment.ci["permissions"], json!({"contents": "read"}));
    assert!(lacks(deployment.job(), &["permissions", "environment"]));
    let steps = deployment.steps();
    let first_party = regex::Regex::new(
        r"^actions/(?:checkout|setup-node|cache/(?:restore|save)|upload-artifact)@[a-f0-9]{40}$",
    )
    .unwrap();
    for (index, step) in steps.iter().enumerate() {
        assert!(lacks(step, &["permissions", "environment"]));
        if step.get("uses").is_none() {
            continue;
        }
        if uses(step).starts_with("dtolnay/rust-toolchain@") {
            assert_eq!(
                index, 1,
                "only the fixed bootstrap may use the toolchain action"
            );
        } else {
            assert!(first_party.is_match(uses(step)), "{}", uses(step));
        }
    }
    let checkout = steps
        .iter()
        .find(|step| uses(step).starts_with("actions/checkout@"))
        .unwrap();
    assert_eq!(checkout["with"], json!({"persist-credentials": false}));
    let commands = steps.iter().map(script).collect::<Vec<_>>().join("\n");
    let remote = regex::Regex::new(r"\b(?:ssh|scp|ansible-playbook|kubectl|reboot)\b").unwrap();
    assert!(!remote.is_match(&commands));
    let registry = regex::Regex::new(r"\bdocker\s+(?:push|login)\b").unwrap();
    assert!(!registry.is_match(&commands));
    let secrets = regex::Regex::new(r"\$\{\{\s*secrets\.").unwrap();
    let job = serde_json::to_string(deployment.job()).unwrap();
    assert!(!secrets.is_match(&job));
}
