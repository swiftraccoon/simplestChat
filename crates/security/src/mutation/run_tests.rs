//! Replays of the recorded mutation run.
//!
//! `recorded_mutation_run.json` holds what the reference in Python did
//! when it prepared the builds' environment with a scripted `rustup`, when
//! it looked at the disk with small bounds, and in whole runs with a
//! scripted mutation tool that logs its arguments and its environment and
//! leaves prepared results. The recording was made on a Mac, so the host is
//! given as one. Where the reference failed with an exception of its
//! interpreter, `named` gives the code here.

use super::{Bounds, Budget, Mutation, environment, finished, run};
use crate::{
    Fault, Result,
    context::{Context, Failure, Limits, Outcome},
    digest,
    fast::Programs,
    scratch::{Scratch, asked, host_git, kept, repository, said, scripted, written},
};
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    fs::DirBuilder,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

const RECORDED: &str = include_str!("recorded_mutation_run.json");

/// What the environment's own shell adds to what it is given.
const SHELL: [&str; 5] = ["_", "PWD", "OLDPWD", "SHLVL", "__CF_USER_TEXT_ENCODING"];
/// The locale of the fixed environment, which follows the host.
const LOCALE: [&str; 2] = ["LANG", "LC_ALL"];

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

fn list(value: &Value) -> &[Value] {
    value.as_array().expect("a recorded list")
}

/// The code here for what the reference raised.
fn named(code: &str) -> &str {
    match code {
        "FileNotFoundError" => "tool_entry_missing",
        "FileExistsError" => "tool_entry_unavailable",
        "TOMLDecodeError" | "UnicodeDecodeError" | "KeyError" | "ValueError" => {
            "mutation_rust_toolchain"
        }
        code => code,
    }
}

/// Write a recorded tree: text, text with a mode, a link, a directory, or
/// a file of so many bytes.
fn tree(root: &Path, files: &Value) {
    let mut plain = Map::new();
    for (path, spec) in files.as_object().expect("files") {
        let target = root.join(path);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("directories");
        if spec.get("directory").is_some() {
            std::fs::create_dir_all(&target).expect("a directory");
        } else if let Some(size) = spec["size"].as_u64() {
            let size = usize::try_from(size).expect("a small size");
            std::fs::write(&target, vec![b'x'; size]).expect("a file");
        } else {
            plain.insert(path.clone(), spec.clone());
        }
    }
    written(root, &plain);
}

/// The host's own programs of a case: the scripted `rustup`.
struct Host(PathBuf);

impl Programs for Host {
    fn program(&self, name: &'static str) -> Outcome<PathBuf> {
        Err(Failure::Prerequisite(name))
    }

    fn prerequisite(&self, name: &'static str) -> Outcome<PathBuf> {
        Ok(self.0.join(name))
    }

    fn python(&self) -> Outcome<PathBuf> {
        Err(Failure::Prerequisite("python"))
    }
}

/// The caller's environment of a case.
fn ambient(place: &Path, aws: Option<&str>) -> impl Fn(&str) -> Option<OsString> {
    let search = format!("{}:/usr/bin:/bin", place.join("tools").display());
    let (home, aws) = (place.to_path_buf(), aws.map(|aws| place.join(aws)));
    move |variable: &str| match variable {
        "PATH" => Some(search.clone().into()),
        "HOME" => Some(home.clone().into()),
        "AWS_LC_SYS_SYSTEM_DIR" => aws.clone().map(OsString::from),
        _ => None,
    }
}

#[test]
fn every_recorded_environment_is_prepared_alike() {
    let recording = recorded();
    let scratch = Scratch::new("me");
    let (mut prepared, mut refused, mut raised, mut stricter) = (0, 0, 0, 0);
    for (index, case) in list(&recording["environments"]).iter().enumerate() {
        let name = text(&case["name"]);
        let place = scratch.0.join(index.to_string());
        for part in ["root", "source", "tools"] {
            std::fs::create_dir_all(place.join(part)).expect("a directory");
        }
        let evidence = place.join("evidence");
        DirBuilder::new()
            .mode(0o700)
            .create(&evidence)
            .expect("a private directory");
        tree(&place, &case["files"]);
        if let Some(toolchain) = case["toolchain"].as_str() {
            // The recording holds each byte of the file as one character.
            let bytes: Vec<u8> = toolchain.chars().map(|byte| byte as u8).collect();
            std::fs::write(place.join("source/rust-toolchain.toml"), bytes).expect("a file");
        }
        let spelled = place.to_string_lossy().into_owned();
        let said = text(&case["said"]).replace("<place>", &spelled);
        let log = place.join("log");
        let answers = json!([[case["status"], said]]);
        scripted(&place.join("tools"), &log, "rustup", "rustup", &answers);
        let ambient = ambient(&place, case["aws"].as_str());
        let mut context = Context::new(&place.join("root"), &evidence, Limits::REVIEWED, &ambient)
            .expect("a context");
        let home = evidence.join("home");
        assert_eq!(context.variable("HOME"), Some(home.as_os_str()), "{name}");
        assert_eq!(context.variable("UNSET"), None, "{name}");
        let host = Host(place.join("tools"));
        let (source, openssl) = (place.join("source"), place.join(text(&case["openssl"])));
        let set = environment(&mut context, &host, &source, &openssl, &ambient);
        assert_eq!(asked(&log, &spelled), case["asked"], "{name}");
        let code = case["result"].get("code").map(text);
        // An empty answer of rustup was the working directory to the
        // reference, which then passed.
        if name == "no path of cargo" {
            assert_eq!(code, None, "{name}");
            let failure = set.expect_err("a refusal").to_string();
            assert_eq!(failure, "mutation_rust_toolchain", "{name}");
            stricter += 1;
        } else if let Some(code) = code {
            let failure = set.expect_err("a refusal").to_string();
            assert_eq!(failure, named(code), "{name}");
            if named(code) == code {
                refused += 1;
            } else {
                raised += 1;
            }
        } else {
            let pair = |(name, value): (&str, OsString)| {
                let value = value.to_string_lossy().replace(&spelled, "<place>");
                (name.to_owned(), Value::from(value))
            };
            let changed: Map<String, Value> =
                set.expect("variables").into_iter().map(pair).collect();
            assert_eq!(Value::Object(changed), case["changed"], "{name}");
            prepared += 1;
        }
    }
    assert_eq!((prepared, refused, raised, stricter), (7, 18, 13, 1));
}

#[test]
fn every_recorded_budget_is_kept_alike() {
    let recording = recorded();
    let scratch = Scratch::new("mb");
    let (mut within, mut over, mut unseen) = (0, 0, 0);
    for (index, case) in list(&recording["budgets"]).iter().enumerate() {
        let name = text(&case["name"]);
        let place = scratch.0.join(index.to_string());
        std::fs::create_dir_all(&place).expect("a place");
        for made in list(&case["made"]) {
            std::fs::create_dir_all(place.join(text(made))).expect("a directory");
        }
        let made = |part: &str| list(&case["made"]).iter().any(|made| made == part);
        if made("source") {
            tree(&place.join("source"), &case["source"]);
        }
        if made("results") {
            tree(&place.join("results"), &case["output"]);
        }
        let limit = |name: &str| case["limits"][name].as_u64().expect("a limit");
        let bounds = Bounds {
            workspace: limit("MAX_WORKSPACE"),
            evidence: limit("MAX_EVIDENCE"),
            entries: limit("MAX_WORKSPACE_ENTRIES"),
            reserve: limit("DISK_RESERVE"),
        };
        let (source, results) = (place.join("source"), place.join("results"));
        let mut budget = Budget::new(&source, &results, bounds);
        let shown = |looked: Result<bool>| match looked {
            Ok(healthy) => json!({"healthy": healthy}),
            Err(Fault(code)) => json!({"code": code}),
        };
        let expected = |looked: &Value| match looked.get("code").map(text) {
            Some(code) => json!({"code": named(code)}),
            None => looked.clone(),
        };
        for (look, forced) in [("first", false), ("second", false), ("forced", true)] {
            assert_eq!(
                shown(budget.healthy(forced)),
                expected(&case[look]),
                "{name}: {look}"
            );
        }
        assert_eq!(json!(budget.failure), case["failure"], "{name}");
        assert_eq!(json!(budget.failure.is_none()), case["valid"], "{name}");
        let mut observed = budget.observed.clone();
        if let Some(available) = observed.get_mut("availableBytes") {
            *available = available.is_u64().into();
        }
        assert_eq!(Value::Object(observed), case["observed"], "{name}");
        match (case["first"].get("code").is_some(), case["valid"] == true) {
            (true, _) => unseen += 1,
            (false, true) => within += 1,
            (false, false) => over += 1,
        }
    }
    assert_eq!((within, over, unseen), (11, 10, 1));
}

/// The scripted mutation tool of a case: it logs its arguments, where it
/// runs, its environment and the modes of two sources, leaves the prepared
/// results when it is not asked for a listing, then answers from files.
fn tool(tools: &Path, log: &Path, extra: &str) -> String {
    let (tools, log) = (tools.display(), log.display());
    let mut lines = vec![
        "#!/bin/sh".to_owned(),
        // The command sets this mask for itself and what it runs.
        "umask 077".to_owned(),
        format!("count=$(cat '{tools}/cargo-mutants.count')"),
        format!("echo $((count + 1)) > '{tools}/cargo-mutants.count'"),
        "printf '%s\\t%s\\n' \\".to_owned(),
        "  \"$(printf '%s\\0' \"$@\" | od -An -v -tx1 | tr -d ' \\n')\" \"$(pwd -P)\" \\"
            .to_owned(),
        format!("  >> '{log}'"),
        "listing=0; out=\"\"; dir=\"\"; previous=\"\"".to_owned(),
        "for argument in \"$@\"; do".to_owned(),
        "  [ \"$argument\" = \"--list\" ] && listing=1".to_owned(),
        "  [ \"$previous\" = \"--output\" ] && out=\"$argument\"".to_owned(),
        "  [ \"$previous\" = \"--dir\" ] && dir=\"$argument\"".to_owned(),
        "  previous=\"$argument\"".to_owned(),
        "done".to_owned(),
        format!("env | LC_ALL=C sort > '{tools}/cargo-mutants.'$count'.env'"),
        "ls -l \"$dir/build/run.sh\" \"$dir/src/labels.rs\" 2>/dev/null | cut -c1-10 \\".to_owned(),
        format!("  > '{tools}/cargo-mutants.'$count'.modes'"),
        "if [ \"$listing\" = 0 ]; then".to_owned(),
        format!(
            "  mkdir -p \"$out/mutants.out\" && cp -R '{tools}/prepared/.' \"$out/mutants.out/\""
        ),
    ];
    lines.extend((!extra.is_empty()).then(|| format!("  {extra}")));
    lines.extend([
        "fi".to_owned(),
        format!("cat '{tools}/cargo-mutants'.$count.out"),
        format!("exit \"$(cat '{tools}/cargo-mutants'.$count.status)\""),
        String::new(),
    ]);
    lines.join("\n")
}

/// A recorded piece of evidence as it is compared: without what differs
/// from run to run, and with the codes this crate gives.
fn compared(name: &str, kept: &str) -> Value {
    let mut value: Value = match name {
        "outcome.json" | "budget.json" | "summary.json" | "inventory.json" => {
            serde_json::from_str(kept).expect("JSON evidence")
        }
        _ => return kept.into(),
    };
    for varying in ["elapsedSeconds", "availableBytes"] {
        if let Some(found) = value.get_mut(varying) {
            *found = (found.is_number() || *found == true).into();
        }
    }
    if let Some(error) = value.get_mut("error") {
        let code = match error.as_str().expect("a code") {
            "JSONDecodeError" => "mutation_inventory_form",
            code => named(code),
        };
        *error = code.into();
    }
    // What was counted up to a bound that was passed depends on the order
    // in which a directory is listed.
    if name == "budget.json" && !value["failure"].is_null() {
        value = json!({"passed": value["passed"], "failure": value["failure"]});
    }
    value
}

#[test]
fn every_recorded_run_is_made_alike() {
    let recording = recorded();
    let scratch = Scratch::new("mr");
    let git = host_git();
    let (mut passed, mut failed, mut unbegun, mut printed) = (0, 0, 0, 0);
    for (index, case) in list(&recording["runs"]).iter().enumerate() {
        let name = text(&case["name"]);
        let place = scratch.0.join(index.to_string());
        for part in ["root", "tools/prepared"] {
            std::fs::create_dir_all(place.join(part)).expect("a directory");
        }
        for made in list(&case["made"]) {
            std::fs::create_dir_all(place.join(text(made))).expect("a directory");
        }
        let (root, tools) = (place.join("root"), place.join("tools"));
        repository(
            &root,
            &git,
            &recording,
            &json!({"first": case["files"], "working": {}}),
        );
        tree(&place, &case["static"]);
        tree(&tools.join("prepared"), &case["prepared"]);
        let spelled = place.to_string_lossy().into_owned();
        let (rustup, called) = (place.join("rustup.log"), place.join("tool.log"));
        let cargo = format!("{spelled}/toolchain/bin/cargo\n");
        scripted(&tools, &rustup, "rustup", "rustup", &json!([[0, cargo]]));
        let answers = json!([
            [case["statuses"][0], case["listing"]],
            [case["statuses"][1], "mutants done\n"]
        ]);
        scripted(&tools, &called, "cargo-mutants", "cargo-mutants", &answers);
        let program = tools.join("cargo-mutants");
        std::fs::write(&program, tool(&tools, &called, text(&case["extra"]))).expect("a tool");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("a mode");
        let identity = digest(&std::fs::read(&program).expect("the tool"));
        let limit = |name: &str, usual: u64| case["limits"][name].as_u64().unwrap_or(usual);
        let bounds = Bounds {
            evidence: limit("MAX_EVIDENCE", Bounds::REVIEWED.evidence),
            ..Bounds::REVIEWED
        };
        let ambient = ambient(&place, Some("aws"));
        let (host, target) = (Host(tools.clone()), place.join(text(&case["output"])));
        let openssl = place.join(text(&case["openssl"]));
        let found = || Ok(program.clone());
        let options = Mutation {
            root: &root,
            output: &target,
            platform: text(&case["platform"]),
            darwin: true,
            openssl: &openssl,
            tool: &found,
            programs: &host,
            git: &git,
            ambient: &ambient,
            bounds,
        };
        let outcome = run(&options);
        let tokenised = |text: &str| {
            text.replace(&spelled, "<place>")
                .replace(&identity, "<tool digest>")
        };
        let listed = kept(&target, &["home", "results/mutants.out"]);
        assert_eq!(listed, case["kept"], "{name}");
        if let Some(mode) = case["outputMode"].as_u64() {
            let made = target
                .metadata()
                .expect("the evidence")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(u64::from(made), mode, "{name}");
        }
        let evidence = [
            "outcome.json",
            "summary.json",
            "inventory.json",
            "budget.json",
            "mutation.stdout",
            "mutation.stderr",
        ];
        let left = said(&target, &spelled, &["home", "results"], |below| {
            evidence.contains(&below)
        });
        let recorded = case["texts"].as_object().expect("texts");
        let names = |texts: &Map<String, Value>| texts.keys().cloned().collect::<Vec<_>>();
        assert_eq!(
            names(left.as_object().expect("texts")),
            names(recorded),
            "{name}"
        );
        for (file, there) in recorded {
            let here = compared(file, &tokenised(text(&left[file])));
            assert_eq!(here, compared(file, text(there)), "{name}: {file}");
        }
        assert_eq!(asked(&rustup, &spelled), case["rustup"], "{name}");
        assert_eq!(asked(&called, &spelled), case["tool"], "{name}");
        let mut environments = Vec::new();
        let mut modes = Vec::new();
        for call in 0..2 {
            let Ok(logged) =
                std::fs::read_to_string(tools.join(format!("cargo-mutants.{call}.env")))
            else {
                continue;
            };
            let pairs = logged.lines().filter_map(|line| line.split_once('='));
            let own = pairs.filter(|(name, _)| !SHELL.contains(name) && !LOCALE.contains(name));
            let pair =
                |(name, value): (&str, &str)| (name.to_owned(), Value::from(tokenised(value)));
            environments.push(Value::Object(own.map(pair).collect()));
            let seen = std::fs::read_to_string(tools.join(format!("cargo-mutants.{call}.modes")));
            let seen = seen.expect("the modes");
            modes.push(json!(seen.split_whitespace().collect::<Vec<_>>()));
        }
        let mut expected = case["environments"].clone();
        for environment in expected.as_array_mut().expect("environments") {
            let variables = environment.as_object_mut().expect("variables");
            variables.retain(|name, _| !LOCALE.contains(&name.as_str()));
        }
        assert_eq!(json!(environments), expected, "{name}");
        assert_eq!(json!(modes), case["modes"], "{name}");
        if case["throughMain"] == true {
            let (line, status) = finished(&outcome, &target);
            assert_eq!(
                json!(line.replace(&spelled, "<place>")),
                case["printed"],
                "{name}"
            );
            assert_eq!(json!(status), case["result"]["status"], "{name}");
            printed += 1;
        } else if let Some(code) = case["result"].get("code").map(text) {
            let code = match code {
                "JSONDecodeError" => "mutation_policy_form",
                code => named(code),
            };
            assert_eq!(outcome.expect_err("a refusal").to_string(), code, "{name}");
            unbegun += 1;
        } else if outcome.expect("an ending") {
            assert_eq!(case["result"]["passed"], true, "{name}");
            passed += 1;
        } else {
            assert_eq!(case["result"]["passed"], false, "{name}");
            failed += 1;
        }
    }
    assert_eq!((passed, failed, unbegun, printed), (2, 15, 4, 5));
}
