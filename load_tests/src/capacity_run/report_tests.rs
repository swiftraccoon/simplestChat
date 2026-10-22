//! Full report bytes and summaries are checked against the retained Python API.

use super::{
    context::Context,
    fixtures::{Directory, FakeClock, FakeEngine},
    host, json,
    options::Options,
    ownership::Ownership,
    policy_tests::oracle_bytes,
    process::Cancellation,
    report, search,
    types::*,
};
use std::sync::Arc;

#[test]
fn generated_reports_projection_cost_recommendations_and_framing_match_reference() {
    let directory = Directory::new();
    let engine = FakeEngine::new(Arc::new(FakeClock::default()));
    let cancellation = Cancellation::default();
    let ownership = Ownership::new(&directory.0, "fixture");
    for index in 0..36 {
        let options = Options {
            meeting_size: if index % 2 == 0 { 5 } else { 3 },
            browser: Browser {
                audio_only: index % 3 == 0,
                speakers: 2,
                chat_interval_ms: if index % 2 == 0 { 0 } else { 3000 },
                ..Browser::default()
            },
            monthly_price: if index % 3 == 0 {
                None
            } else {
                Some(if index % 3 == 1 { 0.0 } else { 14.6 })
            },
            egress_price_per_gb: 0.01,
            included_egress_tb: 1.5,
            port_mbps: if index % 2 == 0 { 100.0 } else { 0.0 },
            worker_threshold: if index % 2 == 0 { 0.7 } else { 0.85 },
            app_cpus: Some(4.5),
            app_memory_mib: Some(if index % 2 == 0 { 512 } else { 16384 }),
            label: if index % 2 == 0 {
                "é host".into()
            } else {
                String::new()
            },
            ..Options::default()
        };
        let host = host::Host {
            cpu_model: "Fixture CPU".into(),
            platform: "Fixture Model".into(),
            logical_cpus: 6,
            memory_mib: 16384,
            idle_busy_share: 0.02,
            idle_steal_share: 0.0,
            rmem_max: if index % 2 == 0 { 212992 } else { 2097152 },
            kernel: "fixture-kernel".into(),
        };
        let images = host::Images {
            server: host::Image {
                reference: "srv".into(),
                id: "id-s".into(),
                revision: "s".into(),
            },
            generator: host::Image {
                reference: "gen".into(),
                id: "id-g".into(),
                revision: "g".into(),
            },
        };
        let deployment = report::Deployment::new(&host, &options).unwrap();
        let shapes = Workload::ALL
            .into_iter()
            .map(|w| (w, search::shape(w, 6, deployment.workers, None, None)))
            .collect();
        let env = vec![
            ("RUST_LOG".into(), "warn".into()),
            ("MEDIA_DIAGNOSTICS".into(), "true".into()),
            ("RUST_LOG".into(), "trace".into()),
        ];
        let context = Context {
            engine: &engine,
            clock: engine.clock.as_ref(),
            cancellation: &cancellation,
            ownership: &ownership,
            run_id: "fixture",
            server_image: "srv",
            generator_image: "gen",
            revisions: ("s".into(), "g".into()),
            shape: Shape {
                server_cpus: 1.0,
                workers: 1,
                generator_cpus: 4.8,
            },
            worker_threshold: options.worker_threshold,
            server_memory: "4096m".into(),
            generator_memory: "8192m".into(),
            browser: &options.browser,
            output: &directory.0,
            server_env: &env,
        };
        let mut results = report::Results::new();
        let mut ceilings = report::Ceilings::new();
        for (offset, workload) in Workload::ALL.into_iter().enumerate() {
            let size = 40 + offset as i64 * 10;
            let plan = Plan {
                workload,
                size,
                meeting_size: options.meeting_size,
                warmup_seconds: 20,
                duration_seconds: 30,
                workers: 2,
            };
            let mut step =
                StepResult::new(plan, &options.browser, "2026-10-21T12:00:00+00:00".into());
            step.generator_passed = true;
            step.worker_load_peak.insert(0, 0.5);
            step.worker_load_mean.insert(0, 0.4);
            step.generator_cores = 1.2;
            step.memory_peak_bytes = (index + 1) * 16 * 1048576;
            step.received_packets = 100000;
            step.received_payload_bytes = 20000000;
            step.measurement_seconds = 30.0;
            let bound = [
                "measured",
                "step-limited",
                "generator-limited",
                "interrupted",
                "below-first-size",
                "not-run",
            ][(index as usize + offset) % 6];
            ceilings.insert(
                workload,
                Ceiling {
                    workload,
                    ceiling: if ["not-run", "below-first-size"].contains(&bound) {
                        None
                    } else {
                        Some(size)
                    },
                    bound: bound.into(),
                    busiest_worker: 0.5,
                    egress_mbps: step.egress_mbps(),
                    memory_peak_bytes: step.memory_peak_bytes,
                    workers: 2,
                    limit: if bound == "measured" {
                        "worker guard".into()
                    } else {
                        String::new()
                    },
                },
            );
            results.insert(
                workload,
                vec![(
                    step,
                    Verdict {
                        passed: true,
                        valid: true,
                        reasons: Vec::new(),
                    },
                )],
            );
        }
        let limits = Limits {
            worker_load: options.worker_threshold,
            ..Limits::default()
        };
        let record = report::build(
            &options,
            report::Measured {
                context: &context,
                host: &host,
                images: &images,
                deployment: &deployment,
                shapes: &shapes,
                limits,
            },
            &results,
            &ceilings,
        )
        .unwrap();
        #[derive(serde::Serialize)]
        struct Input<'a, T: serde::Serialize> {
            report: &'a T,
            options: serde_json::Value,
        }
        let input = Input {
            report: &record,
            options: serde_json::json!({"meeting_size":options.meeting_size,
            "label":options.label,"monthly_price":options.monthly_price,"egress_price_per_gb":options.egress_price_per_gb,
            "included_egress_tb":options.included_egress_tb,"port_mbps":options.port_mbps}),
        };
        let expected = oracle_bytes(
            r#"
from pathlib import Path
from dataclasses import fields
v=json.load(sys.stdin);r=v['report'];m=r['measurement']
def snake(k): return re.sub(r'[A-Z]',lambda m:'_'+m[0].lower(),k)
def record(cls,v):return cls(**{snake(k):v for k,v in v.items() if snake(k) in {f.name for f in fields(cls)}})
options=c.Options(**v['options'])
browser=record(c.Browser,m['browser']);shapes={w:record(c.Shape,s) for w,s in m['shapes'].items()}
context=c.Context(c.Engine('fake'),'fixture','srv','gen',('s','g'),shapes['meetings'],m['limits']['workerLoad'],m['serverMemory'],m['generatorMemory'],browser,Path('.'),lambda _:None,tuple(m['serverEnv'].items()))
limits=record(c.Limits,m['limits']);deployment=record(c.Deployment,r['deployment'])
results={}
for s in r['steps']:
 results.setdefault(s['workload'],[]).append((record(c.StepResult,s),c.Verdict(s['passed'],s['valid'],tuple(s['reasons']))))
ceilings={w:record(c.Ceiling,s) for w,s in r['ceilings'].items()}
measured=c.Measured(r['host'],r['images'],context,limits,deployment,shapes)
result=c.build_report(options,measured,results,ceilings);result['finishedAt']=r['finishedAt']
print(json.dumps({'report':json.dumps(result,indent=2),'summary':c.summary_lines(result)}))
"#,
            &json::bytes(&input).unwrap(),
        );
        assert_eq!(
            String::from_utf8(json::bytes(&record).unwrap()).unwrap(),
            expected["report"].as_str().unwrap(),
            "report case {index}"
        );
        assert_eq!(
            serde_json::to_value(report::summary(&record)).unwrap(),
            expected["summary"],
            "summary case {index}"
        );
    }
}
