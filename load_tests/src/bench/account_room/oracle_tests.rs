//! Full-operation fixed argv/environment and proc-tree mapping against the
//! retained JavaScript reference. The only executed program is a pure oracle.

use super::{database, sampling, tests::Fixture};
use crate::bench::process::{self, Command, NativeHost, Sink};
use serde_json::{Value, json};
use std::{collections::BTreeMap, ffi::OsString, fs, path::Path, time::Duration};

async fn oracle(input: Value) -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let reference = serde_json::to_string(&root.join("build/account-room-check.mjs")).unwrap();
    let script = format!(
        r#"
import fs from 'node:fs';import {{pathToFileURL}} from 'node:url';
const m=await import(pathToFileURL({reference}).href), x=JSON.parse(fs.readFileSync(0,'utf8'));
let r;if(x.name==='serverProcessTree'){{const files=new Map(x.files);r=await m.serverProcessTree(123,[100,4096],{{readFile:async p=>files.get(p),readdir:async p=>p==='/proc/123/task'?['123','124']:[p.split('/')[2]]}});}}
else if(x.name==='generatorArguments'){{r=m.generatorArguments(m.configuration(x.environment,'linux'),x.credentials,x.room);}}
else r=m[x.name](...x.arguments);process.stdout.write(JSON.stringify(r));
"#
    );
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join("node"))
        .find(|p| p.is_file())
        .unwrap();
    let result = process::run(
        &NativeHost::default(),
        Command {
            executable: node,
            arguments: vec!["--input-type=module".into(), "-e".into(), script],
            directory: root.into(),
            environment: [("PATH".into(), std::env::var_os("PATH").unwrap_or_default())].into(),
            timeout: Duration::from_secs(10),
            stdout_limit: 65536,
            stderr_limit: 65536,
            input: serde_json::to_vec(&input).unwrap(),
            stdout: Sink::Capture,
            stderr: Sink::Capture,
            cleanup: false,
        },
    )
    .await
    .unwrap();
    assert!(result.exit.success());
    assert!(result.stderr.is_empty());
    serde_json::from_slice(&result.stdout).unwrap()
}

#[tokio::test]
async fn exact_generator_arguments_and_libpq_environment_match_reference() {
    let fixture = Fixture::new();
    let config = fixture.config();
    let credentials = Path::new("/private/accounts.json");
    let room = "account-room-12345678-1234-1234-1234-123456789012";
    let mut environment: BTreeMap<String, String> = config
        .environment
        .iter()
        .map(|(k, v)| (k.to_str().unwrap().into(), v.to_str().unwrap().into()))
        .collect();
    environment.insert("TEST_SERVER_PID".into(), "123".into());
    environment.insert("TEST_METRICS_TOKEN".into(), config.metrics_token.clone());
    environment.insert("BASE_URL".into(), config.origin.clone());
    assert_eq!(
        json!(config.generator_arguments(credentials, room)),
        oracle(json!({
        "name":"generatorArguments","environment":environment,
        "credentials":credentials,"room":room}))
        .await
    );
    for url in [
        "postgres://test%5Fowner:public%2Dfixture%21@127.0.0.1:5439/fixture_test?sslmode=disable",
        "postgres://fixture@[::1]:5432/fixture_test",
        "postgres://fixture:literal+plus@localhost/fixture_test",
    ] {
        let mut environment = fixture.environment();
        environment.insert("DATABASE_URL".into(), url.into());
        let config = super::config::Config::parse(environment, fixture.0.clone(), "linux").unwrap();
        let actual: BTreeMap<String, String> = database::environment(&config)
            .unwrap()
            .into_iter()
            .map(|(k, v)| (k.to_str().unwrap().into(), v.to_str().unwrap().into()))
            .collect();
        let path = config.environment[&OsString::from("PATH")]
            .to_str()
            .unwrap();
        assert_eq!(
            json!(actual),
            oracle(json!({"name":"databaseEnvironment","arguments":[url,path]})).await
        );
    }
}

fn stat(pid: u32, user: u32, pages: u32) -> String {
    let mut fields = vec!["0".to_owned(); 22];
    fields[0] = "S".into();
    fields[11] = user.to_string();
    fields[12] = "10".into();
    fields[19] = (pid * 10).to_string();
    fields[21] = pages.to_string();
    format!("{pid} (worker with spaces) {}", fields.join(" "))
}
#[tokio::test]
async fn native_process_tree_sums_thread_children_and_preserves_start_identity() {
    let fixture = Fixture::new();
    let root = fixture.0.join("proc");
    fs::create_dir(&root).unwrap();
    let mut files = Vec::new();
    for (pid, user, pages, threads) in [
        (123, 100, 5, vec![(123, "456"), (124, "456")]),
        (456, 200, 7, vec![(456, "789")]),
        (789, 300, 11, vec![(789, "")]),
    ] {
        fs::create_dir_all(root.join(format!("{pid}/task"))).unwrap();
        let source = stat(pid, user, pages);
        fs::write(root.join(format!("{pid}/stat")), &source).unwrap();
        files.push((format!("/proc/{pid}/stat"), source));
        for (thread, children) in threads {
            fs::create_dir(root.join(format!("{pid}/task/{thread}"))).unwrap();
            fs::write(root.join(format!("{pid}/task/{thread}/children")), children).unwrap();
            files.push((
                format!("/proc/{pid}/task/{thread}/children"),
                children.into(),
            ));
        }
    }
    let actual = sampling::process_tree(&root, 123, 100.0, 4096.0).unwrap();
    let expected = oracle(json!({"name":"serverProcessTree","files":files})).await;
    assert_eq!(actual["rssKiB"].as_f64(), expected["rssKiB"].as_f64());
    assert!(
        (actual["cpuSeconds"].as_f64().unwrap() - expected["cpuSeconds"].as_f64().unwrap()).abs()
            < 1e-12
    );
    assert_eq!(actual["processes"], expected["processes"]);
    assert_eq!(actual["childCount"], expected["childCount"]);
    fs::write(root.join("123/task/124/children"), "1").unwrap();
    assert!(sampling::process_tree(&root, 123, 100.0, 4096.0).is_err());
}
