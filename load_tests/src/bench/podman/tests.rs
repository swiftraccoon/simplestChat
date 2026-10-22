//! Actual JavaScript exports remain the oracle; these cases never start Podman.

use super::{attribution, netem, options, parsers};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn root() -> PathBuf {
    std::env::var_os("BENCH_REFERENCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_owned()
        })
}
fn outcome(result: anyhow::Result<impl serde::Serialize>) -> Value {
    match result {
        Ok(value) => json!({"ok":true,"value":value}),
        Err(error) => json!({"ok":false,"error":error.to_string()}),
    }
}
fn native(case: &Value) -> Value {
    let args = case["args"].as_array().unwrap();
    let text = |n: usize| args[n].as_str().unwrap();
    let num = |n: usize| args[n].as_f64().unwrap();
    match case["op"].as_str().unwrap() {
        "parseOptions" => {
            let args = args[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| OsString::from(v.as_str().unwrap()))
                .collect::<Vec<_>>();
            outcome(options::parse(&args, &root()))
        }
        "netemScript" => outcome(netem::script(
            text(0),
            text(1),
            num(2) as u16,
            num(3) as u32,
        )),
        "threadTicks" => outcome(Ok(parsers::thread_ticks(text(0)))),
        "threadGroup" => outcome(Ok(attribution::group(text(0)))),
        "cgroupCpuStat" => outcome(parsers::cgroup_cpu_stat(text(0))),
        "parseMemoryBlock" => outcome(parsers::memory_block(text(0))),
        "parseServerBlock" => outcome(parsers::server_block(text(0), num(1), num(2))),
        "parseNetnsUdp" => outcome(parsers::netns_udp(
            text(0),
            &args[1]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u16)
                .collect::<Vec<_>>(),
        )),
        "serverAttribution" => {
            match serde_json::from_value::<Vec<parsers::ServerSample>>(args[0].clone()) {
                Ok(samples) => outcome(attribution::summarize(
                    &samples,
                    num(1),
                    num(2),
                    num(3),
                    num(4),
                )),
                Err(_) => json!({"ok":false}),
            }
        }
        _ => panic!("unrecognized actual-export fixture"),
    }
}
fn numbers(value: &mut Value) {
    match value {
        Value::Number(number) => {
            *value = serde_json::Number::from_f64(number.as_f64().unwrap())
                .map_or(Value::Null, Value::Number)
        }
        Value::Array(values) => values.iter_mut().for_each(numbers),
        Value::Object(values) => values.values_mut().for_each(numbers),
        _ => {}
    }
}
fn compare(cases: &[Value]) {
    let script = r#"
import {readFileSync} from 'node:fs';import {pathToFileURL} from 'node:url';
const policy=await import(pathToFileURL(process.argv[1]+'/load_tests/benchmark-podman.mjs'));
process.stdout.write(JSON.stringify(JSON.parse(readFileSync(0,'utf8')).map(c=>{
try{return {ok:true,value:policy[c.op](...c.args)}}catch(e){return {ok:false,error:e.message}}
})));
"#;
    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", script])
        .arg(root())
        .current_dir(root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(cases).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    for (index, (case, mut expected)) in cases.iter().zip(expected).enumerate() {
        let mut actual = native(case);
        numbers(&mut actual);
        numbers(&mut expected);
        assert_eq!(actual, expected, "case {index}: {case}");
    }
}
fn base() -> Vec<String> {
    [
        "--server-image",
        "server",
        "--generator-image",
        "generator",
        "--output",
        "new out",
        "--clients",
        "10",
    ]
    .map(str::to_owned)
    .to_vec()
}
fn option(extra: &[&str]) -> Value {
    let mut args = base();
    args.extend(extra.iter().map(|v| v.to_string()));
    json!({"op":"parseOptions","args":[args]})
}

#[test]
fn podman_options_match_original_quotas_admission_graph_and_capture_contracts() {
    let mut cases = vec![option(&[])];
    for (key, values) in [
        (
            "--duration",
            vec![
                "2", "3", "600", "601", "3.0", "3e0", " 3 ", "0x10", "NaN", "Infinity",
            ],
        ),
        ("--warmup", vec!["1", "2", "60", "61"]),
        ("--ramp-up", vec!["0", "1", "1200", "1201"]),
        ("--workers", vec!["0", "1", "4", "5"]),
        ("--repetitions", vec!["0", "1", "5", "6"]),
        ("--cpus", vec!["0.49", "0.5", "2", "16", "16.1", "NaN"]),
        ("--generator-cpus", vec!["0.49", "0.5", "32", "32.1", "NaN"]),
        (
            "--memory",
            vec!["512m", "2g", "0g", "01m", "2GB", "1.5g", ""],
        ),
        ("--capture-handshakes", vec!["true", "false", "yes", ""]),
        ("--port", vec!["1023", "1024", "65535", "65536"]),
        ("--udp-port", vec!["1023", "1024", "65531", "65532"]),
        (
            "--source-addresses",
            vec!["0", "1", "250", "60000", "60001"],
        ),
        (
            "--server-env",
            vec!["RUST_LOG=a,b;A=with=equals", "lower=x", "A_B=", "A=x;A=y"],
        ),
        (
            "--scenarios",
            vec![
                "conference",
                "multi-room",
                "audio",
                "webinar",
                "conference,webinar",
                "audio,audio",
                "bogus",
            ],
        ),
        (
            "--subscription-plan",
            vec!["fifo", "ring-v1", "hotspot-v1", "wrong"],
        ),
    ] {
        for value in values {
            cases.push(option(&[key, value]));
        }
    }
    for count in [2, 10, 30, 100, 200, 300, 1000, 1500, 2000, 2001] {
        for source in [1, 4, 5, 250] {
            for scenario in ["conference", "multi-room", "webinar"] {
                cases.push(option(&[
                    "--clients",
                    &count.to_string(),
                    "--source-addresses",
                    &source.to_string(),
                    "--scenarios",
                    scenario,
                    "--ramp-up",
                    "303",
                ]));
            }
        }
    }
    for plan in ["fifo", "ring-v1", "hotspot-v1"] {
        for seed in ["0", "17", "4294967295", "4294967296", "01", "-1"] {
            cases.push(option(&[
                "--subscription-plan",
                plan,
                "--subscription-seed",
                seed,
            ]));
        }
    }
    cases.push(option(&[
        "--netem",
        "loss 2%",
        "--netem-scope",
        "udp-downlink",
    ]));
    cases.push(option(&["--netem-scope", "udp-downlink"]));
    cases.push(option(&["--unknown", "a"]));
    cases.push(option(&["--workers", "2", "--workers", "3"]));
    compare(&cases);
    let parsed = options::parse(
        &base().into_iter().map(Into::into).collect::<Vec<_>>(),
        &root(),
    )
    .unwrap();
    assert_eq!(parsed.graph().subscription_seed, None);
    assert_eq!(parsed.scenarios[0].scenario().clients, 10);
}

fn stat(tid: u32, name: &str, user: u64, system: u64) -> String {
    format!(
        "{tid} ({name}) S 0 1 1 0 -1 4194560 100 0 0 0 {user} {system} 0 0 20 0 3 0 5 1000 200 18446744073709551615 1 1 0 0\n"
    )
}
#[test]
fn real_sampler_blocks_thread_grouping_and_udp_drops_match_actual_exports() {
    let dump = stat(1, "simplestChat", 10, 5)
        + &stat(2, "mediasoup-worke", 100, 20)
        + &stat(3, "mediasoup-worke", 50, 10);
    let cgroup = "usage_usec 100\nnr_periods 2\nnr_throttled 1\nthrottled_usec 5\n";
    let server = format!("{}@@S\n{cgroup}@@Y\n{dump}", stat(1, "simplestChat", 10, 5));
    let memory = "Rss: 480000 kB\nAnonymous: 450000 kB\n@@M\n55d0c1a00000-55d0e5c00000 8192 [heap]\n7f3a40000000-7f3a44000000 262144\n@@C\nanon 460000000\nfile 12000000\nkernel 30000000\nsock 26000000\nslab 3000000\n";
    let udp = "Udp: InDatagrams InErrors RcvbufErrors\nUdp: 7909035 231 231\n@@UDP\n158: 00000000:A08C 00000000:0000 07 00000000:0001A000 00:00000000 00000000 10001 0 12345 2 0 200\n159: 00000000:A08D 00000000:0000 07 00000000:00000000 00:00000000 00000000 10001 0 12346 2 0 31\n";
    let mut cases = vec![
        json!({"op":"threadTicks","args":[dump]}),
        json!({"op":"cgroupCpuStat","args":[cgroup]}),
        json!({"op":"cgroupCpuStat","args":["usage_usec 10\n"]}),
        json!({"op":"parseServerBlock","args":[server,100,4096]}),
        json!({"op":"parseMemoryBlock","args":[memory]}),
        json!({"op":"parseMemoryBlock","args":["Rss: 1 kB\n@@M\n@@C\n"]}),
        json!({"op":"parseNetnsUdp","args":[udp,[41100,41101]]}),
        json!({"op":"parseNetnsUdp","args":["Udp: InDatagrams\n",[41100]]}),
    ];
    for name in [
        "mediasoup-worke",
        "mediasoup-worker-1",
        "tokio-runtime-w",
        "tokio-rt-worker",
        "simplestChat",
    ] {
        cases.push(json!({"op":"threadGroup","args":[name]}));
    }
    for workers in [1, 2, 3, 4] {
        for port in [41100, 41101, 41104] {
            for scope in ["udp-both", "udp-downlink", "everything"] {
                cases.push(json!({"op":"netemScript","args":["loss 5% delay 50ms 10ms",scope,port,workers]}));
            }
        }
    }
    cases.push(json!({"op":"netemScript","args":["loss 5%; rm -rf /","udp-both",41100,1]}));
    compare(&cases);
}

#[test]
fn attribution_preserves_window_thread_exit_and_cgroup_throttling_semantics() {
    let mut cases = Vec::new();
    for ticks in [100, 250, 1000] {
        for quota in [0.5, 2.0, 16.0] {
            for periods in [0, 100] {
                let detail = |worker, tokio, usage, periods, throttled, usec| {
                    json!({"threads":{"mediasoup-worker-1":worker,"tokio-runtime-w":tokio,"simplestChat":0},
            "cgroup":{"usage_usec":usage,"nr_periods":periods,"nr_throttled":throttled,"throttled_usec":usec}})
                };
                let samples = json!([{"elapsedMs":0,"serverDetail":detail(0,0,0,0,0,0)},
            {"elapsedMs":1000,"serverDetail":detail(100,50,1500000,10,0,0)},
            {"elapsedMs":11000,"serverDetail":detail(1100,250,16500000,10+periods,20,400000)},
            {"elapsedMs":12000,"serverDetail":null}]);
                cases.push(
                    json!({"op":"serverAttribution","args":[samples,1000,11000,ticks,quota]}),
                );
            }
        }
    }
    cases.push(json!({"op":"serverAttribution","args":[[],1000,11000,100,2]}));
    compare(&cases);
}
