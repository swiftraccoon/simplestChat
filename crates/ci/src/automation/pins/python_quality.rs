//! Assertions moved from `ops/ansible/tests/test_python_quality.py`.

use super::{first, steps, workflow};

/// From `test_configuration_metadata_retention_never_uploads_command_logs`: in
/// each job the step whose `run` is exactly the CI binary's validation comes
/// before its metadata upload and never continues on error.
#[test]
fn configuration_validation_precedes_its_metadata_upload_in_each_job() {
    let ci = workflow("ci.yml");
    for (kind, job) in [("promtool", "automation"), ("caddy", "deployment")] {
        let steps = steps(&ci, job);
        let command = format!("cargo run --locked -q -p simplestchat-ci -- validate-config {kind}");
        let validate = first(steps, |step| step["run"] == command.as_str());
        let name = format!("Preserve {kind} validation metadata");
        let upload = first(steps, |step| step["name"] == name.as_str());
        let index = |wanted: &serde_json::Value| {
            steps
                .iter()
                .position(|step| std::ptr::eq(step, wanted))
                .unwrap()
        };
        assert!(index(validate) < index(upload), "{kind}");
        assert!(validate.get("continue-on-error").is_none(), "{kind}");
    }
}
