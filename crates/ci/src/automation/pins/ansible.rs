//! Ansible's own command line as the renderer and evaluator for the playbook
//! and template pins. Every generated play targets localhost over the local
//! connection inside a private scratch directory; nothing contacts a host and
//! no Python is imported.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// The checkout root.
pub(super) fn root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().unwrap()
}

/// One Ansible program: the checkout's controller environment when it exists,
/// otherwise the first match on `PATH`, which is where the hosted test job
/// installs the pinned controller requirements.
pub(super) fn ansible_tool(name: &str) -> PathBuf {
    let local = root().join("ops/ansible/.venv/bin").join(name);
    if local.is_file() {
        return local;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let found = std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file());
    found.unwrap_or_else(|| {
        panic!(
            "{name} is neither in ops/ansible/.venv/bin nor on PATH; \
             install ops/ansible/requirements.txt"
        )
    })
}

/// A private directory under the system temporary directory, removed on drop.
pub(super) struct Scratch(PathBuf);

impl Scratch {
    pub fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-ansible-pin-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run one Ansible program from the playbook directory with the repository's
/// configuration and all controller state confined to `scratch`.
pub(super) fn ansible<A: AsRef<OsStr>>(tool: &str, arguments: &[A], scratch: &Scratch) -> Output {
    let playbooks = root().join("ops/ansible");
    Command::new(ansible_tool(tool))
        .args(arguments)
        .current_dir(&playbooks)
        .env("ANSIBLE_CONFIG", playbooks.join("ansible.cfg"))
        .env("ANSIBLE_HOME", scratch.path().join("home"))
        .env("ANSIBLE_LOCAL_TEMP", scratch.path().join("local"))
        .env("ANSIBLE_NOCOLOR", "1")
        .env_remove("ANSIBLE_FORCE_COLOR")
        .output()
        .unwrap()
}

/// The output of a program that must have succeeded.
pub(super) fn succeeded(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}\n{errors}");
    text
}

/// Run `tasks` as one generated local play with `variables`, and return the
/// scratch directory its tasks wrote into, available to them as `scratch`.
pub(super) fn play(tasks: &Value, variables: &Value, vars_files: &[&str]) -> Scratch {
    let scratch = Scratch::new();
    let mut vars = variables.as_object().unwrap().clone();
    vars.insert("scratch".into(), scratch.path().to_str().unwrap().into());
    // Modules run under the controller's own interpreter, not a discovered one.
    vars.insert(
        "ansible_python_interpreter".into(),
        "{{ ansible_playbook_python }}".into(),
    );
    let files = vars_files.iter().map(|file| root().join(file));
    let book = json!([{
        "hosts": "localhost",
        "connection": "local",
        "gather_facts": false,
        "vars_files": files.collect::<Vec<_>>(),
        "vars": vars,
        "tasks": tasks,
    }]);
    // JSON is YAML, so the generated play needs no YAML writer.
    let path = scratch.path().join("play.json");
    std::fs::write(&path, serde_json::to_vec(&book).unwrap()).unwrap();
    let inventory: &OsStr = "localhost,".as_ref();
    let output = ansible(
        "ansible-playbook",
        &["-i".as_ref(), inventory, path.as_os_str()],
        &scratch,
    );
    succeeded(&output);
    scratch
}

/// Render templates from `ops/ansible/templates` with Ansible's template
/// module, as a deployment renders them, using `vars_file` and `overrides`.
pub(super) fn render(
    names: &[&str],
    vars_file: &str,
    overrides: &Value,
) -> BTreeMap<String, String> {
    let source = root().join("ops/ansible/templates");
    let tasks = json!([{
        "ansible.builtin.template": {
            "src": format!("{}/{{{{ item }}}}", source.to_str().unwrap()),
            "dest": "{{ scratch }}/{{ item }}",
            "mode": "0600",
        },
        "loop": names,
    }]);
    let scratch = play(&tasks, overrides, &[vars_file]);
    let rendered = |name: &&str| {
        let text = std::fs::read_to_string(scratch.path().join(name)).unwrap();
        ((*name).to_owned(), text)
    };
    names.iter().map(rendered).collect()
}

/// Evaluate each expression against each case's variables. The result is one
/// row per case with one value per expression, exactly as Ansible computed it.
pub(super) fn evaluate(expressions: &[&str], cases: &[Value]) -> Vec<Vec<Value>> {
    let mut tasks = Vec::new();
    for case in cases {
        let row = expressions
            .iter()
            .map(|expression| format!("({expression})"))
            .collect::<Vec<_>>()
            .join(", ");
        tasks.push(json!({
            "ansible.builtin.set_fact": {
                "rows": format!("{{{{ rows | default([]) + [[{row}]] }}}}"),
            },
            "vars": case,
        }));
    }
    tasks.push(json!({
        "ansible.builtin.copy": {
            "content": "{{ rows | to_json }}",
            "dest": "{{ scratch }}/rows.json",
            "mode": "0600",
        },
    }));
    let scratch = play(&Value::Array(tasks), &json!({}), &[]);
    let rows = std::fs::read(scratch.path().join("rows.json")).unwrap();
    let rows: Vec<Vec<Value>> = serde_json::from_slice(&rows).unwrap();
    assert_eq!(rows.len(), cases.len());
    rows
}
