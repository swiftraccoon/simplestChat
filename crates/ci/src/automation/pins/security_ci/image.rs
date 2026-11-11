//! Image policy inputs: Fedora package layers, the review projection and the
//! image, layer and vulnerability-database caches.

use super::super::{checkout_file, document, first, lacks, located, run, steps, uses, workflow};
use super::TOOLCHAIN_ACTION;
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const BUILD: &str = "cargo run --locked -q -p simplestchat-ci -- production-image build";
const HINT: &str = "cargo run --locked -q -p simplestchat-ci -- image-cache hint";
const DATABASE_KEY: &str = "${{ steps.image-database-inputs.outputs.key }}";
const IMAGE_FAMILIES: [&str; 3] = ["grype", "image-license", "gitleaks"];

/// From `test_fedora_package_layers_require_current_read_only_policy_inputs`:
/// a restored layer cannot bypass changed reviews in either Fedora install
/// stage. The two inputs are read-only build mounts, never copied files.
#[test]
fn fedora_package_layers_require_current_read_only_policy_inputs() {
    let recipe = checkout_file("Dockerfile").replace("\\\n", " ");
    let header = regex::Regex::new(r"(?m)^FROM .+ AS (\S+)\n").unwrap();
    let names = header
        .captures_iter(&recipe)
        .map(|found| found[1].to_owned());
    let bodies = header.split(&recipe).skip(1);
    let expected = [
        (
            "/image-exceptions.json",
            "/tmp/simplestchat-image-exceptions.json",
        ),
        (
            "security/image-policy.json",
            "/tmp/simplestchat-image-policy.json",
        ),
    ];
    let mut checked = Vec::new();
    for (stage, body) in names.zip(bodies) {
        let installs = body
            .lines()
            .filter(|line| line.starts_with("RUN ") && line.contains("dnf upgrade"))
            .collect::<Vec<_>>();
        if installs.is_empty() {
            continue;
        }
        let [command] = installs[..] else {
            panic!("{stage}: one package installation");
        };
        let tokens = command.split_whitespace().collect::<Vec<_>>();
        for (token, (source, target)) in tokens[1..3].iter().zip(expected) {
            let mut mount = token
                .strip_prefix("--mount=")
                .unwrap()
                .split(',')
                .map(str::to_owned)
                .collect::<Vec<_>>();
            mount.sort();
            let mut wanted = vec![
                "readonly".to_owned(),
                format!("source={source}"),
                format!("target={target}"),
                "type=bind".to_owned(),
            ];
            if source == "/image-exceptions.json" {
                wanted.push("from=image-review-inputs".to_owned());
            }
            wanted.sort();
            assert_eq!(mount, wanted, "{stage}");
            let tested = command.find(&format!("test -s {target}")).unwrap();
            assert!(tested < command.find("dnf upgrade").unwrap(), "{stage}");
            let copied = format!("(?m)^COPY .*{}", regex::escape(source));
            assert!(
                !regex::Regex::new(&copied).unwrap().is_match(body),
                "{stage}"
            );
        }
        for text in [
            "test -n \"${FEDORA_REFRESH_EPOCH}\"",
            "dnf upgrade -y --refresh",
            "dnf install -y",
        ] {
            assert!(command.contains(text), "{stage}: {text}");
        }
        checked.push(stage);
    }
    checked.sort();
    assert_eq!(checked, ["builder", "runtime-base"]);
}

/// Run the Dockerfile's own review projection with Node over private fixture
/// files and return the projected bytes.
fn project_image_reviews(input: &str) -> Vec<u8> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let recipe = checkout_file("Dockerfile");
    let source = recipe.split_once("<<'IMAGE_REVIEWS'\n").unwrap().1;
    let source = source.split_once("\nIMAGE_REVIEWS").unwrap().0;
    let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "image-review-projection-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&directory).unwrap();
    let (reviews, output) = (
        directory.join("reviews.json"),
        directory.join("projected.json"),
    );
    std::fs::write(&reviews, input).unwrap();
    let mut node = Command::new("node")
        .args(["--input-type=module", "-"])
        .args([&reviews, &output])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut program = node.stdin.take().unwrap();
    program.write_all(source.as_bytes()).unwrap();
    drop(program);
    let status = node.wait().unwrap();
    let projected = std::fs::read(&output);
    std::fs::remove_dir_all(&directory).unwrap();
    assert!(status.success(), "{status:?}");
    projected.unwrap()
}

/// The ledger document for `records`; `reversed` reverses the records and
/// each record's keys.
fn ledger(records: &[Value], reversed: bool) -> String {
    let record = |value: &Value| {
        let mut fields = value
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| format!("{}:{value}", Value::from(key.as_str())))
            .collect::<Vec<_>>();
        if reversed {
            fields.reverse();
        }
        format!("{{{}}}", fields.join(","))
    };
    let mut rows = records.iter().map(record).collect::<Vec<_>>();
    if reversed {
        rows.reverse();
    }
    format!(
        "{{\"schemaVersion\":1,\"exceptions\":[{}]}}",
        rows.join(",")
    )
}

/// From `test_image_review_projection_ignores_unrelated_reviews_and_preserves_complete_records`:
/// only the image audit's three review families affect Fedora package
/// refreshes.
#[test]
fn image_review_projection_ignores_unrelated_reviews_and_preserves_complete_records() {
    let exceptions: Value =
        serde_json::from_str(&checkout_file("security/exceptions.json")).unwrap();
    let records = exceptions["exceptions"].as_array().unwrap();
    let projected = project_image_reviews(&ledger(records, false));
    let selected: Value = serde_json::from_slice(&projected).unwrap();
    let sorted = |rows: Vec<&Value>| {
        let mut rows = rows.into_iter().map(Value::to_string).collect::<Vec<_>>();
        rows.sort();
        rows
    };
    let family = |row: &&Value| IMAGE_FAMILIES.iter().any(|name| row["scanner"] == *name);
    let expected = records.iter().filter(family).collect::<Vec<_>>();
    assert!(!expected.is_empty() && expected.len() < records.len());
    assert_eq!(
        sorted(selected["exceptions"].as_array().unwrap().iter().collect()),
        sorted(expected)
    );
    let mut moved = records.clone();
    for row in &mut moved {
        if row["scanner"] == "zizmor" {
            row["scope"] = format!("{}-moved", row["scope"].as_str().unwrap()).into();
        }
    }
    assert_ne!(&moved, records);
    assert_eq!(project_image_reviews(&ledger(&moved, false)), projected);
    assert_eq!(project_image_reviews(&ledger(records, true)), projected);
    for scanner in IMAGE_FAMILIES {
        let mut modified = records[0].clone();
        modified["scanner"] = scanner.into();
        let original = project_image_reviews(&ledger(std::slice::from_ref(&modified), false));
        let rationale = modified["rationale"].as_str().unwrap();
        modified["rationale"] = format!("{rationale} Updated review.").into();
        let updated = project_image_reviews(&ledger(std::slice::from_ref(&modified), false));
        assert_ne!(updated, original, "{scanner}");
    }
}

/// From `test_manual_load_image_keeps_bootstrap_and_artifact_contract`: the
/// manual owner follows the bootstrap and keeps the fixed cache and artifact
/// boundary.
#[test]
fn manual_load_image_keeps_bootstrap_and_artifact_contract() {
    let image = workflow("load-generator-image.yml");
    assert_eq!(image["jobs"]["image"]["timeout-minutes"], 45);
    let steps = steps(&image, "image");
    assert_eq!(steps.len(), 5);
    assert_eq!(steps[0]["with"]["persist-credentials"], false);
    assert_eq!(steps[1]["uses"], TOOLCHAIN_ACTION);
    assert_eq!(steps[1]["with"], json!({"toolchain": "1.99.0"}));
    assert!(lacks(&steps[1], &["if"]));
    assert!(uses(&steps[2]).starts_with("actions/cache/restore@"));
    assert_eq!(steps[3]["shell"], "bash");
    assert_eq!(
        steps[3]["run"],
        "cargo run --locked -q -p simplestchat-ci -- load-generator-image"
    );
    assert!(lacks(&steps[3], &["if"]));
    let upload = &steps[4]["with"];
    assert_eq!(upload["name"], "simplestchat-loadtest-${{ github.sha }}");
    assert_eq!(upload["path"], "${{ runner.temp }}/loadtest-image");
    assert_eq!(upload["if-no-files-found"], "error");
    assert_eq!(upload["compression-level"], 0);
    assert_eq!(upload["retention-days"], 7);
}

/// From `test_image_cache_keys_include_policy_and_operations_inputs`: policy
/// and operations inputs bind both image caches beside the Docker layer
/// checks. The broad fallback is safe only with the Dockerfile mount
/// dependency and the trust and architecture namespace.
#[test]
fn image_cache_keys_include_policy_and_operations_inputs() {
    let (ci, image) = (workflow("ci.yml"), workflow("load-generator-image.yml"));
    let named = [
        "Restore image layer cache",
        "Restore the production image layer cache",
    ];
    let caches = steps(&ci, "deployment")
        .iter()
        .chain(steps(&image, "image"))
        .filter(|step| uses(step).starts_with("actions/cache"))
        .filter(|step| !uses(step).starts_with("actions/cache/save@"))
        .filter(|step| named.iter().any(|name| step["name"] == *name))
        .collect::<Vec<_>>();
    assert_eq!(caches.len(), 2);
    for cache in caches {
        let key = cache["with"]["key"].as_str().unwrap();
        for source in [
            "security/exceptions.json",
            "security/image-policy.json",
            "ops/Cargo.toml",
            "ops/ops-build-inputs.lock.json",
            "ops/src/**",
        ] {
            assert!(key.contains(&format!("'{source}'")), "{source}");
        }
        let fallback = cache["with"]["restore-keys"].as_str().unwrap();
        assert!(fallback.starts_with("buildx-v3-"));
    }
}

/// From `test_image_cache_updates_application_layers_without_reusing_a_scan_verdict`:
/// source changes publish new immutable layers while every image check stays
/// required.
#[test]
fn image_cache_updates_application_layers_without_reusing_a_scan_verdict() {
    let ci = workflow("ci.yml");
    let steps = steps(&ci, "deployment");
    let restore = first(steps, |step| step["id"] == "production-cache");
    let key = restore["with"]["key"].as_str().unwrap();
    for source in ["src/**", "migrations/**", "web/src/**", ".dockerignore"] {
        assert!(key.contains(&format!("'{source}'")), "{source}");
    }
    let scan = first(steps, |step| {
        step["name"] == "Scan the same exported production image"
    });
    assert!(lacks(scan, &["if"]));
    assert!(run(scan).contains("--image-database-cache \"${RUNNER_TEMP}/image-database-cache\""));
}

/// The save condition and key of one job's refreshed-database save step.
fn database_save(steps: &[Value], producer: &str) {
    let save = first(steps, |step| {
        step["name"] == "Save refreshed vulnerability database download bytes"
    });
    assert_eq!(
        save["if"],
        format!(
            "${{{{ success() && (steps.image-database.outputs.cache-hit != 'true' || \
             steps.{producer}.outputs.key != steps.image-database-inputs.outputs.key) }}}}"
        )
    );
    assert_eq!(
        save["with"]["key"],
        format!("${{{{ steps.{producer}.outputs.key }}}}")
    );
}

/// From `test_database_preparation_overlaps_the_image_job_without_sharing_its_engine`:
/// a required independent job warms the bytes, and image freshness and scans
/// stay mandatory.
#[test]
fn database_preparation_overlaps_the_image_job_without_sharing_its_engine() {
    let ci = workflow("ci.yml");
    let needs = ci["jobs"]["required"]["needs"].as_array().unwrap();
    assert!(needs.iter().any(|need| need == "image-database"));
    for job in ["image-database", "deployment"] {
        assert!(lacks(&ci["jobs"][job], &["needs", "continue-on-error"]));
    }
    let preparation = steps(&ci, "image-database");
    let docker = first(preparation, |step| {
        step["name"] == "Start Docker inside the isolated local runner"
    });
    assert_eq!(docker["run"], "build/ci-local-docker.sh");
    assert_eq!(docker["if"], "${{ env.ACT }}");
    let prepare = first(preparation, |step| step["id"] == "prepared-image-database");
    assert!(lacks(prepare, &["if"]));
    for text in [
        "python3 build/security_image.py prepare-database",
        "--tools-directory \"${RUNNER_TEMP}/image-database-preparation/tools\"",
    ] {
        assert!(run(prepare).contains(text), "{text}");
    }
    assert!(!run(prepare).contains("--artifact-dir"));
    assert!(!run(prepare).contains("PRODUCTION_IMAGE"));
    let image = steps(&ci, "deployment");
    let (scanned, scan) = located(image, |step| step["id"] == "scanned-image-database");
    let (fixture, _) = located(image, |step| {
        step["name"] == "App-only release and rollback on a disposable host"
    });
    assert!(lacks(scan, &["if"]));
    assert!(fixture < scanned);
    for (current, producer, id, kind) in [
        (
            preparation,
            prepare,
            "prepared-image-database",
            "preparation",
        ),
        (image, scan, "scanned-image-database", "production"),
    ] {
        let fingerprint = first(current, |step| step["id"] == "image-database-inputs");
        assert_eq!(fingerprint["run"], HINT, "{kind}");
        assert_eq!(
            run(producer).lines().last().unwrap(),
            format!("cargo run --locked -q -p simplestchat-ci -- image-cache status {kind}")
        );
        database_save(current, id);
    }
}

/// From `test_database_cache_output_policy_preserves_guards_budgets_and_compiler_scope`:
/// the typed output glue keeps both workloads and their timing and guards.
#[test]
fn database_cache_output_policy_preserves_guards_budgets_and_compiler_scope() {
    let ci = workflow("ci.yml");
    for (job, producer, kind, minutes) in [
        (
            "image-database",
            "prepared-image-database",
            "preparation",
            20,
        ),
        ("deployment", "scanned-image-database", "production", 25),
    ] {
        let steps = steps(&ci, job);
        let bootstrap = &steps[1];
        assert_eq!(bootstrap["uses"], TOOLCHAIN_ACTION, "{job}");
        assert_eq!(bootstrap["with"]["toolchain"], "1.99.0", "{job}");
        assert!(lacks(bootstrap, &["if", "env"]), "{job}");
        let (hinted, hint) = located(steps, |step| step["id"] == "image-database-inputs");
        assert!(lacks(hint, &["if", "continue-on-error"]), "{job}");
        assert!(hinted > 1, "{job}");
        let producer = first(steps, |step| step["id"] == producer);
        assert!(lacks(producer, &["if", "continue-on-error"]), "{job}");
        assert_eq!(producer["timeout-minutes"], minutes, "{job}");
        assert_eq!(
            run(producer).lines().last().unwrap(),
            format!("cargo run --locked -q -p simplestchat-ci -- image-cache status {kind}")
        );
        assert!(!run(producer).contains("GITHUB_OUTPUT"), "{job}");
    }
    assert_eq!(ci["jobs"]["image-database"]["timeout-minutes"], 25);
}

/// From `test_image_build_exports_layers_only_when_the_primary_cache_is_missing`:
/// the native cache policy uses the same hit signal and completed-only save.
/// The cache arguments and failed-export preservation are covered by the
/// image owner's tests.
#[test]
fn image_build_exports_layers_only_when_the_primary_cache_is_missing() {
    let ci = workflow("ci.yml");
    let steps = steps(&ci, "deployment");
    let build = first(steps, |step| step["name"] == "Build production container");
    assert_eq!(
        build["env"]["CACHE_PRIMARY_HIT"],
        "${{ steps.production-cache.outputs.cache-hit }}"
    );
    let save = first(steps, |step| {
        step["name"] == "Save completed image layer cache"
    });
    assert_eq!(
        save["if"],
        "${{ success() && steps.production-cache.outputs.cache-hit != 'true' }}"
    );
    assert_eq!(build["run"], BUILD);
}

/// The restore step a split save inherits its validated key from.
fn inherited<'a>(steps: &'a [Value], save: &Value) -> &'a Value {
    let key = save["with"]["key"].as_str().unwrap();
    let actual = regex::Regex::new(
        r"^\$\{\{ steps\.((?:prepared|scanned)-image-database)\.outputs\.key \}\}$",
    )
    .unwrap();
    let restore = if actual.is_match(key) {
        // The online update may discover a newer generation than the first
        // descriptor named, and publishes that validated identity.
        first(steps, |step| step["id"] == "image-database")
    } else {
        let primary =
            regex::Regex::new(r"^\$\{\{ steps\.([a-zA-Z0-9_-]+)\.outputs\.cache-primary-key \}\}$")
                .unwrap();
        let id = primary.captures(key).unwrap()[1].to_owned();
        let restore = first(steps, |step| step["id"] == id.as_str());
        assert!(uses(restore).starts_with("actions/cache/restore@"));
        restore
    };
    assert!(restore["with"]["path"].is_string());
    assert_eq!(save["with"]["path"], restore["with"]["path"]);
    restore
}

/// From `test_build_caches_cannot_fall_back_across_trust_or_architecture`:
/// pull-request build artifacts stay outside the main compiler and layer
/// cache namespace. The rollover browser download cache is bound separately.
#[test]
fn build_caches_cannot_fall_back_across_trust_or_architecture() {
    let composite = document(".github/actions/native-toolchain/action.yml");
    let (ci, image) = (workflow("ci.yml"), workflow("load-generator-image.yml"));
    let sources = [
        composite["runs"]["steps"].as_array().unwrap().as_slice(),
        steps(&ci, "image-database"),
        steps(&ci, "deployment"),
        steps(&image, "image"),
    ];
    let rollover = [
        "Restore pinned rollover Chromium download bytes",
        "Save pinned rollover Chromium download bytes",
    ];
    let mut checked = 0;
    for steps in sources {
        for step in steps {
            assert!(step.is_object());
            let action = uses(step);
            let cache =
                action.starts_with("actions/cache") || action.starts_with("Swatinem/rust-cache");
            if !cache || rollover.iter().any(|name| step["name"] == *name) {
                continue;
            }
            let owner = if action.starts_with("actions/cache/save@") {
                inherited(steps, step)
            } else {
                step
            };
            let config = owner["with"].as_object().unwrap();
            let key = config.get("prefix-key").or_else(|| config.get("key"));
            let key = key.map_or("", |key| key.as_str().unwrap());
            checked += 1;
            if key == DATABASE_KEY {
                // Database bytes have no CPU architecture; the cache owner's
                // tests bind scanner format, trust and the actual generation.
                assert_eq!(
                    config["restore-keys"],
                    "${{ steps.image-database-inputs.outputs.prefix }}"
                );
                let fingerprint = first(steps, |step| step["id"] == "image-database-inputs");
                assert_eq!(fingerprint["run"], HINT);
                continue;
            }
            if action.starts_with("Swatinem/rust-cache@") {
                assert!(key.starts_with("v3-rust-"), "{key}");
                assert!(!config.contains_key("cache-targets"));
            }
            for text in [
                "github.event_name != 'pull_request'",
                "'main' || 'untrusted'",
                "runner.arch",
            ] {
                assert!(key.contains(text), "{action}: {text}");
            }
            if let Some(fallback) = config.get("restore-keys") {
                assert!(fallback.as_str().unwrap().contains("'main' || 'untrusted'"));
            }
        }
    }
    assert!(checked > 0);
}
