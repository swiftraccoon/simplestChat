//! Inert policy fixtures plus the actual maintained presentation owner oracle.

use super::*;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn profile(settings: &[(&str, &str)]) -> Profile {
    Profile::parse(
        &settings
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect(),
    )
    .unwrap()
}
fn presentation() -> Profile {
    profile(&[
        ("IMPAIR_UDP_PORT", "41010"),
        ("IMPAIR_DELAY_MS", "30"),
        ("IMPAIR_RATE_KBIT", "800"),
    ])
}
fn root() -> Value {
    json!({"kind":"prio","handle":"1:","root":true,"refcnt":2,
        "options":{"bands":3,"priomap":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"multiqueue":false}})
}
fn netem(profile: &Profile) -> Value {
    let mut options = json!({"limit":200000,"ecn":false,"gap":0});
    if profile.delay_ms != 0.0 {
        options["delay"] = json!({"delay":profile.delay_ms/1000.0,
            "jitter":profile.jitter_ms/1000.0,"correlation":0});
    }
    if profile.loss != 0.0 {
        options["loss-random"] = json!({"loss":profile.loss/100.0,"correlation":0});
    }
    if profile.rate_kbit != 0.0 {
        options["rate"] = json!({"rate":(profile.rate_kbit*125.0).floor(),
            "packetoverhead":0,"cellsize":0,"celloverhead":0});
    }
    json!({"kind":"netem","handle":"30:","parent":"1:3","options":options})
}
fn filters(profile: &Profile, priorities: &[u8]) -> String {
    let mut output = Vec::new();
    for priority in priorities {
        let span = u32::from(profile.workers).next_power_of_two();
        let (value, mask) = if *priority == 1 {
            (u32::from(profile.port) << 16, (65536 - span) << 16)
        } else {
            (u32::from(profile.port), 65536 - span)
        };
        output.extend([
            format!(r#"{{"protocol":"ip","pref":{priority},"kind":"u32","chain":0}}"#),
            format!(r#"{{"protocol":"ip","pref":{priority},"kind":"u32","chain":0,"options":{{"fh":"800:","ht_divisor":1}}}}"#),
            format!(
                concat!(
                    "{{\"protocol\":\"ip\",\"pref\":{},\"kind\":\"u32\",\"chain\":0,",
                    "\"options\":{{\"fh\":\"800::800\",\"order\":2048,\"key_ht\":\"800\",",
                    "\"bkt\":\"0\",\"flowid\":\"1:3\",\"not_in_hw\":true,",
                    "\"match\":{{\"value\":\"110000\",\"mask\":\"ff0000\",\"offmask\":\"\",\"off\":8}},",
                    "\"match\":{{\"value\":\"{:x}\",\"mask\":\"{:x}\",\"offmask\":\"\",\"off\":20}}}}}}"
                ),
                priority, value, mask
            ),
        ]);
    }
    format!("[{}]", output.join(","))
}
fn state(qdiscs: &Value, filters: &str, profile: &Profile) -> State {
    inspect(
        &serde_json::to_vec(qdiscs).unwrap(),
        filters.as_bytes(),
        profile,
    )
    .unwrap()
}

#[test]
fn profile_defaults_bounds_and_literal_additions() {
    let defaults = profile(&[]);
    assert_eq!(defaults.port, 41100);
    assert_eq!(defaults.workers, 1);
    assert_eq!(defaults.direction, Direction::Downlink);
    assert_eq!(defaults.additions().len(), 3);
    assert_eq!(defaults.additions()[1].last().unwrap(), "200000");
    let empty = profile(&[
        ("IMPAIR_UDP_PORT", ""),
        ("IMPAIR_WORKERS", ""),
        ("IMPAIR_DIRECTION", ""),
        ("IMPAIR_LOSS", ""),
        ("IMPAIR_DELAY_MS", ""),
        ("IMPAIR_JITTER_MS", ""),
        ("IMPAIR_RATE_KBIT", ""),
    ]);
    assert_eq!(
        serde_json::to_value(empty).unwrap(),
        serde_json::to_value(defaults).unwrap()
    );
    let p = profile(&[
        ("IMPAIR_LOSS", "5"),
        ("IMPAIR_DELAY_MS", "50"),
        ("IMPAIR_JITTER_MS", "10"),
        ("IMPAIR_DIRECTION", "both"),
        ("IMPAIR_WORKERS", "3"),
        ("IMPAIR_UDP_PORT", "41100"),
    ]);
    assert_eq!(
        p.additions()[1],
        [
            "qdisc", "add", "dev", "lo", "parent", "1:3", "handle", "30:", "netem", "limit",
            "200000", "loss", "5%", "delay", "50ms", "10ms",
        ]
    );
    assert_eq!(p.additions().len(), 4);
    let incoming = &p.additions()[3];
    assert_eq!(
        &incoming[incoming.len() - 4..],
        ["41100", "0xfffc", "flowid", "1:3"]
    );
    for (key, value) in [
        ("IMPAIR_LOSS", "NaN"),
        ("IMPAIR_LOSS", "101"),
        ("IMPAIR_LOSS", "-0"),
        ("IMPAIR_DELAY_MS", "1;id"),
        ("IMPAIR_DELAY_MS", "60001"),
        ("IMPAIR_DELAY_MS", "0.000001"),
        ("IMPAIR_JITTER_MS", "1"),
        ("IMPAIR_RATE_KBIT", "0.001"),
        ("IMPAIR_RATE_KBIT", "10000001"),
        ("IMPAIR_RATE_KBIT", "1e3"),
        ("IMPAIR_WORKERS", "0"),
        ("IMPAIR_WORKERS", "5"),
        ("IMPAIR_UDP_PORT", "0"),
        ("IMPAIR_UDP_PORT", "65536"),
        ("IMPAIR_DIRECTION", "both;id"),
        ("IMPAIR_LOSS", "0.0000001"),
    ] {
        let map = BTreeMap::from([(key.into(), value.into())]);
        assert_eq!(
            Profile::parse(&map).unwrap_err().to_string(),
            INVALID_PROFILE
        );
    }
    for workers in 1..=4 {
        for port in [1, 2, 3, 4, 41100, 65532, 65534, 65535] {
            let map = BTreeMap::from([
                ("IMPAIR_WORKERS".into(), workers.to_string().into()),
                ("IMPAIR_UDP_PORT".into(), port.to_string().into()),
            ]);
            let span = (workers as u32).next_power_of_two();
            assert_eq!(
                Profile::parse(&map).is_ok(),
                (port as u32).is_multiple_of(span)
            );
        }
    }
}

#[test]
fn all_prefixes_require_exact_direction_port_mask_and_profile() {
    for workers in 1..=4 {
        for direction in ["downlink", "both"] {
            let p = profile(&[
                ("IMPAIR_LOSS", "5"),
                ("IMPAIR_DELAY_MS", "50"),
                ("IMPAIR_JITTER_MS", "10"),
                ("IMPAIR_DIRECTION", direction),
                ("IMPAIR_WORKERS", &workers.to_string()),
            ]);
            let qdiscs = json!([root(), netem(&p)]);
            assert!(state(&json!([root()]), "[]", &p).owned_shape);
            assert!(!state(&json!([root()]), "[]", &p).complete);
            assert!(state(&qdiscs, "[]", &p).owned_shape);
            let first = state(&qdiscs, &filters(&p, &[1]), &p);
            assert!(first.owned_shape);
            assert_eq!(first.complete, direction == "downlink");
            if direction == "both" {
                assert!(state(&qdiscs, &filters(&p, &[1, 2]), &p).complete);
                assert!(!state(&qdiscs, &filters(&p, &[2]), &p).owned_shape);
            }
            let mut foreign = netem(&p);
            foreign["options"]["loss-random"]["loss"] = json!(5);
            assert!(!state(&json!([root(), foreign]), &filters(&p, &[1]), &p).owned_shape);
            let wrong = filters(&p, &[1]).replace("110000", "60000");
            assert!(!state(&qdiscs, &wrong, &p).owned_shape);
            let wrong = filters(&p, &[1]).replace("\"off\":20", "\"off\":22");
            assert!(!state(&qdiscs, &wrong, &p).owned_shape);
        }
    }
}

#[test]
fn every_repeated_selector_and_decoded_duplicate_is_accounted_for() {
    let p = presentation();
    let qdiscs = serde_json::to_vec(&json!([root(), netem(&p)])).unwrap();
    let native = filters(&p, &[1]);
    assert!(inspect(&qdiscs, native.as_bytes(), &p).unwrap().complete);
    let escaped = native.replacen("\"match\"", "\"m\\u0061tch\"", 1);
    assert!(inspect(&qdiscs, escaped.as_bytes(), &p).unwrap().complete);
    for wire in [
        native.replace("\"match\"", "\"fixtureMatch0\""),
        native.replace("\"match\"", "\"fixture\\u004datc\\u00680\""),
        native.replace(
            "\"kind\":\"u32\"",
            "\"kind\":\"bpf\",\"k\\u0069nd\":\"u32\"",
        ),
    ] {
        assert_eq!(
            inspect(&qdiscs, wire.as_bytes(), &p)
                .unwrap_err()
                .to_string(),
            INVALID_STATE
        );
    }
    let extra = native.replace(
        "\"flowid\":\"1:3\"",
        "\"flowid\":\"1:3\",\"match\":{\"value\":\"bad\"}",
    );
    assert!(!inspect(&qdiscs, extra.as_bytes(), &p).unwrap().owned_shape);
    for invalid in [
        b"null".to_vec(),
        b"[null]".to_vec(),
        b"[] []".to_vec(),
        vec![b' '; MAX_OUTPUT + 1],
        serde_json::to_vec(&vec![json!({}); 17]).unwrap(),
        format!("[{{\"x\":{}0{}}}]", "[".repeat(13), "]".repeat(13)).into_bytes(),
        format!("[{{\"x\":[{}]}}]", vec!["0"; MAX_VALUES].join(",")).into_bytes(),
        b"[{\"x\":1e400}]".to_vec(),
    ] {
        assert_eq!(
            inspect(&invalid, b"[]", &p).unwrap_err().to_string(),
            INVALID_STATE
        );
    }
}

#[test]
fn signatures_exclude_counters_but_bind_all_projected_state_and_selectors() {
    let p = presentation();
    let initial = json!([root(), netem(&p)]);
    let wire = filters(&p, &[1]);
    let original = state(&initial, &wire, &p);
    let mut counters = initial.clone();
    counters[0]["refcnt"] = json!(9000);
    counters[0]["bytes"] = json!(23456);
    counters.as_array_mut().unwrap().reverse();
    assert_eq!(state(&counters, &wire, &p).signature, original.signature);
    assert_ne!(
        state(&initial, &wire.replace("800::800", "800::801"), &p).signature,
        original.signature
    );
    assert_ne!(
        state(&initial, &wire.replace("110000", "60000"), &p).signature,
        original.signature
    );
    assert_eq!(original.signature.len(), 64);
    assert!(
        !serde_json::to_string(&original.evidence)
            .unwrap()
            .contains("800::")
    );
}

#[test]
fn zero_and_quantized_profiles_use_the_explicit_iproute2_units() {
    for settings in [
        vec![],
        vec![("IMPAIR_LOSS", "100")],
        vec![("IMPAIR_LOSS", "0.000001"), ("IMPAIR_RATE_KBIT", "0.008")],
        vec![("IMPAIR_DELAY_MS", "60000"), ("IMPAIR_JITTER_MS", "60000")],
        vec![
            ("IMPAIR_DELAY_MS", "50.123456"),
            ("IMPAIR_RATE_KBIT", "10000000"),
        ],
    ] {
        let p = profile(&settings);
        assert!(state(&json!([root(), netem(&p)]), &filters(&p, &[1]), &p).complete);
    }
    let p = profile(&[
        ("IMPAIR_LOSS", "33.333333"),
        ("IMPAIR_DELAY_MS", "50.123456"),
    ]);
    let mut observed = netem(&p);
    // iproute2 v6.1's %g JSON printer emits six significant digits; the older
    // kernel time path also quantizes to microseconds.
    observed["options"]["loss-random"]["loss"] = json!(0.333333);
    observed["options"]["delay"]["delay"] = json!(0.050123);
    assert!(state(&json!([root(), observed.clone()]), &filters(&p, &[1]), &p).complete);
    observed["options"]["loss-random"]["loss"] = json!(33.333333);
    assert!(!state(&json!([root(), observed]), &filters(&p, &[1]), &p).owned_shape);
}

fn reference(cases: &Value) -> Value {
    let root = std::env::var_os("BENCH_REFERENCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_owned()
        });
    let script = r#"
const fs=require('node:fs');
const {createPresentationImpairment}=require(process.argv[1]);
const cases=JSON.parse(fs.readFileSync(0,'utf8'));
const output=cases.map(({qdisc,filter,afterQdisc,afterFilter})=>{
  let reads=0;const commands=[],records=[];
  const owner=createPresentationImpairment({port:'41010',artifacts:'/unused',
    execute(command,args){
      if(command==='tc'){
        reads++;
        if(reads<=2)return '[]';
        if(reads>4 && afterQdisc)return args[1]==='qdisc'?afterQdisc:afterFilter;
        return args[1]==='qdisc'?qdisc:filter;
      }
      commands.push(args.slice(1));return '';
    },persist(value){records.splice(0,records.length,...structuredClone(value));}
  });
  let applied=true;try{owner('apply')}catch{applied=false}
  let cleared=true;try{owner('clear')}catch{cleared=false}
  return {applied,cleared,after:records[0].after,commands};
});
process.stdout.write(JSON.stringify(output));
"#;
    let mut child = Command::new("node")
        .arg("-e")
        .arg(script)
        .arg(root.join("web/e2e/presentation-impairment.cjs"))
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
    assert!(output.status.success(), "inert presentation oracle failed");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn actual_presentation_owner_agrees_on_commands_shapes_and_signature_changes() {
    let p = presentation();
    let qdisc = json!([root(), netem(&p)]);
    let filter = filters(&p, &[1]);
    let mut cases = vec![
        json!({"qdisc":"[]","filter":"[]"}),
        json!({"qdisc":json!([root()]).to_string(),"filter":"[]"}),
        json!({"qdisc":qdisc.to_string(),"filter":"[]"}),
        json!({"qdisc":qdisc.to_string(),"filter":filter}),
    ];
    for change in ["rate", "delay", "loss", "selector", "extra"] {
        let mut q = qdisc.clone();
        let mut f = filter.clone();
        match change {
            "rate" => q[1]["options"]["rate"]["rate"] = json!(800000),
            "delay" => q[1]["options"]["delay"]["delay"] = json!(30),
            "loss" => q[1]["options"]["loss-random"] = json!({"loss":0.01}),
            "selector" => f = f.replace("110000", "60000"),
            "extra" => f = f.replace("\"order\":2048", "\"action\":\"foreign\",\"order\":2048"),
            _ => unreachable!(),
        }
        cases.push(json!({"qdisc":q.to_string(),"filter":f}));
    }
    let output = reference(&json!(cases));
    for (case, expected) in cases.iter().zip(output.as_array().unwrap()) {
        let observed = inspect(
            case["qdisc"].as_str().unwrap().as_bytes(),
            case["filter"].as_str().unwrap().as_bytes(),
            &p,
        )
        .unwrap();
        assert_eq!(observed.evidence, expected["after"]);
        assert_eq!(observed.complete, expected["applied"].as_bool().unwrap());
    }
    assert_eq!(
        output[3]["commands"].as_array().unwrap()[..3],
        p.additions()
            .iter()
            .map(|args| json!(args))
            .collect::<Vec<_>>()
    );
    let mut changed = qdisc.clone();
    changed[0]["bytes"] = json!(9999);
    let unchanged = json!({"qdisc":qdisc.to_string(),"filter":filter,
        "afterQdisc":changed.to_string(),"afterFilter":filter});
    let changed = json!({"qdisc":qdisc.to_string(),"filter":filter,
        "afterQdisc":qdisc.to_string(),"afterFilter":filter.replace("800::800","800::801")});
    let results = reference(&json!([unchanged, changed]));
    assert_eq!(results[0]["commands"].as_array().unwrap().len(), 4);
    assert_eq!(results[1]["commands"].as_array().unwrap().len(), 3);
}
