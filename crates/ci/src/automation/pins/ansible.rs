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

/// The inventory name of the fixture host that assertion plays target.
const FIXTURE_HOST: &str = "_fixture";

/// The checkout root.
pub(super) fn root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().unwrap()
}

/// One Ansible program: the checkout's controller environment when it exists,
/// otherwise the first match on `PATH`, which is where the hosted test job
/// installs the pinned controller requirements.
pub(crate) fn ansible_tool(name: &str) -> PathBuf {
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
    run(&book, "localhost,", &[], &scratch);
    scratch
}

/// Run one generated playbook against `inventory`, with `options` added to
/// Ansible's command line, and require it to succeed.
fn run(book: &Value, inventory: &str, options: &[&str], scratch: &Scratch) {
    run_from(scratch.path(), book, inventory, options, scratch);
}

/// Run one generated playbook written into `directory`, which is then the
/// play's `playbook_dir`, as `run` does.
fn run_from(directory: &Path, book: &Value, inventory: &str, options: &[&str], scratch: &Scratch) {
    // JSON is YAML, so the generated play needs no YAML writer.
    let path = directory.join("play.json");
    std::fs::write(&path, serde_json::to_vec(book).unwrap()).unwrap();
    let mut arguments: Vec<&OsStr> = vec!["-i".as_ref(), inventory.as_ref()];
    arguments.extend(options.iter().map(OsStr::new));
    arguments.push(path.as_os_str());
    succeeded(&ansible("ansible-playbook", &arguments, scratch));
}

/// Render templates from `ops/ansible/templates` with Ansible's template
/// module, as a deployment renders them, using `vars_file` and `overrides`.
pub(super) fn render(
    names: &[&str],
    vars_file: &str,
    overrides: &Value,
) -> BTreeMap<String, String> {
    rendered(names, &[vars_file], overrides)
}

/// Render templates with exactly `variables`, as a host that has those
/// values renders them; a value a template needs and is not given fails.
pub(super) fn render_with(names: &[&str], variables: &Value) -> BTreeMap<String, String> {
    rendered(names, &[], variables)
}

fn rendered(names: &[&str], vars_files: &[&str], overrides: &Value) -> BTreeMap<String, String> {
    let source = root().join("ops/ansible/templates");
    let tasks = json!([{
        "ansible.builtin.template": {
            "src": format!("{}/{{{{ item }}}}", source.to_str().unwrap()),
            "dest": "{{ scratch }}/{{ item }}",
            "mode": "0600",
        },
        "loop": names,
    }]);
    let scratch = play(&tasks, overrides, vars_files);
    let rendered = |name: &&str| {
        let text = std::fs::read_to_string(scratch.path().join(name)).unwrap();
        ((*name).to_owned(), text)
    };
    names.iter().map(rendered).collect()
}

/// Run the assert module with `rules` once per case and report which cases it
/// accepted. An undefined variable, or a rule that cannot be evaluated,
/// refuses the case, as it would on a host.
pub(super) fn asserted(rules: &Value, cases: &[Value]) -> Vec<bool> {
    asserted_on(&[FIXTURE_HOST], &[], rules, cases).remove(0)
}

/// Run the assert module with `rules` once per case on each of `hosts`, as a
/// run that selected exactly those hosts with `options` (such as `--check`)
/// on Ansible's command line, and report each host's verdicts in the order of
/// `hosts`. Ansible itself sets the play's hosts and the check mode, which a
/// case's variables cannot replace.
pub(super) fn asserted_on(
    hosts: &[&str],
    options: &[&str],
    rules: &Value,
    cases: &[Value],
) -> Vec<Vec<bool>> {
    let mut tasks = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        tasks.push(json!({
            "ansible.builtin.assert": {"that": rules, "quiet": true},
            "vars": case,
            "register": format!("verdict_{index}"),
            "ignore_errors": true,
        }));
    }
    let verdicts = (0..cases.len())
        .map(|index| format!("verdict_{index} is success"))
        .collect::<Vec<_>>()
        .join(", ");
    let scratch = Scratch::new();
    // Each host writes its own verdicts, in a check run as well.
    tasks.push(json!({
        "ansible.builtin.copy": {
            "content": format!("{{{{ [{verdicts}] | to_json }}}}"),
            "dest": scratch.path().join("{{ inventory_hostname }}.json"),
            "mode": "0600",
        },
        "check_mode": false,
        "delegate_to": "localhost",
        "vars": {
            "ansible_connection": "local",
            "ansible_python_interpreter": "{{ ansible_playbook_python }}",
        },
    }));
    // The play targets named hosts over the default transport, because a
    // local play defines `ansible_connection` and `ansible_host` itself and
    // would decide rules about them. Assertions run on the controller and
    // never connect. No host's name is a valid address, so a case is
    // accepted only on the values it supplies and those Ansible sets.
    let book = json!([{"hosts": "all", "gather_facts": false, "tasks": tasks}]);
    let inventory: String = hosts.iter().map(|host| format!("{host},")).collect();
    run(&book, &inventory, options, &scratch);
    let verdicts = |host: &&str| {
        let verdicts = std::fs::read(scratch.path().join(format!("{host}.json"))).unwrap();
        let verdicts: Vec<bool> = serde_json::from_slice(&verdicts).unwrap();
        assert_eq!(verdicts.len(), cases.len(), "{host}");
        verdicts
    };
    hosts.iter().map(verdicts).collect()
}

/// What one case made of a task sequence that Ansible ran as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Every task passed or was skipped.
    Passed,
    /// The task at this position was the first to fail, with the message
    /// of its result, or of its first failed item for a loop.
    Failed(usize, String),
}

/// Run `tasks` as written, one host per case whose variables the case
/// supplies, and report each case's verdict in the order of `cases`. Each
/// task's result is registered and its failure ignored, so a case whose
/// first failure is found still reports the tasks after it; only the first
/// counts, as a run stops at it. Hosts keep their own facts, so no case
/// sees another's. The tasks must be ones that run on the controller and
/// never connect, such as assertions and facts; a task that registers its
/// result keeps that name.
pub(super) fn first_failures(tasks: &[Value], cases: &[Value]) -> Vec<Verdict> {
    let scratch = Scratch::new();
    first_failures_from(scratch.path(), tasks, cases, &scratch)
}

/// Run as [`first_failures`] does, with the playbook written into
/// `directory`, the play's `playbook_dir`; tasks may then also run modules
/// on the controller through `delegate_to: localhost`.
pub(super) fn first_failures_from(
    directory: &Path,
    tasks: &[Value],
    cases: &[Value],
    scratch: &Scratch,
) -> Vec<Verdict> {
    let mut steps = Vec::new();
    let mut registered = Vec::new();
    for (index, task) in tasks.iter().enumerate() {
        let mut step = task.as_object().unwrap().clone();
        // A task that registers its result keeps its name, which later
        // tasks read; any other one is registered for its verdict alone.
        let name = step.get("register").and_then(Value::as_str);
        let name = name.map_or_else(|| format!("outcome_{index}"), str::to_owned);
        step.insert("register".into(), name.clone().into());
        step.insert("ignore_errors".into(), true.into());
        steps.push(Value::Object(step));
        registered.push(name);
    }
    let outcomes = registered.iter().map(|result| {
        format!(
            "{{'failed': {result} is failed, 'messages': ({result}.results | default([{result}]))\
             | selectattr('failed', 'defined') | selectattr('failed') | map(attribute='msg') \
             | list}}"
        )
    });
    let outcomes = outcomes.collect::<Vec<_>>().join(", ");
    steps.push(json!({
        "ansible.builtin.copy": {
            "content": format!("{{{{ [{outcomes}] | to_json }}}}"),
            "dest": scratch.path().join("{{ inventory_hostname }}.json"),
            "mode": "0600",
        },
        "check_mode": false,
        "delegate_to": "localhost",
        "vars": {
            "ansible_connection": "local",
            "ansible_python_interpreter": "{{ ansible_playbook_python }}",
        },
    }));
    let hosts: serde_json::Map<String, Value> = cases
        .iter()
        .enumerate()
        .map(|(index, case)| (format!("case_{index}"), case.clone()))
        .collect();
    let inventory = scratch.path().join("inventory.json");
    let hosts = json!({"all": {"hosts": hosts}});
    std::fs::write(&inventory, serde_json::to_vec(&hosts).unwrap()).unwrap();
    let book = json!([{"hosts": "all", "gather_facts": false, "tasks": steps}]);
    run_from(
        directory,
        &book,
        inventory.to_str().unwrap(),
        &["--forks", "16"],
        scratch,
    );
    let verdict = |index: usize| {
        let path = scratch.path().join(format!("case_{index}.json"));
        let outcomes: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(outcomes.len(), tasks.len());
        let failed = outcomes
            .iter()
            .position(|outcome| outcome["failed"] == true);
        failed.map_or(Verdict::Passed, |position| {
            let messages = outcomes[position]["messages"].as_array().unwrap();
            let message = messages.first().and_then(Value::as_str).unwrap_or_default();
            Verdict::Failed(position, message.to_owned())
        })
    };
    (0..cases.len()).map(verdict).collect()
}

/// Evaluate each expression against each case's variables. The result is one
/// row per case with one value per expression, exactly as Ansible computed it.
pub(super) fn evaluate(expressions: &[&str], cases: &[Value]) -> Vec<Vec<Value>> {
    evaluate_in(&[], expressions, cases)
}

/// Evaluate as [`evaluate`] does, with the variable files of a deployment
/// loaded underneath each case, so a case stands for a host's facts and its
/// inventory overrides.
pub(super) fn evaluate_in(
    vars_files: &[&str],
    expressions: &[&str],
    cases: &[Value],
) -> Vec<Vec<Value>> {
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
    let scratch = play(&Value::Array(tasks), &json!({}), vars_files);
    let rows = std::fs::read(scratch.path().join("rows.json")).unwrap();
    let rows: Vec<Vec<Value>> = serde_json::from_slice(&rows).unwrap();
    assert_eq!(rows.len(), cases.len());
    rows
}
