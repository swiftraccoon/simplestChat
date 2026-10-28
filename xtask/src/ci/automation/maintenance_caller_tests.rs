//! Current native caller contracts, including passive staged-identity custody.
//! A Linux test executes fixed GNU tar extraction; no Ansible/Jinja, SSH,
//! authentication, candidate executable or maintenance runs here.
use serde_json::{Value, json};

type Check<T> = Result<T, &'static str>;
fn require(ok: bool, cause: &'static str) -> Check<()> {
    if ok { Ok(()) } else { Err(cause) }
}
fn parse(bytes: &[u8]) -> Check<Value> {
    use serde_saphyr::granit_parser::{Scanner, StrInput, TokenType};
    let text = std::str::from_utf8(bytes).map_err(|_| "utf8")?;
    require(bytes.len() <= 65536, "document-size")?;
    for token in Scanner::new(StrInput::new(text)) {
        let token = token.map_err(|_| "yaml-token")?;
        require(
            !matches!(
                token.token_type(),
                TokenType::Alias(_) | TokenType::Anchor(_) | TokenType::Tag(_, _)
            ),
            "yaml-indirection",
        )?;
    }
    let options = serde_saphyr::options! {
        duplicate_keys:serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys:serde_saphyr::MergeKeyPolicy::Error,reject_unsupported_tags:true,
        budget:serde_saphyr::budget! {
            max_reader_input_bytes:Some(65536),max_documents:1,max_depth:24,
            max_nodes:16384,max_events:32768,max_aliases:0,max_anchors:0,
            max_total_scalar_bytes:65536,max_merge_keys:0,
        },
    };
    serde_saphyr::from_slice_with_options(bytes, options).map_err(|_| "yaml")
}
fn sequence(value: &Value) -> Check<&[Value]> {
    value.as_array().map(Vec::as_slice).ok_or("sequence")
}
fn play(value: &Value) -> Check<&Value> {
    let values = sequence(value)?;
    require(values.len() == 1, "one-play")?;
    Ok(&values[0])
}
fn named<'a>(tasks: &'a [Value], name: &str) -> Check<(usize, &'a Value)> {
    let found = tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| t["name"] == name)
        .collect::<Vec<_>>();
    require(found.len() == 1, "one-task")?;
    Ok(found[0])
}
fn arguments(value: &Value) -> Check<Vec<&str>> {
    sequence(&value["ansible.builtin.command"]["argv"])?
        .iter()
        .map(|v| v.as_str().ok_or("argv-string"))
        .collect()
}
fn local(task: &Value) -> Check<()> {
    require(
        task["delegate_to"] == "localhost" && task["become"] == false,
        "controller-only",
    )
}
fn default_failure(task: &Value) -> Check<()> {
    require(
        task.get("failed_when").is_none()
            && task.get("ignore_errors").is_none()
            && task.get("ignore_unreachable").is_none()
            && task.get("async").is_none()
            && task.get("poll").is_none(),
        "actual-status",
    )
}
// Fixed expressions are compared after whitespace folding only. These tests do
// not interpret Jinja or turn a decoded predicate into authentication authority.
fn expression(value: &Value, expected: &str) -> bool {
    value.as_str().is_some_and(|text| {
        text.split_whitespace().collect::<Vec<_>>()
            == expected.split_whitespace().collect::<Vec<_>>()
    })
}
fn assertions(task: &Value, expected: &[&str]) -> Check<()> {
    let actual = sequence(&task["ansible.builtin.assert"]["that"])?;
    require(
        actual.len() == expected.len()
            && actual.iter().zip(expected).all(|(a, b)| expression(a, b)),
        "identity-assertions",
    )
}
fn stat(task: &Value, path: &str, register: &str) -> Check<()> {
    let module = &task["ansible.builtin.stat"];
    require(
        expression(&module["path"], path)
            && module["follow"] == false
            && module["get_checksum"] == true
            && module["checksum_algorithm"] == "sha256"
            && task["register"] == register,
        "original-stat",
    )
}
fn delivery_contract(auth: &[Value]) -> Check<()> {
    require(auth.len() == 32, "closed-delivery-actions")?;
    executable_bootstrap_contract(auth)?;
    for task in auth {
        default_failure(task)?;
        require(task.get("rescue").is_none(), "no-recovery-bypass")?;
        let expected = match task["name"].as_str() {
            Some("Read the user's gh token only when GH_TOKEN was not supplied") => {
                Some("lookup('ansible.builtin.env', 'GH_TOKEN') | length == 0")
            }
            Some("Require a private existing bootstrap parent") => {
                Some("scpub_ops_parent.stat.exists")
            }
            Some("Create only an absent fixed bootstrap parent") => {
                Some("not scpub_ops_parent.stat.exists")
            }
            _ => None,
        };
        require(
            match expected {
                None => task.get("when").is_none(),
                Some(condition) => task["when"] == condition,
            },
            "handoff-condition",
        )?;
    }
    let (_, paths) = named(
        auth,
        "Match the exact maintenance revision and trusted controller paths",
    )?;
    assertions(
        paths,
        &[
            "scpub_release_repository is string",
            "scpub_release_ci_run is integer",
            "scpub_release_ci_run > 0",
            "scpub_ops_selection.repository == scpub_release_repository",
            "scpub_ops_selection.buildRunId == scpub_release_ci_run",
            "scpub_ops_selection.revision == scpub_release_revision",
            "scpub_ops_selection.revision == scpub_release_expected_revision",
            "scpub_ops_xtask is match('^/')",
            "scpub_ops_gh_executable is match('^/')",
            "scpub_ops_gh_executable | realpath | basename == 'gh'",
        ],
    )?;
    let (temporary_at, temporary) = named(auth, "Create a private controller evidence directory")?;
    local(temporary)?;
    require(
        temporary["ansible.builtin.tempfile"]
            == json!({
                "state":"directory", "prefix":"simplestchat-native-maintenance-"
            })
            && temporary["register"] == "scpub_ops_local",
        "private-controller",
    )?;
    let (paths_at, paths) = named(auth, "Select this operation's private paths")?;
    let facts = &paths["ansible.builtin.set_fact"];
    require(
        temporary_at < paths_at
            && facts["cacheable"] == false
            && facts["scpub_ops_controller"] == "{{ scpub_ops_local.path | realpath }}"
            && facts["scpub_ops_gh"] == "{{ scpub_ops_gh_executable | realpath }}"
            && expression(
                &facts["scpub_ops_nonce"],
                concat!(
                    "{{ lookup('ansible.builtin.password', '/dev/null', length=32, ",
                    "chars=['digits', 'abcdef']) }}"
                ),
            ),
        "operation-paths",
    )?;
    let (old_at, old) = named(
        auth,
        "Snapshot fixed canonical bytes as expected-old custody only",
    )?;
    stat(
        old,
        "/usr/local/libexec/simplestchat-public/simplestchat-ops",
        "scpub_ops_old",
    )?;
    let (serialize_at, request) = named(
        auth,
        "Serialize one original request for the strict native decoder",
    )?;
    require(
        paths_at < old_at
            && old_at < serialize_at
            && request["ansible.builtin.set_fact"]["cacheable"] == false
            && expression(
                &request["ansible.builtin.set_fact"]["scpub_ops_request"],
                concat!(
                    "{{ { 'schemaVersion': 1, 'selection': scpub_ops_selection, ",
                    "'artifactId': scpub_ops_artifact_id, ",
                    "'output': scpub_ops_controller ~ '/export', 'authentication': 'gh-token', ",
                    "'remoteInput': '/usr/local/libexec/simplestchat-ops-bootstrap/' ~ ",
                    "scpub_ops_nonce ~ '/input' ",
                    "} | combine({'installedBinary': { 'sha256': scpub_ops_old.stat.checksum, ",
                    "'bytes': scpub_ops_old.stat.size }} ",
                    "if scpub_ops_old.stat.exists else {}) | to_json }}"
                ),
            ),
        "typed-original-request",
    )?;
    let (write_at, write) = named(auth, "Retain the original controller request privately")?;
    local(write)?;
    require(
        serialize_at < write_at
            && write["ansible.builtin.copy"]
                == json!({
                    "content":"{{ scpub_ops_request }}",
                    "dest":"{{ scpub_ops_controller }}/export-request.json", "mode":"0600"
                }),
        "private-original-request",
    )?;
    let (credential_at, credential) = named(
        auth,
        "Read the user's gh token only when GH_TOKEN was not supplied",
    )?;
    let (_, export) = named(
        auth,
        "Authenticate and export the exact signed pair through Rust",
    )?;
    require(
        write_at < credential_at
            && credential["register"] == "scpub_ops_credential"
            && export["environment"]["PATH"] == "{{ scpub_ops_gh | dirname }}:/usr/bin:/bin"
            && expression(
                &export["environment"]["GH_TOKEN"],
                concat!(
                    "{{ lookup('ansible.builtin.env', 'GH_TOKEN') or ",
                    "scpub_ops_credential.stdout | trim }}"
                ),
            ),
        "local-token-flow",
    )?;
    let (parent_at, parent) = named(
        auth,
        "Refuse an unknown bootstrap parent without repairing it",
    )?;
    require(
        parent["ansible.builtin.stat"]
            == json!({
                "path":"/usr/local/libexec/simplestchat-ops-bootstrap",
                "follow":false, "get_checksum":false
            })
            && parent["register"] == "scpub_ops_parent",
        "bootstrap-stat",
    )?;
    let (parent_check_at, _) = named(auth, "Require a private existing bootstrap parent")?;
    let (parent_create_at, create) = named(auth, "Create only an absent fixed bootstrap parent")?;
    require(
        parent_at < parent_check_at
            && parent_check_at < parent_create_at
            && arguments(create)?
                == [
                    "/usr/bin/mkdir",
                    "--mode=0700",
                    "--",
                    "/usr/local/libexec/simplestchat-ops-bootstrap",
                ],
        "absent-parent-only",
    )?;
    let (nonce_at, _) = named(
        auth,
        "Exclusively create this nonce and its private empty input directory",
    )?;
    let (copy_at, copy) = named(
        auth,
        "Copy only the native export's tar and original installer request",
    )?;
    require(
        parent_create_at < nonce_at
            && nonce_at < copy_at
            && copy["ansible.builtin.copy"]
                == json!({
                    "src":"{{ scpub_ops_controller }}/export/{{ item }}",
                    "dest":"{{ scpub_ops_remote }}/{{ item }}", "owner":"root", "group":"root",
                    "mode":"0600", "force":false
                })
            && copy["loop"] == json!(["ops-pair.tar", "install-request.json"]),
        "fresh-transfer",
    )?;
    let (tar_stat_at, tar_stat) = named(
        auth,
        "Join the actual received tar before the trusted parser runs",
    )?;
    stat(
        tar_stat,
        "{{ scpub_ops_remote }}/ops-pair.tar",
        "scpub_ops_tar",
    )?;
    let (tar_join_at, _) = named(
        auth,
        "Require the exact Rust-produced three-member transport",
    )?;
    let (extract_at, _) = named(
        auth,
        "Extract only the three fixed members into the fresh private directory",
    )?;
    let (candidate_stat_at, candidate_stat) = named(
        auth,
        "Join the candidate bytes before their first execution",
    )?;
    stat(
        candidate_stat,
        "{{ scpub_ops_remote }}/input/simplestchat-ops",
        "scpub_ops_candidate",
    )?;
    let (candidate_join_at, _) = named(auth, "Require the native authenticated candidate pin")?;
    let (chmod_at, chmod) = named(auth, "Make only this candidate executable")?;
    let (install_at, install) = named(
        auth,
        "Install or upgrade through the authenticated candidate and native checks",
    )?;
    let (_, retained) = named(
        auth,
        "Record the bounded private evidence retained for this operation",
    )?;
    require(
        retained.get("ansible.builtin.debug").is_some(),
        "retained-evidence-only",
    )?;
    require(
        copy_at < tar_stat_at
            && tar_stat_at < tar_join_at
            && tar_join_at < extract_at
            && extract_at < candidate_stat_at
            && candidate_stat_at < candidate_join_at
            && candidate_join_at < chmod_at
            && chmod_at < install_at
            && chmod["ansible.builtin.file"]
                == json!({
                    "path":"{{ scpub_ops_remote }}/input/simplestchat-ops",
                    "mode":"0755", "follow":false
                })
            && install["environment"]["PATH"] == "/usr/sbin:/usr/bin:/sbin:/bin",
        "first-execution-custody",
    )
}

fn executable_bootstrap_contract(auth: &[Value]) -> Check<()> {
    let (selection_at, _) = named(
        auth,
        "Match the exact maintenance revision and trusted controller paths",
    )?;
    let (parent_at, parent) = named(
        auth,
        "Inspect the fixed executable bootstrap ancestor without following links",
    )?;
    require(
        parent["ansible.builtin.stat"]
            == json!({"path":"/usr/local/libexec", "follow":false, "get_checksum":false})
            && parent["register"] == "scpub_ops_exec_parent",
        "executable-ancestor-stat",
    )?;
    let (profile_at, profile) = named(
        auth,
        "Require the protected existing executable bootstrap ancestor",
    )?;
    assertions(
        profile,
        &[
            "scpub_ops_exec_parent.stat.isdir | default(false)",
            "not (scpub_ops_exec_parent.stat.islnk | default(false))",
            "scpub_ops_exec_parent.stat.uid == 0",
            "scpub_ops_exec_parent.stat.gid == 0",
            "scpub_ops_exec_parent.stat.mode == '0755'",
        ],
    )?;
    let (mount_at, mount) = named(
        auth,
        "Read the fixed executable bootstrap ancestor mount options",
    )?;
    require(
        arguments(mount)?
            == [
                "/usr/bin/findmnt",
                "--noheadings",
                "--raw",
                "--output",
                "OPTIONS",
                "--target",
                "/usr/local/libexec",
            ]
            && mount["register"] == "scpub_ops_exec_mount"
            && mount["changed_when"] == false
            && mount["timeout"] == 15,
        "executable-mount-observation",
    )?;
    let (admission_at, admission) = named(
        auth,
        "Require one executable bootstrap mount without changing mount policy",
    )?;
    assertions(
        admission,
        &[
            "scpub_ops_exec_mount.stdout_lines | length == 1",
            "scpub_ops_exec_mount.stdout == (scpub_ops_exec_mount.stdout | trim)",
            r"scpub_ops_exec_mount.stdout is match('^[^\\s,]+(,[^\\s,]+)*$')",
            "'noexec' not in scpub_ops_exec_mount.stdout.split(',')",
        ],
    )?;
    let (temporary_at, _) = named(auth, "Create a private controller evidence directory")?;
    require(
        selection_at == 0
            && parent_at == selection_at + 1
            && profile_at == parent_at + 1
            && mount_at == profile_at + 1
            && admission_at == mount_at + 1
            && admission_at < temporary_at,
        "executable-preflight-before-export",
    )
}
fn rendering_contract(tasks: &[Value]) -> Check<()> {
    let (image_at, image) = named(
        tasks,
        "Read the selected image identity for the candidate configuration",
    )?;
    require(
        image["ansible.builtin.slurp"]["src"] == "{{ scpub_config }}/images.json"
            && image["register"] == "scpub_selected_images",
        "selected-image",
    )?;
    let (secret_at, secret) = named(
        tasks,
        "Read protected secrets for private configuration rendering",
    )?;
    require(
        image_at < secret_at
            && secret["ansible.builtin.slurp"]["src"] == "{{ scpub_config }}/secrets.json"
            && secret["register"] == "scpub_secret_file",
        "selected-secret",
    )?;
    let (facts_at, facts) = named(
        tasks,
        "Keep secret values out of logs and persistent fact caches",
    )?;
    require(
        secret_at < facts_at
            && facts["ansible.builtin.set_fact"]
                == json!({
                    "scpub_secrets":"{{ scpub_secret_file.content | b64decode | from_json }}",
                    "scpub_server_image":concat!(
                        "{{ (scpub_selected_images.content | b64decode | from_json)",
                        ".serverImage }}"
                    ),
                    "cacheable":false
                }),
        "selected-render-values",
    )?;
    let (relay_at, relay) = named(
        tasks,
        "Read the managed relay secret for candidate rendering",
    )?;
    let (relay_facts_at, relay_facts) = named(tasks, "Retain the relay secret only in memory")?;
    let (candidate_at, _) = named(
        tasks,
        "Render the candidate application configuration from the host's facts",
    )?;
    require(
        facts_at < relay_at
            && relay_at < relay_facts_at
            && relay_facts_at < candidate_at
            && relay["ansible.builtin.slurp"]["src"] == "/etc/simplestchat-turn/secret"
            && relay["register"] == "scpub_maintenance_turn_secret"
            && relay["when"] == "scpub_turn_enabled | bool"
            && relay["no_log"] == true
            && relay_facts["when"] == "scpub_turn_enabled | bool"
            && relay_facts["no_log"] == true
            && relay_facts["ansible.builtin.set_fact"]
                == json!({
                    "scpub_turn_secret":concat!(
                        "{{ scpub_maintenance_turn_secret.content ",
                        "| b64decode }}"
                    ),
                    "cacheable":false
                }),
        "relay-memory",
    )?;
    let (proxy_at, proxy) = named(
        tasks,
        "Render only the explicitly requested browser asset proxy transition",
    )?;
    require(
        proxy["when"] == "scpub_browser_assets_upgrade | default(false) | bool"
            && proxy.get("rescue").is_none()
            && proxy_at < tasks.len() - 1,
        "proxy-scope",
    )?;
    let block = sequence(&proxy["block"])?;
    require(block.len() == 4, "proxy-actions")?;
    for task in block {
        default_failure(task)?;
        require(task.get("when").is_none(), "proxy-child")?;
    }
    require(
        block[0]["ansible.builtin.set_fact"]
            == json!({
                "scpub_postgres_image":concat!(
                    "{{ (scpub_selected_images.content | b64decode | from_json)",
                    ".postgresImage }}"
                ),
                "scpub_caddy_image":concat!(
                    "{{ (scpub_selected_images.content | b64decode | from_json)",
                    ".caddyImage }}"
                ),
                "cacheable":false
            }),
        "proxy-images",
    )?;
    private_template(
        &block[1],
        "public-compose.yml.j2",
        "{{ scpub_config }}/compose.public.yml.candidate",
    )?;
    require(
        block[2]["ansible.builtin.copy"]
            == json!({
                "src":"{{ playbook_dir }}/../../Caddyfile",
                "dest":"{{ scpub_config }}/Caddyfile.candidate",
                "owner":"root", "group":"root", "mode":"0600"
            })
            && block[3]["ansible.builtin.import_tasks"] == "tasks/browser-assets.yml"
            && block[3]["vars"]
                == json!({
                    "scpub_browser_assets_revision":concat!(
                        "{{ (scpub_selected_images.content | b64decode | from_json)",
                        ".revision }}"
                    ),
                    "scpub_browser_assets_record_installed":false
                }),
        "proxy-storage",
    )
}
fn private_template(task: &Value, source: &str, destination: &str) -> Check<()> {
    let template = &task["ansible.builtin.template"];
    require(
        template["src"] == source
            && template["dest"] == destination
            && template["owner"] == "root"
            && template["group"] == "root"
            && template["mode"] == "0600",
        "private-template",
    )
}
fn current_contract(document: &Value, source_document: &Value) -> Check<()> {
    let p = play(document)?;
    require(
        p["hosts"] == "benchmark_hosts"
            && p["become"] == true
            && p["gather_facts"] == true
            && p["serial"] == 1,
        "host-context",
    )?;
    require(
        p["vars"]["scbench_revision"] == "{{ scpub_release_revision }}",
        "source-revision",
    )?;
    let tasks = sequence(&p["tasks"])?;
    rendering_contract(tasks)?;
    require(
        tasks[0]["ansible.builtin.import_tasks"] == "tasks/source.yml",
        "source-first",
    )?;
    let source = sequence(source_document)?;
    require(
        source.len() == 2
            && source[0]["ansible.builtin.git"]["force"] == false
            && source[0]["ansible.builtin.git"]["recursive"] == false
            && source[0]["ansible.builtin.git"]["accept_hostkey"] == false
            && source[0]["ansible.builtin.git"]["version"] == "{{ scbench_revision }}",
        "source-keeper",
    )?;
    require(
        arguments(&source[1])? == ["git", "rev-parse", "HEAD"]
            && source[1]["failed_when"] == "scbench_source_commit.stdout != scbench_revision",
        "source-identity",
    )?;
    let (secret_at, secret) = named(
        tasks,
        "Read protected secrets for private configuration rendering",
    )?;
    require(secret["no_log"] == true, "secret-read")?;
    let (_, facts) = named(
        tasks,
        "Keep secret values out of logs and persistent fact caches",
    )?;
    require(
        facts["no_log"] == true
            && facts["ansible.builtin.set_fact"]["cacheable"] == false
            && facts["ansible.builtin.set_fact"]
                .get("scpub_secrets")
                .is_some(),
        "secret-facts",
    )?;
    let (candidate_at, candidate) = named(
        tasks,
        "Render the candidate application configuration from the host's facts",
    )?;
    private_template(
        candidate,
        "public-app.env.j2",
        "{{ scpub_config }}/app.env.candidate",
    )?;
    require(
        candidate["no_log"] == true && secret_at < candidate_at,
        "candidate-secret-order",
    )?;
    let (grants_at, grants) = named(
        tasks,
        "Render the runtime grants the launcher applies after migrating",
    )?;
    private_template(
        grants,
        "public-runtime-grants.sql.j2",
        "{{ scpub_config }}/runtime-grants.sql",
    )?;
    require(candidate_at < grants_at, "grants-order")?;

    let pre = sequence(&p["pre_tasks"])?;
    let (_, delivery) = named(
        pre,
        "Deliver the signed current Ops executable through native policy",
    )?;
    require(
        delivery["when"] == "not ansible_check_mode" && delivery.get("rescue").is_none(),
        "delivery-scope",
    )?;
    let auth = sequence(&delivery["block"])?;
    delivery_contract(auth)?;
    staged_identity_contract(auth)?;
    let (credential_at, credential) = named(
        auth,
        "Read the user's gh token only when GH_TOKEN was not supplied",
    )?;
    local(credential)?;
    default_failure(credential)?;
    require(
        credential["no_log"] == true
            && credential["timeout"] == 30
            && arguments(credential)?
                == [
                    "/usr/bin/env",
                    "-u",
                    "GH_TOKEN",
                    "-u",
                    "GITHUB_TOKEN",
                    "{{ scpub_ops_gh }}",
                    "auth",
                    "token",
                    "--hostname",
                    "github.com",
                ],
        "credential",
    )?;
    let (export_at, export) = named(
        auth,
        "Authenticate and export the exact signed pair through Rust",
    )?;
    local(export)?;
    default_failure(export)?;
    require(
        export["no_log"] == true
            && export["timeout"] == 1920
            && export["register"] == "scpub_ops_export"
            && export["environment"]["GITHUB_TOKEN"] == ""
            && arguments(export)?
                == [
                    "{{ scpub_ops_xtask }}",
                    "ops-artifact",
                    "export",
                    "--request",
                    "{{ scpub_ops_controller }}/export-request.json",
                    "--request-sha256",
                    "{{ scpub_ops_request | hash('sha256') }}",
                ],
        "native-export",
    )?;
    let (receipt_at, receipt) = named(
        auth,
        "Read the current receipt only after actual native command success",
    )?;
    require(
        credential_at < export_at
            && export_at < receipt_at
            && receipt["ansible.builtin.set_fact"]
                == json!({
                    "scpub_ops_receipt":"{{ scpub_ops_export.stdout | from_json }}",
                    "scpub_ops_remote":concat!(
                        "/usr/local/libexec/simplestchat-ops-bootstrap/",
                        "{{ scpub_ops_nonce }}"
                    ),
                    "cacheable":false
                }),
        "rc-before-json",
    )?;
    let (_, base) = named(auth, "Require a private existing bootstrap parent")?;
    require(
        base["when"] == "scpub_ops_parent.stat.exists"
            && base["ansible.builtin.assert"]["that"]
                == json!([
                    "scpub_ops_parent.stat.isdir and not scpub_ops_parent.stat.islnk",
                    "scpub_ops_parent.stat.uid == 0 and scpub_ops_parent.stat.gid == 0",
                    "scpub_ops_parent.stat.mode == '0700'"
                ]),
        "bootstrap-parent",
    )?;
    let (_, mkdir) = named(
        auth,
        "Exclusively create this nonce and its private empty input directory",
    )?;
    require(
        arguments(mkdir)? == ["/usr/bin/mkdir", "--mode=0700", "--", "{{ item }}"]
            && mkdir["loop"] == json!(["{{ scpub_ops_remote }}", "{{ scpub_ops_remote }}/input"]),
        "exclusive-nonce",
    )?;
    let (tar_join_at, tar_join) = named(
        auth,
        "Require the exact Rust-produced three-member transport",
    )?;
    require(
        tar_join["ansible.builtin.assert"]["that"]
            == json!([
                "scpub_ops_tar.stat.size == scpub_ops_receipt.transfer.bytes",
                "scpub_ops_tar.stat.checksum == scpub_ops_receipt.transfer.sha256"
            ]),
        "tar-byte-join",
    )?;
    let (extract_at, extract) = named(
        auth,
        "Extract only the three fixed members into the fresh private directory",
    )?;
    require(
        tar_join_at < extract_at
            && arguments(extract)?
                == [
                    "/usr/bin/tar",
                    "--extract",
                    "--file",
                    "{{ scpub_ops_remote }}/ops-pair.tar",
                    "--directory",
                    "{{ scpub_ops_remote }}/input",
                    "--keep-old-files",
                    "--no-same-owner",
                    "--no-same-permissions",
                    "--no-wildcards",
                    "--",
                    "ops-release.json",
                    "ops-selection.json",
                    "simplestchat-ops",
                ],
        "fixed-extract",
    )?;
    let (candidate_join_at, join) = named(auth, "Require the native authenticated candidate pin")?;
    require(
        extract_at < candidate_join_at
            && join["ansible.builtin.assert"]["that"]
                == json!([
                    "scpub_ops_candidate.stat.size == scpub_ops_receipt.binary.bytes",
                    "scpub_ops_candidate.stat.checksum == scpub_ops_receipt.binary.sha256"
                ]),
        "candidate-byte-join",
    )?;
    let (install_at, install) = named(
        auth,
        "Install or upgrade through the authenticated candidate and native checks",
    )?;
    default_failure(install)?;
    require(
        candidate_join_at < install_at
            && install["timeout"] == 150
            && install["environment"]["GH_TOKEN"] == ""
            && install["environment"]["GITHUB_TOKEN"] == ""
            && arguments(install)?
                == [
                    "{{ scpub_ops_remote }}/input/simplestchat-ops",
                    "distribution",
                    "{{ scpub_ops_receipt.operation }}",
                    "--request",
                    "{{ scpub_ops_remote }}/install-request.json",
                    "--request-sha256",
                    "{{ scpub_ops_receipt.installerRequest.sha256 }}",
                ],
        "native-install",
    )?;
    let (_, clear) = named(
        sequence(&delivery["always"])?,
        "Clear the noncached token-bearing command result",
    )?;
    require(
        sequence(&delivery["always"])?.len() == 1,
        "only-token-finalizer",
    )?;
    require(
        clear["no_log"] == true
            && clear["ansible.builtin.set_fact"]["cacheable"] == false
            && clear["ansible.builtin.set_fact"]["scpub_ops_credential"] == json!({}),
        "token-retirement",
    )?;
    let last = tasks.last().ok_or("last-task")?;
    default_failure(last)?;
    require(
        grants_at < tasks.len() - 1
            && last["when"] == "not ansible_check_mode"
            && last["environment"]["PATH"] == "/usr/sbin:/usr/bin:/sbin:/bin"
            && last["environment"]["GH_TOKEN"] == ""
            && last["environment"]["GITHUB_TOKEN"] == ""
            && last["vars"]["ansible_python_interpreter"] == "/usr/bin/python3",
        "native-context",
    )?;
    let argv = arguments(last)?;
    require(
        argv.len() == 18
            && argv[0] == "systemd-run"
            && argv[1].contains("simplestchat-maintenance-")
            && argv[1].contains("scpub_ops_nonce")
            && argv[2..6]
                == [
                    "--property=Type=exec",
                    "--property=RuntimeMaxSec=1800",
                    "--property=TimeoutStopSec=240",
                    "--wait",
                ]
            && argv[6..12]
                == [
                    "/usr/local/libexec/simplestchat-public/simplestchat-ops",
                    "release",
                    "maintain",
                    "--revision",
                    "{{ scpub_release_revision }}",
                    "--candidate-env",
                ],
        "canonical-maintain",
    )?;
    require(
        argv[12] == "{{ scpub_config }}/app.env.candidate"
            && argv[13].contains("--browser-assets-upgrade")
            && argv[13].contains("--no-browser-assets-upgrade")
            && argv[14..] == ["--timeout-seconds", "1800", "--finalization-seconds", "60"],
        "native-bounds",
    )?;
    Ok(())
}

fn original_documents() -> (Value, Value) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let caller = std::fs::read(root.join("ops/ansible/maintenance.yml")).unwrap();
    let source = std::fs::read(root.join("ops/ansible/tasks/source.yml")).unwrap();
    assert_eq!(
        caller,
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ops/ansible/maintenance.yml"
        ))
    );
    assert_eq!(
        source,
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ops/ansible/tasks/source.yml"
        ))
    );
    (parse(&caller).unwrap(), parse(&source).unwrap())
}
fn auth_task_mut<'a>(document: &'a mut Value, name: &str) -> &'a mut Value {
    let pre = sequence(&document[0]["pre_tasks"]).unwrap();
    let (delivery, block) = named(
        pre,
        "Deliver the signed current Ops executable through native policy",
    )
    .unwrap();
    let (task, _) = named(sequence(&block["block"]).unwrap(), name).unwrap();
    &mut document[0]["pre_tasks"][delivery]["block"][task]
}
#[test]
fn current_native_caller_preserves_source_privacy_transport_and_owner_contracts() {
    let (caller, source) = original_documents();
    current_contract(&caller, &source).unwrap();
}
#[test]
fn auth_status_token_and_original_byte_handoff_mutations_are_refused() {
    let (caller, source) = original_documents();
    for (name, field, replacement) in [
        (
            "Authenticate and export the exact signed pair through Rust",
            "failed_when",
            json!(false),
        ),
        (
            "Read the user's gh token only when GH_TOKEN was not supplied",
            "no_log",
            json!(false),
        ),
        (
            "Install or upgrade through the authenticated candidate and native checks",
            "ignore_errors",
            json!(true),
        ),
    ] {
        let mut changed = caller.clone();
        auth_task_mut(&mut changed, name)[field] = replacement;
        assert!(
            current_contract(&changed, &source).is_err(),
            "{name}: {field}"
        );
    }
    let mut missing_tar_hash = caller.clone();
    auth_task_mut(
        &mut missing_tar_hash,
        "Require the exact Rust-produced three-member transport",
    )["ansible.builtin.assert"]["that"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(current_contract(&missing_tar_hash, &source).is_err());
    let mut wrong_request = caller;
    auth_task_mut(
        &mut wrong_request,
        "Install or upgrade through the authenticated candidate and native checks",
    )["ansible.builtin.command"]["argv"][6] = "{{ scpub_ops_receipt.binary.sha256 }}".into();
    assert!(current_contract(&wrong_request, &source).is_err());
}
#[test]
fn candidate_custody_must_precede_candidate_execution() {
    let (caller, source) = original_documents();
    let mut changed = caller.clone();
    let pre = sequence(&changed[0]["pre_tasks"]).unwrap();
    let (delivery, block) = named(
        pre,
        "Deliver the signed current Ops executable through native policy",
    )
    .unwrap();
    let auth = sequence(&block["block"]).unwrap();
    let (extract, _) = named(
        auth,
        "Extract only the three fixed members into the fresh private directory",
    )
    .unwrap();
    let (join, _) = named(auth, "Require the native authenticated candidate pin").unwrap();
    changed[0]["pre_tasks"][delivery]["block"]
        .as_array_mut()
        .unwrap()
        .swap(extract, join);
    assert!(current_contract(&changed, &source).is_err());
    let mut changed = caller;
    auth_task_mut(
        &mut changed,
        "Require the native authenticated candidate pin",
    )["ansible.builtin.assert"]["that"] = json!([]);
    assert!(current_contract(&changed, &source).is_err());
}
#[test]
fn native_command_source_force_and_deadline_regressions_are_refused() {
    let (caller, source) = original_documents();
    for (index, value) in [(6, "/usr/bin/python3"), (15, "1801"), (17, "0")] {
        let mut changed = caller.clone();
        let last = changed[0]["tasks"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap();
        last["ansible.builtin.command"]["argv"][index] = value.into();
        assert!(current_contract(&changed, &source).is_err(), "{index}");
    }
    let mut changed = source;
    changed[0]["ansible.builtin.git"]["force"] = true.into();
    assert!(current_contract(&caller, &changed).is_err());
}

const PREDICATE: &str =
    "{{ scpub_root }}/releases/{{ scpub_release_revision }}/release-predicate.json";
const MANIFEST: &str = "{{ scpub_root }}/releases/{{ scpub_release_revision }}/release.json";
const VERIFIED_PREDICATE: &str =
    "{{ (scpub_validated_release.stdout | from_json).directory }}/release-predicate.json";
const PREDICATE_CUSTODY_TASKS: [&str; 7] = [
    "Snapshot the original verified controller predicate",
    "Snapshot the actual published signed application manifest",
    "Inspect any existing retained application predicate without following links",
    "Require original staged bytes and an absent or exact retained predicate",
    "Copy only the original verified predicate without replacing existing evidence",
    "Recheck the actual transferred predicate before staging can succeed",
    "Require the retained original predicate bytes and private profile",
];

fn staged_identity_contract(auth: &[Value]) -> Check<()> {
    let (paths_at, _) = named(
        auth,
        "Match the exact maintenance revision and trusted controller paths",
    )?;
    let (predicate_at, predicate) =
        named(auth, "Snapshot the retained signed application predicate")?;
    stat(predicate, PREDICATE, "scpub_app_predicate_file")?;
    let (manifest_at, manifest) = named(auth, "Snapshot the actual staged application manifest")?;
    stat(manifest, MANIFEST, "scpub_app_manifest_file")?;
    let (profile_at, profile) = named(
        auth,
        "Require protected original application identity files",
    )?;
    assertions(
        profile,
        &[
            "scpub_app_predicate_file.stat.isreg | default(false)",
            "not (scpub_app_predicate_file.stat.islnk | default(false))",
            "scpub_app_predicate_file.stat.uid == 0",
            "scpub_app_predicate_file.stat.gid == 0",
            "scpub_app_predicate_file.stat.mode == '0600'",
            "scpub_app_predicate_file.stat.nlink == 1",
            "scpub_app_predicate_file.stat.size > 0",
            "scpub_app_predicate_file.stat.size <= 67108864",
            "scpub_app_predicate_file.stat.checksum is match('^[a-f0-9]{64}$')",
            "scpub_app_manifest_file.stat.isreg | default(false)",
            "not (scpub_app_manifest_file.stat.islnk | default(false))",
            "scpub_app_manifest_file.stat.uid == 0",
            "scpub_app_manifest_file.stat.gid == 0",
            "scpub_app_manifest_file.stat.mode == '0600'",
            "scpub_app_manifest_file.stat.checksum is match('^[a-f0-9]{64}$')",
        ],
    )?;
    let (read_at, read) = named(auth, "Read the original retained application predicate")?;
    require(
        read["ansible.builtin.slurp"]["src"] == PREDICATE
            && read["register"] == "scpub_app_predicate_bytes"
            && read["no_log"] == true,
        "predicate-read",
    )?;
    let (decode_at, decode) = named(
        auth,
        "Decode the retained predicate only for passive identity joins",
    )?;
    require(
        decode["no_log"] == true
            && decode["ansible.builtin.set_fact"]
                == json!({
                    "scpub_app_predicate":concat!(
                        "{{ scpub_app_predicate_bytes.content ",
                        "| b64decode | from_json }}"
                    ),
                    "cacheable":false
                }),
        "passive-predicate",
    )?;
    let (join_at, join) = named(
        auth,
        "Join staged application bytes and exact Ops selection before export",
    )?;
    assertions(
        join,
        &[
            concat!(
                "scpub_app_predicate_bytes.content | b64decode | hash('sha256') == ",
                "scpub_app_predicate_file.stat.checksum"
            ),
            "scpub_app_predicate is mapping",
            "scpub_app_predicate.repository == scpub_ops_selection.repository",
            "scpub_app_predicate.revision == scpub_ops_selection.revision",
            "scpub_app_predicate.runId is integer",
            "scpub_app_predicate.runId == scpub_ops_selection.buildRunId",
            "scpub_app_predicate.runAttempt is integer",
            "scpub_app_predicate.runAttempt == scpub_ops_selection.runAttempt",
            concat!(
                "scpub_app_predicate.fileDigests['release.json'] == ",
                "scpub_app_manifest_file.stat.checksum"
            ),
        ],
    )?;
    let (private_at, _) = named(auth, "Create a private controller evidence directory")?;
    let (export_at, _) = named(
        auth,
        "Authenticate and export the exact signed pair through Rust",
    )?;
    require(
        paths_at < predicate_at
            && predicate_at < manifest_at
            && manifest_at < profile_at
            && profile_at < read_at
            && read_at < decode_at
            && decode_at < join_at
            && join_at < private_at
            && private_at < export_at,
        "staged-identity-order",
    )
}

fn release_contract(document: &Value) -> Check<()> {
    let p = play(document)?;
    require(
        p["hosts"] == "benchmark_hosts" && p["become"] == true && p["serial"] == 1,
        "release-context",
    )?;
    let (_, verify) = named(
        sequence(&p["pre_tasks"])?,
        "Verify the signed release on the controller before any remote preflight",
    )?;
    local(verify)?;
    default_failure(verify)?;
    require(
        verify["register"] == "scpub_validated_release"
            && verify["check_mode"] == false
            && verify["timeout"] == 660
            && arguments(verify)?.get(2)
                == Some(&"{{ playbook_dir }}/../../build/verify-release.py"),
        "current-verifier",
    )?;
    let tasks = sequence(&p["tasks"])?;
    let (fetch_at, fetch) = named(
        tasks,
        "Verify the exact GitHub release and fetch it directly onto the prepared host",
    )?;
    local(fetch)?;
    default_failure(fetch)?;
    require(
        arguments(fetch)?.get(2) == Some(&"{{ playbook_dir }}/../../build/fetch-release.py")
            && fetch["when"]
                == json!([
                    "not ansible_check_mode",
                    "scpub_release_artifact_id is defined"
                ]),
        "signed-fetch",
    )?;
    let (stage_at, stage) = named(
        tasks,
        "Import and verify the image while the public service remains running",
    )?;
    default_failure(stage)?;
    require(
        stage["when"] == "not ansible_check_mode" && arguments(stage)?.get(9) == Some(&"stage"),
        "stage-success",
    )?;
    let (retain_at, _) = named(tasks, PREDICATE_CUSTODY_TASKS[0])?;
    let end = retain_at
        .checked_add(PREDICATE_CUSTODY_TASKS.len())
        .ok_or("retention-range")?;
    let block = tasks.get(retain_at..end).ok_or("retention-range")?;
    require(
        fetch_at < stage_at
            && stage_at + 1 == retain_at
            && block
                .iter()
                .zip(PREDICATE_CUSTODY_TASKS)
                .all(|(task, name)| task["name"] == name),
        "retention-after-success",
    )?;
    for task in block {
        default_failure(task)?;
        require(
            task["when"] == "not ansible_check_mode"
                && task.get("block").is_none()
                && task.get("rescue").is_none()
                && task.get("always").is_none(),
            "retention-condition",
        )?;
    }
    stat(
        &block[0],
        VERIFIED_PREDICATE,
        "scpub_release_predicate_source",
    )?;
    local(&block[0])?;
    stat(&block[1], MANIFEST, "scpub_release_published_manifest")?;
    stat(&block[2], PREDICATE, "scpub_release_predicate_existing")?;
    assertions(
        &block[3],
        &[
            "scpub_release_predicate_source.stat.isreg | default(false)",
            "not (scpub_release_predicate_source.stat.islnk | default(false))",
            "scpub_release_predicate_source.stat.nlink == 1",
            "scpub_release_predicate_source.stat.size > 0",
            "scpub_release_predicate_source.stat.size <= 67108864",
            "scpub_release_predicate_source.stat.checksum is match('^[a-f0-9]{64}$')",
            "scpub_release_published_manifest.stat.isreg | default(false)",
            "not (scpub_release_published_manifest.stat.islnk | default(false))",
            "scpub_release_published_manifest.stat.uid == 0",
            "scpub_release_published_manifest.stat.gid == 0",
            "scpub_release_published_manifest.stat.mode == '0600'",
            concat!(
                "scpub_release_published_manifest.stat.checksum == ",
                "(scpub_validated_release.stdout | from_json)['release.json']"
            ),
            concat!(
                "not scpub_release_predicate_existing.stat.exists or ",
                "(scpub_release_predicate_existing.stat.isreg | default(false) and ",
                "not (scpub_release_predicate_existing.stat.islnk | default(false)) and ",
                "scpub_release_predicate_existing.stat.uid == 0 and ",
                "scpub_release_predicate_existing.stat.gid == 0 and ",
                "scpub_release_predicate_existing.stat.mode == '0600' and ",
                "scpub_release_predicate_existing.stat.nlink == 1 and ",
                "scpub_release_predicate_existing.stat.size == ",
                "scpub_release_predicate_source.stat.size and ",
                "scpub_release_predicate_existing.stat.checksum == ",
                "scpub_release_predicate_source.stat.checksum)"
            ),
        ],
    )?;
    require(
        block[4]["ansible.builtin.copy"]
            == json!({
                "src":VERIFIED_PREDICATE, "dest":PREDICATE, "owner":"root", "group":"root",
                "mode":"0600", "follow":false, "local_follow":false, "force":false
            }),
        "retained-original-copy",
    )?;
    stat(&block[5], PREDICATE, "scpub_release_predicate_retained")?;
    assertions(
        &block[6],
        &[
            "scpub_release_predicate_retained.stat.isreg | default(false)",
            "not (scpub_release_predicate_retained.stat.islnk | default(false))",
            "scpub_release_predicate_retained.stat.uid == 0",
            "scpub_release_predicate_retained.stat.gid == 0",
            "scpub_release_predicate_retained.stat.mode == '0600'",
            "scpub_release_predicate_retained.stat.nlink == 1",
            concat!(
                "scpub_release_predicate_retained.stat.size == ",
                "scpub_release_predicate_source.stat.size"
            ),
            concat!(
                "scpub_release_predicate_retained.stat.checksum == ",
                "scpub_release_predicate_source.stat.checksum"
            ),
        ],
    )
}

fn original_release() -> Value {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let bytes = std::fs::read(root.join("ops/ansible/release.yml")).unwrap();
    assert_eq!(
        bytes,
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ops/ansible/release.yml"
        ))
    );
    parse(&bytes).unwrap()
}
fn task_mut<'a>(document: &'a mut Value, section: &str, name: &str) -> &'a mut Value {
    let (at, _) = named(sequence(&document[0][section]).unwrap(), name).unwrap();
    &mut document[0][section][at]
}

#[test]
fn staged_predicate_retention_follows_actual_signed_fetch_and_stage_success() {
    release_contract(&original_release()).unwrap();
}
#[test]
fn every_passive_identity_profile_and_byte_join_is_required() {
    let (caller, source) = original_documents();
    for name in [
        "Match the exact maintenance revision and trusted controller paths",
        "Require protected original application identity files",
        "Join staged application bytes and exact Ops selection before export",
    ] {
        let count = auth_task_mut(&mut caller.clone(), name)["ansible.builtin.assert"]["that"]
            .as_array()
            .unwrap()
            .len();
        for at in 0..count {
            let mut changed = caller.clone();
            auth_task_mut(&mut changed, name)["ansible.builtin.assert"]["that"]
                .as_array_mut()
                .unwrap()
                .remove(at);
            assert!(current_contract(&changed, &source).is_err(), "{name}: {at}");
            let mut bypassed = caller.clone();
            auth_task_mut(&mut bypassed, name)["ansible.builtin.assert"]["that"][at] =
                "true".into();
            assert!(
                current_contract(&bypassed, &source).is_err(),
                "{name}: bypass {at}"
            );
        }
    }
    let mut changed = caller;
    auth_task_mut(
        &mut changed,
        "Join staged application bytes and exact Ops selection before export",
    )["ansible.builtin.assert"]["that"][0] = "scpub_app_predicate.passed == true".into();
    assert!(current_contract(&changed, &source).is_err());
}
#[test]
fn identity_checks_and_executable_transition_cannot_be_reordered_or_skipped() {
    let (caller, source) = original_documents();
    for (left, right) in [
        (
            "Join staged application bytes and exact Ops selection before export",
            "Authenticate and export the exact signed pair through Rust",
        ),
        (
            "Require the native authenticated candidate pin",
            "Make only this candidate executable",
        ),
    ] {
        let mut changed = caller.clone();
        let delivery = task_mut(
            &mut changed,
            "pre_tasks",
            "Deliver the signed current Ops executable through native policy",
        );
        let tasks = sequence(&delivery["block"]).unwrap();
        let a = named(tasks, left).unwrap().0;
        let b = named(tasks, right).unwrap().0;
        delivery["block"].as_array_mut().unwrap().swap(a, b);
        assert!(current_contract(&changed, &source).is_err(), "{left}");
    }
    for name in [
        "Join staged application bytes and exact Ops selection before export",
        "Require the exact Rust-produced three-member transport",
    ] {
        let mut changed = caller.clone();
        auth_task_mut(&mut changed, name)["when"] = false.into();
        assert!(current_contract(&changed, &source).is_err());
    }
}
#[test]
fn retained_stage_evidence_requires_exact_original_bytes_and_completed_staging() {
    let original = original_release();
    for (at, field, value) in [
        (0, "follow", json!(true)),
        (4, "force", json!(true)),
        (4, "src", json!("/tmp/assumed-verified.json")),
        (5, "checksum_algorithm", json!("sha1")),
    ] {
        let mut changed = original.clone();
        let retain = task_mut(&mut changed, "tasks", PREDICATE_CUSTODY_TASKS[at]);
        let module = if at == 4 {
            "ansible.builtin.copy"
        } else {
            "ansible.builtin.stat"
        };
        retain[module][field] = value;
        assert!(release_contract(&changed).is_err(), "{at}: {field}");
    }
    let mut changed = original.clone();
    task_mut(
        &mut changed,
        "tasks",
        "Import and verify the image while the public service remains running",
    )["failed_when"] = false.into();
    assert!(release_contract(&changed).is_err());
    let mut changed = original.clone();
    let tasks = sequence(&changed[0]["tasks"]).unwrap();
    let stage = named(
        tasks,
        "Import and verify the image while the public service remains running",
    )
    .unwrap()
    .0;
    let retain = named(tasks, PREDICATE_CUSTODY_TASKS[0]).unwrap().0;
    changed[0]["tasks"]
        .as_array_mut()
        .unwrap()
        .swap(stage, retain);
    assert!(release_contract(&changed).is_err());
    for at in [3, 6] {
        let mut changed = original.clone();
        let retain = task_mut(&mut changed, "tasks", PREDICATE_CUSTODY_TASKS[at]);
        retain["ansible.builtin.assert"]["that"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert!(release_contract(&changed).is_err());
    }
    for name in PREDICATE_CUSTODY_TASKS {
        let mut changed = original.clone();
        task_mut(&mut changed, "tasks", name)["when"] = false.into();
        assert!(release_contract(&changed).is_err(), "{name}");
    }
    let mut changed = original;
    let at = named(
        sequence(&changed[0]["tasks"]).unwrap(),
        PREDICATE_CUSTODY_TASKS[0],
    )
    .unwrap()
    .0;
    let tasks = changed[0]["tasks"].as_array_mut().unwrap();
    let retained = tasks
        .drain(at..at + PREDICATE_CUSTODY_TASKS.len())
        .collect::<Vec<_>>();
    tasks.insert(
        at,
        json!({
            "name":"Retain the original verified application predicate after successful staging",
            "when":"not ansible_check_mode", "block":retained
        }),
    );
    assert!(release_contract(&changed).is_err());
}
#[test]
fn request_types_private_rendering_and_fresh_transport_cannot_be_weakened() {
    let (caller, source) = original_documents();
    for (name, module, field, value) in [
        (
            "Join the actual received tar before the trusted parser runs",
            "ansible.builtin.stat",
            "follow",
            json!(true),
        ),
        (
            "Copy only the native export's tar and original installer request",
            "ansible.builtin.copy",
            "force",
            json!(true),
        ),
        (
            "Make only this candidate executable",
            "ansible.builtin.file",
            "path",
            json!("/usr/local/bin/unpinned"),
        ),
    ] {
        let mut changed = caller.clone();
        auth_task_mut(&mut changed, name)[module][field] = value;
        assert!(
            current_contract(&changed, &source).is_err(),
            "{name}: {field}"
        );
    }
    let mut changed = caller.clone();
    let request = &mut auth_task_mut(
        &mut changed,
        "Serialize one original request for the strict native decoder",
    )["ansible.builtin.set_fact"]["scpub_ops_request"];
    let original = request.as_str().unwrap();
    let lossy = original.replace(
        "'artifactId': scpub_ops_artifact_id,",
        "'artifactId': scpub_ops_artifact_id | int,",
    );
    assert_ne!(lossy, original);
    *request = lossy.into();
    assert!(current_contract(&changed, &source).is_err());
    let mut changed = caller.clone();
    let delivery = task_mut(
        &mut changed,
        "pre_tasks",
        "Deliver the signed current Ops executable through native policy",
    );
    delivery["block"].as_array_mut().unwrap().push(json!({
        "name":"Delete unknown bootstrap state",
        "ansible.builtin.file":{
            "path":"/usr/local/libexec/simplestchat-ops-bootstrap", "state":"absent"
        }
    }));
    assert!(current_contract(&changed, &source).is_err());
    for (name, field, value) in [
        (
            "Retain the relay secret only in memory",
            "no_log",
            json!(false),
        ),
        (
            "Render only the explicitly requested browser asset proxy transition",
            "when",
            json!(true),
        ),
    ] {
        let mut changed = caller.clone();
        task_mut(&mut changed, "tasks", name)[field] = value;
        assert!(
            current_contract(&changed, &source).is_err(),
            "{name}: {field}"
        );
    }
}
#[test]
fn caller_yaml_rejects_duplicate_keys_aliases_tags_and_extra_documents() {
    for bytes in [
        b"- hosts: one\n  hosts: two\n".as_slice(),
        b"- &a {name: first}\n- *a\n".as_slice(),
        b"- !!str 3\n".as_slice(),
        b"---\n- name: one\n---\n- name: two\n".as_slice(),
    ] {
        assert!(parse(bytes).is_err());
    }
}

fn reference_context_contract(text: &str) -> Check<()> {
    let rules = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    require(rules.first() == Some(&"**"), "context-deny-default")?;
    let ancestors = [
        "xtask",
        "xtask/src",
        "xtask/src/ci",
        "xtask/src/ci/automation",
    ];
    let mut previous = 0;
    for ancestor in ancestors {
        let directory = format!("!{ancestor}/");
        let deny = format!("{ancestor}/**");
        let positions = rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| **rule == directory)
            .map(|(at, _)| at)
            .collect::<Vec<_>>();
        require(positions.len() == 1, "context-directory")?;
        let at = positions[0];
        require(
            at > previous && rules.get(at + 1) == Some(&deny.as_str()),
            "context-reexclude",
        )?;
        previous = at;
    }
    let reference = "xtask/src/ci/automation/reference";
    let expected = [
        "!xtask/src/ci/automation/reference/",
        "xtask/src/ci/automation/reference/**",
        "!xtask/src/ci/automation/reference/maintenance-python.yml",
        "!xtask/src/ci/automation/reference/source-python.yml",
    ];
    let positions = rules
        .windows(expected.len())
        .enumerate()
        .filter(|(_, block)| *block == expected)
        .map(|(at, _)| at)
        .collect::<Vec<_>>();
    require(
        positions.len() == 1 && positions[0] > previous,
        "context-exact-reference-block",
    )?;
    let mut admitted = std::collections::BTreeSet::new();
    for rule in &rules {
        let Some(pattern) = rule.strip_prefix('!') else {
            continue;
        };
        let pattern = pattern.trim_start_matches('/');
        let root = pattern.split('/').next().ok_or("context-root")?;
        require(!root.contains(['*', '?', '[']), "context-global-wildcard")?;
        if root != "xtask" {
            continue;
        }
        require(admitted.insert(pattern), "context-duplicate-admission")?;
        if pattern.starts_with(reference) {
            require(
                expected
                    .iter()
                    .filter_map(|rule| rule.strip_prefix('!'))
                    .any(|allowed| allowed == pattern),
                "context-reference-whitelist",
            )?;
        }
        require(
            !pattern.contains(['*', '?', '[']) || pattern.ends_with("/*.rs"),
            "context-data-wildcard",
        )?;
        if ancestors.contains(&pattern.trim_end_matches('/')) {
            require(pattern.ends_with('/'), "context-ancestor-shape")?;
        }
    }
    Ok(())
}

#[test]
fn docker_context_admits_only_the_two_fixed_legacy_yaml_inputs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let bytes = std::fs::read(root.join(".dockerignore")).unwrap();
    assert_eq!(
        bytes,
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../.dockerignore"))
    );
    let text = std::str::from_utf8(&bytes).unwrap();
    reference_context_contract(text).unwrap();
    for admission in [
        "!xtask/src/ci/automation/reference/*.yml",
        "!xtask/src/ci/automation/reference/**",
        "!xtask/src/ci/automation/reference/private.env",
        "!xtask/src/ci/automation/**",
        "!xtask/**",
        "!**/*.yaml",
    ] {
        assert!(
            reference_context_contract(&format!("{text}\n{admission}\n")).is_err(),
            "{admission}"
        );
    }
    let missing_reexclude = text.replace("xtask/src/ci/automation/reference/**\n", "");
    assert_ne!(missing_reexclude, text);
    assert!(reference_context_contract(&missing_reexclude).is_err());
    let unlisted = text.replace(
        "!xtask/src/ci/automation/reference/source-python.yml",
        "!xtask/src/ci/automation/reference/other.yaml",
    );
    assert_ne!(unlisted, text);
    assert!(reference_context_contract(&unlisted).is_err());
}

#[test]
fn executable_bootstrap_preflight_and_fixed_namespace_cannot_be_bypassed() {
    let (caller, source) = original_documents();
    assert!(current_contract(&caller, &source).is_ok());
    let names = [
        "Inspect the fixed executable bootstrap ancestor without following links",
        "Require the protected existing executable bootstrap ancestor",
        "Read the fixed executable bootstrap ancestor mount options",
        "Require one executable bootstrap mount without changing mount policy",
    ];
    for name in names {
        for (field, value) in [
            ("when", json!(false)),
            ("ignore_errors", json!(true)),
            ("failed_when", json!(false)),
        ] {
            let mut changed = caller.clone();
            auth_task_mut(&mut changed, name)[field] = value;
            assert!(
                current_contract(&changed, &source).is_err(),
                "{name}: {field}"
            );
        }
        let mut changed = caller.clone();
        let delivery = task_mut(
            &mut changed,
            "pre_tasks",
            "Deliver the signed current Ops executable through native policy",
        );
        let tasks = delivery["block"].as_array_mut().unwrap();
        let (at, _) = named(tasks, name).unwrap();
        tasks.remove(at);
        assert!(
            current_contract(&changed, &source).is_err(),
            "missing {name}"
        );
    }
    for (name, count) in [(names[1], 5), (names[3], 4)] {
        for index in 0..count {
            let mut changed = caller.clone();
            auth_task_mut(&mut changed, name)["ansible.builtin.assert"]["that"][index] =
                json!(true);
            assert!(
                current_contract(&changed, &source).is_err(),
                "{name}: {index}"
            );
        }
    }
    for argv in [
        json!([
            "/usr/bin/findmnt",
            "--output",
            "TARGET",
            "/usr/local/libexec"
        ]),
        json!([
            "/usr/bin/findmnt",
            "--output",
            "OPTIONS",
            "--target",
            "/run"
        ]),
        json!(["/usr/bin/mount", "-o", "remount,exec", "/run"]),
    ] {
        let mut changed = caller.clone();
        auth_task_mut(&mut changed, names[2])["ansible.builtin.command"]["argv"] = argv;
        assert!(current_contract(&changed, &source).is_err());
    }
    let mut changed = caller.clone();
    auth_task_mut(&mut changed, names[0])["ansible.builtin.stat"]["follow"] = json!(true);
    assert!(current_contract(&changed, &source).is_err());
    let mut changed = caller.clone();
    let delivery = task_mut(
        &mut changed,
        "pre_tasks",
        "Deliver the signed current Ops executable through native policy",
    );
    let tasks = delivery["block"].as_array_mut().unwrap();
    let (admission, _) = named(tasks, names[3]).unwrap();
    let (export, _) = named(
        tasks,
        "Authenticate and export the exact signed pair through Rust",
    )
    .unwrap();
    tasks.swap(admission, export);
    assert!(current_contract(&changed, &source).is_err());
    let mut changed = caller.clone();
    auth_task_mut(
        &mut changed,
        "Read the current receipt only after actual native command success",
    )["ansible.builtin.set_fact"]["scpub_ops_remote"] =
        json!("/run/simplestchat-ops-bootstrap/{{ scpub_ops_nonce }}");
    assert!(current_contract(&changed, &source).is_err());
}

/// Exercise GNU tar itself; structural argv equality cannot detect conflicting flags.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn gnu_tar_fixed_transport_extracts_and_refuses_existing_members() {
    use simplestchat_ops::owned_process::{Group, exited_now, finish_before, reserved};
    use std::{
        fs,
        io::Write,
        os::unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
            process::CommandExt,
        },
        process::{Command, ExitStatus, Stdio},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    // One interval includes YAML parsing, fixture preparation and both owned children.
    let end = Instant::now() + Duration::from_secs(30);
    let body = end - Duration::from_secs(5);
    async fn invoke(arguments: &[std::ffi::OsString], body: Instant, end: Instant) -> ExitStatus {
        assert!(Instant::now() < body, "original tar body deadline");
        let child = Command::new(&arguments[0])
            .args(&arguments[1..])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut group = Group::new(child);
        let pid = group.pid();
        let observed = loop {
            match exited_now(pid) {
                Ok(true) => break true,
                Ok(false) if Instant::now() < body => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                _ => break false,
            }
        };
        // Do not timeout/drop a disarmed wait: settle this exact reservation first.
        let retired = finish_before(pid, end).await;
        let reaped = group.reap().await;
        assert!(observed && retired && !reserved(pid));
        let status = reaped.unwrap();
        assert!(Instant::now() < end, "original tar cleanup deadline");
        status
    }

    let (caller, _) = original_documents();
    let (_, delivery) = named(
        sequence(&caller[0]["pre_tasks"]).unwrap(),
        "Deliver the signed current Ops executable through native policy",
    )
    .unwrap();
    let (_, extract) = named(
        sequence(&delivery["block"]).unwrap(),
        "Extract only the three fixed members into the fresh private directory",
    )
    .unwrap();
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "simplestchat-maintenance-tar-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let input = root.join("input");
    fs::DirBuilder::new().mode(0o700).create(&input).unwrap();
    let archive = root.join("ops-pair.tar");
    let members: [(&str, &[u8]); 3] = [
        ("ops-release.json", b"{\"fixture\":\"manifest\"}\n"),
        ("ops-selection.json", b"{\"fixture\":\"selection\"}\n"),
        ("simplestchat-ops", b"INERT_BINARY_BYTES\0\xff"),
    ];
    let mut tar = tar::Builder::new(Vec::new());
    for (name, bytes) in members {
        let mut header = tar::Header::new_ustar();
        header.set_mode(0o600);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(bytes.len() as u64);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes).unwrap();
    }
    let bytes = tar.into_inner().unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&archive)
        .unwrap();
    file.write_all(&bytes).unwrap();
    nix::unistd::close(file).unwrap();
    let mut replaced = [0, 0];
    let arguments = arguments(extract)
        .unwrap()
        .into_iter()
        .map(|argument| match argument {
            "{{ scpub_ops_remote }}/ops-pair.tar" => {
                replaced[0] += 1;
                archive.clone().into_os_string()
            }
            "{{ scpub_ops_remote }}/input" => {
                replaced[1] += 1;
                input.clone().into_os_string()
            }
            literal => std::ffi::OsString::from(literal),
        })
        .collect::<Vec<_>>();
    assert_eq!(replaced, [1, 1]);
    assert_eq!(arguments[0], "/usr/bin/tar");
    let first = invoke(&arguments, body, end).await;
    let first_files = members.map(|(name, expected)| {
        let path = input.join(name);
        let metadata = fs::symlink_metadata(&path);
        let data = fs::read(&path);
        (name, expected, metadata, data)
    });
    // This independent edit makes a silent overwrite observable, rather than
    // accepting equal archive bytes as evidence that keep-old-files worked.
    fs::write(input.join("ops-release.json"), b"EXISTING_PRIVATE_SENTINEL").unwrap();
    let before = members.map(|(name, _)| {
        let path = input.join(name);
        let metadata = fs::symlink_metadata(&path).unwrap();
        (
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            fs::read(path).unwrap(),
        )
    });
    let second = invoke(&arguments, body, end).await;
    let after = members.map(|(name, _)| {
        let path = input.join(name);
        let metadata = fs::symlink_metadata(&path).unwrap();
        (
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            fs::read(path).unwrap(),
        )
    });
    let mut names = fs::read_dir(&input)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    // Remove only this fixture's fixed files; unknown additions refuse cleanup.
    for (name, _) in members {
        fs::remove_file(input.join(name)).unwrap();
    }
    fs::remove_file(archive).unwrap();
    fs::remove_dir(input).unwrap();
    fs::remove_dir(root).unwrap();
    assert_eq!(first.code(), Some(0));
    for (_, expected, metadata, data) in first_files {
        let metadata = metadata.unwrap();
        assert!(metadata.is_file() && metadata.nlink() == 1);
        assert_eq!(metadata.mode() & 0o7777, 0o600);
        assert_eq!(data.unwrap(), expected);
    }
    assert_eq!(
        names,
        members.map(|(name, _)| std::ffi::OsString::from(name))
    );
    assert_eq!(second.code(), Some(2));
    assert_eq!(before, after);
    assert!(Instant::now() < end);
}
