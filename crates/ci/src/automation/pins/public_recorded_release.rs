//! `ops/ansible/public.yml` refuses to replace the release a host records
//! unless the run names one. Not carried over from a Python module: the guard
//! and this pin arrived together.

use super::document;
use serde_json::{Value, json};

/// The image identities file the configuration run and every release write.
const RECORD: &str = "{{ scpub_config }}/images.json";

/// Exactly one task with `name`, and its position.
fn named<'a>(tasks: &'a [Value], name: &str) -> (usize, &'a Value) {
    let matches = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task["name"] == name)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "Expected exactly one {name:?} task");
    matches[0]
}

#[test]
fn public_configuration_refuses_to_replace_the_recorded_release_implicitly() {
    let playbook = document("ops/ansible/public.yml");
    let plays = playbook.as_array().unwrap();
    assert_eq!(plays.len(), 1);
    let pre_tasks = plays[0]["pre_tasks"].as_array().unwrap();
    // The guard is the last three pre-tasks, so no task changes the host
    // before it, and it follows the task that selects the image it compares.
    let first = pre_tasks.len() - 3;
    let guard = &pre_tasks[first..];
    let (selected, _) = named(pre_tasks, "Retain the verified application image selection");
    assert!(selected < first);
    assert_eq!(
        guard[0],
        json!({
            "name": "Look for the image identities an earlier run recorded",
            "ansible.builtin.stat": {"path": RECORD, "get_checksum": false},
            "register": "scpub_recorded_images_file",
        })
    );
    assert_eq!(
        guard[1],
        json!({
            "name": "Read the image identities an earlier run recorded",
            "ansible.builtin.slurp": {"src": RECORD},
            "register": "scpub_recorded_images",
            "when": "scpub_recorded_images_file.stat.exists",
        })
    );
    let recorded = "{{ scpub_recorded_images.content | b64decode | from_json }}";
    let refusal = "This host records release {{ scpub_recorded.revision }}; name the running or \
        a staged release with -e scpub_release_revision=<rev> -e scbench_revision=<rev> instead \
        of recording the provisioning image, which cannot serve a database migrated past it.";
    assert_eq!(
        guard[2],
        json!({
            "name": "Refuse to replace the recorded release with the host's own build",
            "vars": {"scpub_recorded": recorded},
            "ansible.builtin.assert": {
                "that": [
                    "scpub_recorded.revision == scbench_revision",
                    "scpub_recorded.serverImage == scpub_server_image",
                ],
                "fail_msg": refusal,
            },
            "when": [
                "scpub_release_revision is not defined",
                "scpub_recorded_images_file.stat.exists",
            ],
        })
    );
    // The guard reads the file and the two identities this run itself records.
    let tasks = plays[0]["tasks"].as_array().unwrap();
    let (_, record) = named(
        tasks,
        "Record reviewed source and image identities for deployment checks",
    );
    let copy = &record["ansible.builtin.copy"];
    assert_eq!(copy["dest"], RECORD);
    assert!(
        copy["content"]
            .as_str()
            .unwrap()
            .contains("'revision': scbench_revision, 'serverImage': scpub_server_image")
    );
}
