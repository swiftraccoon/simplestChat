//! Live scheduling policy compared with the retained JavaScript sampler. The
//! request and clock are explicit fixtures; no HTTP request or workload runs.

use super::{
    artifacts,
    media_sampler::{self, Sampler},
    options, policy_tests,
};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::Write;
use std::process::{Command, Stdio};

fn settings(duration: u32) -> options::Options {
    let mut arguments = policy_tests::required();
    arguments.extend([
        "--purpose".into(),
        "diagnostic".into(),
        "--duration".into(),
        duration.to_string(),
        "--warmup".into(),
        "2".into(),
        "--ramp-up".into(),
        "1".into(),
    ]);
    options::parse(
        &arguments
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>(),
        &std::env::current_dir().unwrap(),
    )
    .unwrap()
}

fn root() -> std::path::PathBuf {
    std::env::var_os("BENCH_REFERENCE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_owned()
        })
}

#[test]
fn schedule_lateness_teardown_missed_samples_and_private_write_failures_match_reference() {
    let alive = json!({"server":true,"generator":true});
    let dead = json!({"server":true,"generator":false});
    let mut cases = Vec::new();
    for duration in [3, 4, 5, 6, 60, 179, 180] {
        let options = settings(duration);
        let schedule = media_sampler::schedule(&options);
        for variant in 0..8 {
            let mut actions = Vec::new();
            for (index, &at) in schedule.iter().enumerate() {
                let at = at + if variant == 1 { 1001.0 } else { 0.0 };
                actions.push(json!({"at":at,"requestMs":if variant == 2 {2100} else {10},
                    "before":if variant == 3 {&dead} else {&alive},
                    "after":if variant == 4 {&dead} else {&alive},
                    "result":{"status":"error","issue":if variant == 5 {
                        "media_request_failed"} else {"media_unavailable"}},
                    "persistFail":variant == 6 && index == 0}));
                if variant == 7 {
                    actions.push(json!({"at":at+100.0,"requestMs":0,
                        "before":alive,"after":alive,"result":{
                            "status":"error","issue":"media_busy"},"persistFail":false}));
                }
            }
            cases.push(json!({"options":options,"actions":actions,
                "finishAt":f64::from(3+duration)*1000.0+1500.0,"finishAlive":dead}));
        }
        cases.push(json!({"options":options,"actions":[],"finishAt":0,"finishAlive":dead}));
    }
    let script = r#"
import {readFileSync} from 'node:fs';import {pathToFileURL} from 'node:url';
const policy=await import(pathToFileURL(process.argv[1]+'/load_tests/media-diagnostic-report.mjs'));
const cases=JSON.parse(readFileSync(0,'utf8')),rows=[];
for(const row of cases){let time=0,alive=row.finishAlive,current;
const sampler=policy.createMediaSampler(row.options,{now:()=>time,alive:()=>alive,
request:async()=>{time+=current.requestMs;alive=current.after;return current.result;},
persist:async()=>{if(current?.persistFail)throw Error('private sentinel');}});
for(current of row.actions){time=current.at;alive=current.before;await sampler.poll();}
time=row.finishAt;alive=row.finishAlive;current=null;await sampler.finish();
rows.push({schedule:policy.mediaSampleSchedule(row.options),samples:sampler.samples});}
process.stdout.write(JSON.stringify(rows));
"#;
    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", script])
        .arg(root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&cases).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "Reference import and pure sampler must succeed"
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    let directory =
        std::env::temp_dir().join(format!("benchmark-sampler-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    for (index, (case, expected)) in cases.iter().zip(expected).enumerate() {
        let options = serde_json::from_value(case["options"].clone()).unwrap();
        let target = directory.join(index.to_string());
        artifacts::directory(&target).unwrap();
        let mut sampler = Sampler::new(&options, target.clone());
        for action in case["actions"].as_array().unwrap() {
            let now = action["at"].as_f64().unwrap();
            if let Some(mut sample) = sampler.prepare(now, action["before"].clone()) {
                if sample["status"].is_null() {
                    sampler.complete(
                        &mut sample,
                        action["result"].clone(),
                        now + action["requestMs"].as_f64().unwrap(),
                        action["after"].clone(),
                    );
                }
                if action["persistFail"] == true {
                    let path = target.join(format!(
                        "server-media-sample-{:02}.json",
                        sample["ordinal"].as_u64().unwrap()
                    ));
                    artifacts::new_file(&path, b"owned sentinel").unwrap();
                }
                sampler.save(sample);
            }
        }
        sampler.finish(
            case["finishAt"].as_f64().unwrap(),
            case["finishAlive"].clone(),
        );
        let actual =
            json!({"schedule":media_sampler::schedule(&options),"samples":sampler.samples});
        let normalize = |value: &Value| {
            serde_json::from_str::<Value>(&serde_json::to_string(value).unwrap()).unwrap()
        };
        fn numbers(value: &mut Value) {
            match value {
                Value::Number(number) => {
                    *number = serde_json::Number::from_f64(number.as_f64().unwrap()).unwrap()
                }
                Value::Array(values) => values.iter_mut().for_each(numbers),
                Value::Object(values) => values.values_mut().for_each(numbers),
                _ => {}
            }
        }
        let mut actual = normalize(&actual);
        let mut expected = normalize(&expected);
        numbers(&mut actual);
        numbers(&mut expected);
        assert_eq!(actual, expected, "sampler case{index}");
    }
    std::fs::remove_dir_all(directory).unwrap();
}
