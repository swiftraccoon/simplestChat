//! The infrastructure security workflow's manual boot-only mode. It is a
//! diagnosis of the owned guest's boot and cannot stand in for the scheduled
//! signed validation.

use super::{run, steps, unique, workflow};

/// From `test_workflow_manual_mode_cannot_replace_scheduled_signed_validation`:
/// default and scheduled runs keep acquisition and full validation, and the
/// boot diagnosis receives no artifact.
#[test]
fn workflow_manual_mode_cannot_replace_scheduled_signed_validation() {
    let security = workflow("infrastructure-security.yml");
    let trigger = security["on"].as_object().unwrap();
    let option = &trigger["workflow_dispatch"]["inputs"]["boot_only"];
    assert_eq!(option["type"], "boolean");
    assert_eq!(option["default"], false);
    assert!(trigger.contains_key("schedule"));
    let name = security["jobs"]["infrastructure"]["name"].as_str().unwrap();
    assert!(name.contains("boot diagnostics"));
    let steps = steps(&security, "infrastructure");
    for name in [
        "Acquire and verify the exact signed main release",
        "Apply twice and verify host policy and isolated restore",
    ] {
        assert_eq!(unique(steps, name)["if"], "${{ !inputs.boot_only }}");
    }
    let boot = unique(steps, "Diagnose only the owned Debian guest boot");
    assert_eq!(
        boot["if"],
        "${{ github.event_name == 'workflow_dispatch' && inputs.boot_only }}"
    );
    assert!(run(boot).contains("--boot-only"));
    assert!(!run(boot).contains("--artifact-dir"));
}
