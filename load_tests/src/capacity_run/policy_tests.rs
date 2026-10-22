//! Offline parser, judging, search, generator and report comparisons to Python.

use super::{commands, conclude, host, json, judge, options, parsers, search, types::*};
use serde_json::{Value, json as value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub(super) fn oracle(script: &str, input: &Value) -> Value {
    oracle_bytes(script, &serde_json::to_vec(input).unwrap())
}

pub(super) fn oracle_bytes(script: &str, input: &[u8]) -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut child = Command::new("python3").arg("-c")
        .arg(format!("import sys,json,re\nsys.path[:0]=[sys.argv[1],sys.argv[2]]\nimport capacity as c\n{script}"))
        .arg(root.join("build")).arg(root.join("ops/ansible/files"))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn step(index: usize) -> StepResult {
    let workload = Workload::ALL[index % 3];
    StepResult {
        workload,
        size: 40,
        clients: if workload == Workload::Webinar {
            41
        } else {
            40
        },
        rooms: if workload == Workload::Meetings { 8 } else { 1 },
        generator_passed: true,
        meeting_size: 5,
        speakers: 1,
        ..StepResult::default()
    }
}

#[test]
fn all_judging_reasons_thresholds_and_cap_attribution_match_python() {
    let mut cases = Vec::new();
    for index in 0..256 {
        let mut result = step(index);
        result.generator_throttled = if index & 1 != 0 { 0.051 } else { 0.05 };
        result.generator_setup_throttled = if index & 2 != 0 { 0.051 } else { 0.0 };
        result.generator_out_of_memory = index & 4 != 0;
        result.failed_connections = i64::from(index & 8 != 0);
        result.failed_consumers = if index & 16 != 0 { 3 } else { 0 };
        result.failed_video_consumers = result.failed_consumers;
        result.viewers_at_cap = 30;
        result.socket_drops = if index & 32 != 0 { 11 } else { 10 };
        result.sent_datagrams = 10000;
        result
            .worker_load_peak
            .insert(0, if index & 64 != 0 { 0.71 } else { 0.7 });
        result.worker_load_mean.insert(0, 0.3);
        result.server_throttled = if index & 128 != 0 { 0.11 } else { 0.1 };
        if index % 13 == 0 {
            result.server_exit = "stopped".into();
        }
        if index % 17 == 0 {
            result.error = "bad evidence".into();
        }
        if index % 7 == 0 {
            result.server_saturated_scrapes = 1;
        }
        if index % 11 == 0 {
            result.memory_peak_bytes = 86;
            result.memory_limit_bytes = 100;
        }
        if index % 19 == 0 {
            result.audio_only = true;
        }
        if index % 23 == 0 {
            result.generator_passed = false;
            result.generator_errors = 9;
        }
        cases.push(result);
    }
    let mut failed = step(0);
    failed.generator_passed = false;
    failed.generator_failures = vec!["delivery failed".into()];
    failed.generator_failure_count = 8;
    cases.push(failed);
    let input = serde_json::to_value(&cases).unwrap();
    let expected = oracle(
        r#"
def snake(k): return re.sub(r'[A-Z]',lambda m:'_'+m[0].lower(),k)
answer=[]
for v in json.load(sys.stdin):
 s=c.StepResult(**{snake(k):v for k,v in v.items()})
 answer.append(c.record(c.judge(s,c.Limits())))
print(json.dumps(answer))
"#,
        &input,
    );
    let actual = cases
        .iter()
        .map(|step| judge::judge(step, Limits::default()))
        .collect::<Vec<_>>();
    assert_eq!(serde_json::to_value(actual).unwrap(), expected);
}

#[test]
fn plans_argv_streams_shapes_and_search_match_python_generated_cases() {
    let mut input = Vec::new();
    let mut actual = Vec::new();
    for workload in Workload::ALL {
        for host in [1, 2, 3, 4, 6, 8, 10, 32, 128] {
            for audio in [false, true] {
                let shape = search::shape(workload, host, (host - 1).max(1), None, None);
                let plan = Plan {
                    workload,
                    size: if workload == Workload::Meetings {
                        40
                    } else {
                        41
                    },
                    meeting_size: 5,
                    warmup_seconds: 20,
                    duration_seconds: 30,
                    workers: shape.workers,
                };
                let browser = Browser {
                    audio_only: audio,
                    chat_interval_ms: 3000,
                    ..Browser::default()
                };
                let config = search::search(workload, shape, 5, 6, None, audio);
                let trial = Trial {
                    size: plan.size,
                    passed: true,
                    valid: true,
                    busiest_worker: 0.31,
                    mean_worker: 0.25,
                    generator_cores: 0.6,
                };
                input.push(value!({"workload": workload, "host": host, "audio":audio,
            "size":plan.size, "workers":shape.workers}));
                actual.push(
                    value!({"shape":shape,"clients":plan.clients(),"rooms":plan.rooms(),
            "ramp":plan.ramp_seconds(),"window":plan.window(100.0),
            "streams":search::streams(workload,plan.size,5,audio),
            "args":commands::generator_args(plan,&browser,"label", &("srv".into(),"gen".into())),
            "next":search::next(config,&[trial],Limits::default(),shape.generator_cpus)}),
                );
            }
        }
    }
    let expected = oracle(
        r#"
answer=[]
for v in json.load(sys.stdin):
 w,h,a,n=v['workload'],v['host'],v['audio'],v['size']
 shape=c.shape_for(w,h,deployment_workers=max(1,h-1))
 plan=c.StepPlan(w,n,5,20,30,shape.workers)
 search=c.replace(c.search_for(w,shape,5,6),audio_only=a)
 trial=c.Trial(n,True,True,.31,.25,.6)
 answer.append(dict(shape=c.record(shape),clients=plan.clients,rooms=plan.rooms,
 ramp=plan.ramp_seconds,window=c.step_window(100.,plan),
 streams=c.forwarded_streams(w,n,5,audio_only=a),
 args=plan.generator_args(c.Browser(audio_only=a,chat_interval_ms=3000),'label',('srv','gen')),
 next=c.next_size(search,[trial],c.Limits(),shape.generator_cpus)))
print(json.dumps(answer))
"#,
        &Value::Array(input),
    );
    assert_eq!(Value::Array(actual), expected);
}

#[test]
fn parser_and_namespace_counters_preserve_the_reference() {
    let cpu = parsers::cpu_stat("usage_usec 5000000\nnr_periods 100\nnr_throttled 5\n").unwrap();
    assert_eq!(
        cpu,
        parsers::CpuStat {
            usage_usec: 5000000,
            periods: 100,
            throttled: 5
        }
    );
    assert_eq!(
        parsers::throttled(
            cpu,
            parsers::CpuStat {
                usage_usec: 9000000,
                periods: 200,
                throttled: 15
            }
        ),
        0.1
    );
    assert!(parsers::cpu_stat("nr_periods 5").is_err());
    let stat = "cpu 100 5 50 1000 20 0 3 7 999 888\ncpu0 1 2\ncpu1 1 2\n";
    assert_eq!(
        parsers::proc_stat(stat).unwrap(),
        parsers::HostTimes {
            total: 1185,
            idle: 1020,
            steal: 7
        }
    );
    assert_eq!(parsers::count_cpus(stat), 2);
    assert_eq!(parsers::memory_kib("MemTotal: 12345 kB\n").unwrap(), 12345);
    assert_eq!(parsers::platform("Vendor\n  Model\n"), "Vendor Model");
    let snmp = "Udp: InDatagrams RcvbufErrors\nUdp: 1000 12\n";
    let udp = "1: 00000000:9C40 00000000:0000 07 0 0 0 10001 0 1 2 0 7\n2: 00000000:C1A7 0 07 0 0 0 10001 0 2 2 0 100\n";
    assert_eq!(
        parsers::udp(snmp, udp, &[40000]).unwrap(),
        parsers::Udp {
            in_datagrams: 1000,
            rcvbuf_errors: 12,
            socket_drops: 7
        }
    );
    assert!(parsers::udp("", udp, &[40000]).is_err());
    let metrics = parsers::prometheus(
        "# comment\nsimplestchat_media_worker_cpu{worker=\"0\"} 0.3\nsimplestchat_media_worker_cpu{worker=\"1\"} 0.4\nother 1e3\nbad x\n",
    );
    assert_eq!(parsers::worker_loads(&metrics), [(0, 0.3), (1, 0.4)].into());
    assert_eq!(metrics["other"], 1000.0);
}

#[test]
fn browser_settings_and_ownership_options_are_explicit() {
    let args = [
        "--server-image",
        "server",
        "--generator-image",
        "gen",
        "--audio-only",
        "--chat-interval-ms",
        "3000",
        "--workloads",
        "meetings",
        "webinar",
        "--server-env",
        "RUST_LOG=trace",
    ]
    .map(Into::into);
    let parsed = options::parse(&args).unwrap();
    assert_eq!(parsed.meeting_size, 5);
    assert_eq!(parsed.worker_threshold, 0.7);
    assert!(parsed.browser.audio_only);
    assert_eq!(parsed.workloads.len(), 2);
    for name in [
        "BIND_ADDR",
        "PORT",
        "ANNOUNCE_IP",
        "MEDIA_WORKERS",
        "ALLOW_AD_HOC_ROOMS",
        "ALLOWED_ORIGINS",
        "METRICS_TOKEN",
        "CPU_SATURATION_WORKER_UTILIZATION",
    ] {
        assert!(options::server_env(&format!("{name}=bad")).is_err());
    }
    assert!(options::server_env("lower=x").is_err());
    assert_eq!(
        options::server_env("A_B=with=equals").unwrap(),
        ("A_B".into(), "with=equals".into())
    );
}

#[test]
fn summary_delivery_counts_and_invalid_measurement_guards_match_python() {
    let summary = value!({"run":{"passed":false,"failureReasons":["a","b","c","d","e","f"]},
        "totalErrors":3,"failedConnections":1,"failedConsumers":2,"validatedConsumers":9,
        "totalDatagramsSent":1200,"measurement":{"packetsReceived":100,"bytesReceived":2000,"durationMs":5000},
        "receiveMediaReady":{"p99Ms":1400},"videoStart":{"p99Ms":2200}});
    let clients = value!([{"lastAvailableBitrate":3000000,"consumerDelivery":[{"passed":false,"isAudio":false},{"passed":false,"isAudio":true}]}]);
    let mut result = step(0);
    conclude::read_summary(
        &mut result,
        &crate::capacity::json::parse(&serde_json::to_vec(&summary).unwrap()).unwrap(),
    )
    .unwrap();
    conclude::read_clients(
        &mut result,
        &crate::capacity::json::parse(&serde_json::to_vec(&clients).unwrap()).unwrap(),
    )
    .unwrap();
    let expected = oracle(
        r#"
v=json.load(sys.stdin);s=c.StepResult('meetings',40,40,8,'',generator_passed=True)
c.read_summary(s,v['summary']);c.read_clients(s,v['clients']);print(json.dumps(c.record(s)))
"#,
        &value!({"summary":summary,"clients":clients}),
    );
    assert_eq!(serde_json::to_value(&result).unwrap(), expected);
    assert_eq!(result.generator_failure_count, 6);
    assert_eq!(result.generator_failures.len(), 5);
    let legacy =
        crate::capacity::json::parse(br#"{"run":{"passed":true},"measurement":{}}"#).unwrap();
    assert!(
        conclude::read_summary(&mut result, &legacy)
            .unwrap_err()
            .to_string()
            .contains("rebuild")
    );
    assert!(
        conclude::read_clients(
            &mut result,
            &crate::capacity::json::parse(br#"[false]"#).unwrap()
        )
        .is_err()
    );
}

#[test]
fn python_report_number_and_unicode_framing_is_preserved() {
    #[derive(serde::Serialize)]
    struct Example<'a> {
        text: &'a str,
        values: Vec<f64>,
        map: std::collections::BTreeMap<i64, f64>,
    }
    let record = Example {
        text: "é\u{7f}😀",
        values: vec![1.0, 1e-7, 1e20, -0.0, f64::INFINITY, f64::NAN],
        map: [(0, 0.5)].into(),
    };
    let expected = oracle(
        "v={'text':'é\\x7f😀','values':[1.,1e-7,1e20,-0.,float('inf'),float('nan')],'map':{0:.5}}\nprint(json.dumps(json.dumps(v,indent=2)))",
        &Value::Null,
    );
    assert_eq!(
        String::from_utf8(json::bytes(&record).unwrap()).unwrap(),
        expected.as_str().unwrap()
    );
    assert_eq!(host::rounded(2.675, 2), 2.67);
}

#[test]
fn bracketing_budget_invalid_trials_and_every_ceiling_bound_match_reference() {
    let mut inputs = Vec::new();
    let mut actual = Vec::new();
    for workload in Workload::ALL {
        for audio in [false, true] {
            for mode in 0..12 {
                let pattern: &[(i64, bool, bool)] = match mode {
                    0 => &[],
                    1 => &[(10, true, true)],
                    2 => &[(10, false, true)],
                    3 => &[(10, true, true), (30, false, true)],
                    4 => &[(30, false, true), (10, true, true)],
                    5 | 6 => &[(10, true, true), (20, false, false)],
                    7 => &[(10, true, true), (20, true, true)],
                    8 => &[(10, false, true), (5, false, true)],
                    9 => &[(10, true, true), (12, false, true)],
                    10 => &[(10, true, true), (20, false, true), (15, true, true)],
                    _ => &[(10, false, false)],
                };
                let history = pattern
                    .iter()
                    .map(|(size, passed, valid)| {
                        let mut result = step(0);
                        result.workload = workload;
                        result.size = *size;
                        result.audio_only = audio;
                        result.worker_load_peak.insert(0, 0.3);
                        result.worker_load_mean.insert(0, 0.25);
                        result.generator_cores = if mode == 7 { 2.7 } else { 0.5 };
                        if mode == 6 && !*valid {
                            result.error = "lost readings".into();
                        }
                        (
                            result,
                            Verdict {
                                passed: *passed,
                                valid: *valid,
                                reasons: if *passed {
                                    Vec::new()
                                } else {
                                    vec!["test limit".into()]
                                },
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                let shape = Shape {
                    server_cpus: 1.0,
                    workers: 1,
                    generator_cpus: 2.8,
                };
                let search = search::search(
                    workload,
                    shape,
                    5,
                    if mode == 7 { 2 } else { 6 },
                    None,
                    audio,
                );
                let trials = history
                    .iter()
                    .map(|(s, v)| Trial::from_step(s, v))
                    .collect::<Vec<_>>();
                inputs.push(value!({"workload":workload,"audio":audio,"steps":search.max_steps,
            "history":history.iter().map(|(s,v)|value!({"step":s,"verdict":v})).collect::<Vec<_>>()}));
                actual.push(
                    value!({"next":search::next(search,&trials,Limits::default(),2.8),
            "ceiling":search::ceiling(workload,&history,2.8,1)}),
                );
            }
        }
    }
    let expected = oracle(
        r#"
def snake(k):return re.sub(r'[A-Z]',lambda m:'_'+m[0].lower(),k)
answer=[]
for v in json.load(sys.stdin):
 history=[]
 for entry in v['history']:
  s=c.StepResult(**{snake(k):v for k,v in entry['step'].items()});z=c.Verdict(**entry['verdict']);history.append((s,z))
 trials=[c.Trial(s.size,z.passed,z.valid,s.busiest_worker,s.mean_worker,s.generator_cores) for s,z in history]
 search=c.replace(c.search_for(v['workload'],c.Shape(1.,1,2.8),5,v['steps']),audio_only=v['audio'])
 answer.append(dict(next=c.next_size(search,trials,c.Limits(),2.8),ceiling=c.record(c.ceiling_of(v['workload'],history,2.8,workers=1))))
print(json.dumps(answer))
"#,
        &Value::Array(inputs),
    );
    assert_eq!(Value::Array(actual), expected);
    assert_eq!(
        search::shape(Workload::Meetings, 4, 3, Some(1.25), Some(0.2)).generator_cpus,
        0.2
    );
}

#[test]
fn strict_loopback_control_framing_rejects_partial_and_unsuccessful_outputs() {
    use super::{engine::parse_control, process::Output};
    let parse = |bytes: &[u8], status| {
        parse_control(Output {
            status,
            stdout: bytes.to_vec(),
            stderr: Vec::new(),
        })
    };
    assert_eq!(
        parse(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok", 0).unwrap(),
        "ok"
    );
    for bytes in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nok".as_slice(),
        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\nContent-Length: 2\n\nok",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n\xff",
    ] {
        assert!(parse(bytes, 0).is_err());
    }
    assert!(parse(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok", 7).is_err());
    assert!(parsers::proc_stat("cpu 9223372036854775807 1 1 1").is_err());
    assert!(parsers::proc_stat("cpu 1 -1 1 1").is_err());
}
