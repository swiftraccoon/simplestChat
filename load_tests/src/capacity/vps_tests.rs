//! Actual retained Python functions are the oracle; every operational boundary
//! is refused or replaced with owned in-memory observations/private fixtures.

use super::*;
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn root() -> PathBuf {
    std::env::var_os("VPS_REFERENCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .into()
        })
}
fn request() -> Value {
    json!({"revision":REVISION,"run":"b".repeat(32),"workload":"meetings","label":"audio30",
        "firstSize":30,"steps":1,"meetingSize":30,"speakers":30,"runtimeSeconds":1200,
        "quick":false,"audioOnly":true,"chatIntervalMs":30000,"serverCpus":1,
        "generatorCpus":4.8,"monthlyPrice":null,"portMbps":2000,"appCpus":null})
}
fn report() -> Value {
    json!({"schemaVersion":1,"measurement":{"meetingSize":30,"browser":{
        "audioOnly":true,"speakers":30,"chatIntervalMs":30000}},
        "images":{"server":{"id":format!("sha256:{}","d".repeat(64)),"revision":REVISION},
            "generator":{"id":format!("sha256:{}","e".repeat(64)),"revision":REVISION}},
        "steps":[{"workload":"meetings","size":30,"valid":true,"passed":true}],
        "ceilings":{"meetings":{"size":30,"lowerBound":true}},"projection":{"lowerBound":true}})
}
fn images() -> Value {
    json!({"serverImage":format!("sha256:{}","d".repeat(64)),
    "generatorImage":format!("sha256:{}","e".repeat(64))})
}
fn inspection() -> Value {
    json!({"Id":"f".repeat(64),"Name":"/capacity-server-fixture","Image":format!("sha256:{}","d".repeat(64)),
        "Config":{"Image":format!("sha256:{}","d".repeat(64)),"Env":["PRIVATE_SENTINEL"]},
        "State":{"Status":"exited","Running":false,"ExitCode":0,"OOMKilled":false,
            "StartedAt":"s","FinishedAt":"f","Secret":"PRIVATE_SENTINEL"}})
}
fn snapshot() -> Value {
    json!({"before":[],"after":["f".repeat(64)],"owned":["f".repeat(64)],
        "journal":{"runId":"c".repeat(16),"containers":[{"name":"capacity-server-fixture",
            "image":format!("sha256:{}","d".repeat(64)),"id":"f".repeat(64)}]},
        "inspections":{"f".repeat(64):[inspection()]}})
}
fn decode_request(value: &Value) -> Result<Request> {
    Request::decode(&serde_json::to_vec(value)?)
}
fn bytes(case: &Value, key: &str) -> Vec<u8> {
    serde_json::to_vec(&case[key]).unwrap()
}
fn native(case: &Value) -> Result<Value> {
    match case["op"].as_str().unwrap() {
        "validate" => {
            let source = case
                .get("raw")
                .and_then(Value::as_str)
                .map_or_else(|| bytes(case, "request"), |v| v.as_bytes().to_vec());
            let request = Request::decode(&source)?;
            Ok(serde_json::from_slice(request.as_json())?)
        }
        "arguments" => Ok(serde_json::to_value(capacity_arguments(
            &decode_request(&case["request"])?,
            &bytes(case, "images"),
        )?)?),
        "assess" => Ok(serde_json::from_slice(
            &assess(&bytes(case, "report"), &decode_request(&case["request"])?)?.to_json(),
        )?),
        "finished" => Ok(unit_finished(&bytes(case, "state"))?.into()),
        "parse-unit" => Ok(serde_json::from_slice(&parse_unit_state(
            case["text"].as_str().unwrap(),
        )?)?),
        "residuals" => {
            let value = &case["snapshot"];
            let strings =
                |key: &str| serde_json::from_value::<Vec<String>>(value[key].clone()).unwrap();
            let snapshot = Snapshot {
                before: strings("before"),
                after: strings("after"),
                owned: strings("owned"),
                journal: value
                    .get("journal")
                    .filter(|value| !value.is_null())
                    .map(|v| serde_json::to_vec(v).unwrap()),
                inspections: value["inspections"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(key, value)| (key.clone(), serde_json::to_vec(value).unwrap()))
                    .collect(),
            };
            Ok(serde_json::from_slice(&residuals(&snapshot)?.to_json())?)
        }
        _ => panic!("unknown fixture"),
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
import json,pathlib,sys,tempfile
root=pathlib.Path(sys.argv[1]);sys.path[:0]=[str(root/'build'),str(root/'ops/ansible/files')]
import vps_capacity_remote as p

def forbidden(*args,**kwargs):raise AssertionError('operational boundary forbidden')
p.command=forbidden;p.container_ids=forbidden
p.subprocess.Popen=forbidden;p.subprocess.run=forbidden
result=[]
for case in json.load(sys.stdin):
 try:
  op=case['op']
  if op=='validate':value=p.validate_request(json.loads(case['raw']) if 'raw'in case else case['request'])
  elif op=='arguments':value=p.capacity_argv(p.validate_request(case['request']),case['images'])[3:]
  elif op=='assess':value=p.assess(case['report'],p.validate_request(case['request']))
  elif op=='finished':value=p.unit_finished(case['state'])
  elif op=='parse-unit':
   p.command=lambda *a,**kw:case['text'];value=p.unit_state('fixture.service');p.command=forbidden
  elif op=='residuals':
   s=case['snapshot']
   with tempfile.TemporaryDirectory(prefix='vps-policy-oracle-') as directory:
    directory=pathlib.Path(directory);(directory/'workload').mkdir()
    if s.get('journal') is not None:(directory/'workload/ownership.json').write_text(json.dumps(s['journal']))
    p.container_ids=lambda **kw:s['owned'] if kw.get('label') else s['after']
    def inspect(args,**kw):
     assert args[:len(p.DOCKER)+1]==p.DOCKER+['inspect']
     return json.dumps(s['inspections'][args[-1]])
    p.command=inspect;value=p.residuals(directory,s['before'])
   p.command=forbidden;p.container_ids=forbidden
  else:raise AssertionError('unknown pure operation')
  result.append({'ok':True,'value':value})
 except p.CapacityControlError as error:result.append({'ok':False,'error':str(error)})
json.dump(result,sys.stdout)
"#;
    let mut child = Command::new("python3")
        .args(["-I", "-c", script])
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
        .write_all(&serde_json::to_vec(cases).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(expected.len(), cases.len());
    for (index, (case, mut expected)) in cases.iter().zip(expected).enumerate() {
        let mut actual = match native(case) {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(error) => json!({"ok":false,"error":error.to_string()}),
        };
        numbers(&mut actual);
        numbers(&mut expected);
        assert_eq!(actual, expected, "case{index}: {case}");
    }
}

#[test]
fn requests_preserve_python_types_error_order_and_operational_bounds() {
    let mut cases = vec![json!({"op":"validate","request":request()})];
    for (key, values) in [
        (
            "revision",
            vec![
                Value::Null,
                json!(false),
                json!("main"),
                json!("A".repeat(40)),
                json!("a".repeat(40)),
            ],
        ),
        (
            "run",
            vec![
                json!("../outside"),
                json!("a".repeat(31)),
                json!("a".repeat(32)),
            ],
        ),
        (
            "label",
            vec![
                json!(""),
                json!("two words"),
                json!("a".repeat(64)),
                json!("a".repeat(65)),
                json!("-a"),
                json!("a_.-Z9"),
            ],
        ),
        (
            "workload",
            vec![
                json!("meetings"),
                json!("large-meeting"),
                json!("webinar"),
                json!("other"),
                Value::Null,
            ],
        ),
        ("quick", vec![json!(true), json!(0), json!(1), Value::Null]),
        (
            "audioOnly",
            vec![json!(false), json!(0), json!(1), Value::Null],
        ),
    ] {
        for value in values {
            let mut input = request();
            input[key] = value;
            cases.push(json!({"op":"validate","request":input}));
        }
    }
    for (key, low, high) in [
        ("firstSize", 2, 10000),
        ("steps", 1, 12),
        ("meetingSize", 2, 64),
        ("speakers", 1, 64),
        ("runtimeSeconds", 120, 21600),
        ("chatIntervalMs", 0, 600000),
    ] {
        for value in [
            json!(-1),
            json!(low - 1),
            json!(low),
            json!(high),
            json!(high + 1),
            json!(true),
            json!(low as f64),
            json!(low.to_string()),
            Value::Null,
        ] {
            let mut input = request();
            input[key] = value;
            cases.push(json!({"op":"validate","request":input}));
        }
    }
    for key in [
        "serverCpus",
        "generatorCpus",
        "appCpus",
        "monthlyPrice",
        "portMbps",
    ] {
        for value in [
            json!(0),
            json!(0.01),
            json!(0.1),
            json!(1),
            json!(256),
            json!(256.1),
            json!(1e6),
            json!(1e6 + 1.0),
            json!(true),
            json!("1"),
            Value::Null,
        ] {
            let mut input = request();
            input[key] = value;
            cases.push(json!({"op":"validate","request":input}));
        }
    }
    for quick in [false, true] {
        for interval in [0, 999, 1000, 28000, 28001, 30000, 58000, 58001, 600001] {
            let mut input = request();
            input["quick"] = quick.into();
            input["chatIntervalMs"] = interval.into();
            cases.push(json!({"op":"validate","request":input}));
        }
    }
    for raw in [
        "NaN",
        "Infinity",
        "-Infinity",
        "1e400",
        "1000000000000000000000000000000000000000000",
    ] {
        for key in ["serverCpus", "firstSize"] {
            let mut input = request();
            input[key] = "PLACEHOLDER".into();
            let input = serde_json::to_string(&input)
                .unwrap()
                .replace("\"PLACEHOLDER\"", raw);
            cases.push(json!({"op":"validate","raw":input}));
        }
    }
    let mut unknown = request();
    unknown["futureField"] = json!({"a":[1,"two",true]});
    cases.push(json!({"op":"validate","request":unknown}));
    compare(&cases);
}

#[test]
fn capacity_arguments_reuse_the_existing_run_contract_for_every_supported_shape() {
    let mut cases = Vec::new();
    for workload in ["meetings", "large-meeting", "webinar"] {
        for audio in [false, true] {
            for quick in [false, true] {
                for cpu in [
                    json!(1),
                    json!(1.0),
                    json!(4.8),
                    json!(0.10000000000000002),
                    Value::Null,
                ] {
                    let mut input = request();
                    input["workload"] = workload.into();
                    input["audioOnly"] = audio.into();
                    input["quick"] = quick.into();
                    input["chatIntervalMs"] = 1000.into();
                    input["serverCpus"] = cpu;
                    cases.push(json!({"op":"arguments","request":input,"images":images()}));
                }
            }
        }
    }
    compare(&cases);
}

#[test]
fn assessment_retains_lower_bounds_but_rejects_foreign_or_invalid_measurements() {
    let mut cases = Vec::new();
    for valid in [json!(true), json!(false), json!(1), Value::Null] {
        for passed in [json!(true), json!(false), json!(1), Value::Null] {
            let mut input = report();
            input["steps"][0]["valid"] = valid.clone();
            input["steps"][0]["passed"] = passed;
            cases.push(json!({"op":"assess","request":request(),"report":input}));
        }
    }
    for value in [
        json!(1),
        json!(1.0),
        json!(true),
        json!(2),
        Value::Null,
        json!("1"),
    ] {
        let mut input = report();
        input["schemaVersion"] = value;
        cases.push(json!({"op":"assess","request":request(),"report":input}));
    }
    for key in ["audioOnly", "speakers", "chatIntervalMs"] {
        for value in [
            json!(0),
            json!(1),
            json!(true),
            json!(false),
            Value::Null,
            json!("30"),
        ] {
            let mut input = report();
            input["measurement"]["browser"][key] = value;
            cases.push(json!({"op":"assess","request":request(),"report":input}));
        }
    }
    for role in ["server", "generator"] {
        for key in ["id", "revision"] {
            for value in [
                Value::Null,
                json!(false),
                json!("wrong"),
                json!("sha256:".to_owned() + &"D".repeat(64)),
            ] {
                let mut input = report();
                input["images"][role][key] = value;
                cases.push(json!({"op":"assess","request":request(),"report":input}));
            }
        }
    }
    for steps in [
        json!([]),
        json!([{"workload":"webinar","valid":true,"passed":true}]),
        json!([false]),
        json!([{"workload":"meetings","valid":true,"passed":true},
            {"workload":"meetings","valid":false,"passed":false,"privateObservation":"retained"}]),
    ] {
        let mut input = report();
        input["steps"] = steps;
        cases.push(json!({"op":"assess","request":request(),"report":input}));
    }
    for audio in [false, true] {
        let mut req = request();
        req["audioOnly"] = audio.into();
        let mut input = report();
        input["measurement"]["browser"]["audioOnly"] =
            if audio { 1.0.into() } else { (-0.0).into() };
        cases.push(json!({"op":"assess","request":req,"report":input}));
    }
    compare(&cases);
}

#[test]
fn terminal_unit_recognition_keeps_retained_success_and_missing_state_separate() {
    let mut cases = Vec::new();
    for active in [
        "inactive",
        "failed",
        "active",
        "activating",
        "deactivating",
        "reloading",
        "other",
    ] {
        for sub in ["exited", "running", "dead"] {
            for pid in [json!("0"), json!("123"), json!(0), Value::Null] {
                cases.push(json!({"op":"finished","state":{"ActiveState":active,"SubState":sub,"MainPID":pid}}));
            }
        }
    }
    for text in [
        "LoadState=not-found\nActiveState=inactive\nMainPID=0\n",
        "LoadState=loaded\nActiveState=active\nSubState=exited\nMainPID=0\nResult=success\n",
        "",
        "Failed to connect to bus",
        "LoadState=not-found\n",
        "LoadState=other\nActiveState=inactive\nMainPID=0\n",
        "LoadState=loaded\nLoadState=not-found\nActiveState=inactive\nMainPID=0\n=ignored\n",
    ] {
        cases.push(json!({"op":"parse-unit","text":text}));
    }
    for separator in [
        "\n", "\r", "\r\n", "\u{b}", "\u{c}", "\u{1c}", "\u{1d}", "\u{1e}", "\u{85}", "\u{2028}",
        "\u{2029}",
    ] {
        cases.push(json!({"op":"parse-unit","text":format!(
            "LoadState=loaded{separator}ActiveState=inactive{separator}MainPID=0")}));
    }
    compare(&cases);
}

#[test]
fn ownership_requires_each_exact_observation_and_redacts_unrelated_inspection_fields() {
    let mut cases = vec![json!({"op":"residuals","snapshot":snapshot()})];
    for name in [
        "capacity-server-fixture",
        "/capacity-server-fixture",
        "//capacity-server-fixture",
        "/foreign",
    ] {
        for owned in [true, false] {
            for identity in [Value::Null, json!("f".repeat(64)), json!("1".repeat(64))] {
                let mut input = snapshot();
                input["inspections"]["f".repeat(64)][0]["Name"] = name.into();
                input["journal"]["containers"][0]["id"] = identity;
                if !owned {
                    input["owned"] = json!([]);
                }
                cases.push(json!({"op":"residuals","snapshot":input}));
            }
        }
    }
    for key in ["Image", "Id", "Config", "State"] {
        for value in [Value::Null, json!("wrong"), json!(false)] {
            let mut input = snapshot();
            input["inspections"]["f".repeat(64)][0][key] = value;
            cases.push(json!({"op":"residuals","snapshot":input}));
        }
    }
    let mut none = snapshot();
    none["journal"] = Value::Null;
    cases.push(json!({"op":"residuals","snapshot":none}));
    let mut foreign = snapshot();
    foreign["journal"]["containers"][0]["image"] = json!("foreign");
    cases.push(json!({"op":"residuals","snapshot":foreign}));
    let mut resolved = snapshot();
    resolved["inspections"]["f".repeat(64)][0]["Image"] = json!("different-resolved-platform");
    cases.push(json!({"op":"residuals","snapshot":resolved}));
    let mut clear = snapshot();
    clear["before"] = json!(["f".repeat(64)]);
    clear["owned"] = json!([]);
    cases.push(json!({"op":"residuals","snapshot":clear}));
    let mut duplicate = snapshot();
    duplicate["journal"]["containers"] = json!([
        {"name":"capacity-server-fixture","image":"foreign"},
        {"name":"capacity-server-fixture","image":format!("sha256:{}","d".repeat(64)),"id":null}]);
    cases.push(json!({"op":"residuals","snapshot":duplicate}));
    for (expected, actual) in [
        (json!(9007199254740993_u64), json!(9007199254740992_u64)),
        (json!(9007199254740993_u64), json!(9007199254740992.0)),
        (json!(9007199254740992_u64), json!(9007199254740992.0)),
        (json!(0), json!(-0.0)),
        (json!(false), json!(0.0)),
    ] {
        let mut input = snapshot();
        input["journal"]["containers"][0]["image"] = expected;
        input["inspections"]["f".repeat(64)][0]["Config"]["Image"] = actual;
        cases.push(json!({"op":"residuals","snapshot":input}));
    }
    compare(&cases);
    let output = native(&cases[0]).unwrap().to_string();
    assert!(!output.contains("PRIVATE_SENTINEL"));
    assert!(!output.contains("Env"));
}

#[test]
fn request_roundtrip_and_bounds_are_explicit_without_starting_any_operation() {
    let mut value = request();
    value["future"] = json!({"nested":[1,"two"]});
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    let request = Request::decode(&bytes).unwrap();
    assert_eq!(request.as_json(), bytes);
    assert_eq!(request.revision(), REVISION);
    assert_eq!(request.run_id(), "b".repeat(32));
    assert_eq!(request.workload(), "meetings");
    assert_eq!(
        Request::decode(&vec![b' '; MAX_REQUEST + 1])
            .unwrap_err()
            .to_string(),
        "remote_request_too_large"
    );
    assert!(assess(&vec![b' '; MAX_JSON + 1], &request).is_err());
    let assessment = assess(&serde_json::to_vec(&report()).unwrap(), &request).unwrap();
    assert!(assessment.passed);
    assert_eq!(assessment.passing_steps(), 1);
    assert_eq!(assessment.failed_steps(), 0);
    assert_eq!(
        parse_unit_state(&" ".repeat(2 * MAX_JSON + 1))
            .unwrap_err()
            .to_string(),
        "unit_response_too_large"
    );
    let snapshot = Snapshot::default();
    let residual = residuals(&snapshot).unwrap();
    assert!(residual.clean);
}

#[test]
fn unsupported_unicode_and_nesting_are_explicit_decoder_refusals() {
    let mut request = request();
    request["future"] = json!("PRIVATE_PLACEHOLDER");
    let source = serde_json::to_string(&request).unwrap();
    for replacement in [
        "\"\\ud800\"".to_owned(),
        format!("{}0{}", "[".repeat(160), "]".repeat(160)),
    ] {
        let source = source.replace("\"PRIVATE_PLACEHOLDER\"", &replacement);
        assert_eq!(
            Request::decode(source.as_bytes()).unwrap_err().to_string(),
            "invalid_json"
        );
    }
}

#[test]
fn malformed_nested_nan_image_values_do_not_confirm_ownership() {
    let id = "f".repeat(64);
    let mut journal = snapshot()["journal"].clone();
    journal["containers"][0]["image"] = json!("NAN_PLACEHOLDER");
    let mut inspection = inspection();
    inspection["Config"]["Image"] = json!("NAN_PLACEHOLDER");
    let encode = |value| {
        serde_json::to_string(&value)
            .unwrap()
            .replace("\"NAN_PLACEHOLDER\"", "[NaN]")
            .into_bytes()
    };
    let snapshot = Snapshot {
        after: vec![id.clone()],
        owned: vec![id.clone()],
        journal: Some(encode(journal)),
        inspections: BTreeMap::from([(id, encode(json!([inspection])))]),
        ..Snapshot::default()
    };
    let residuals = residuals(&snapshot).unwrap();
    assert!(!residuals.clean);
    let value = json::parse(&residuals.to_json()).unwrap();
    let Json::Array(details) = &value["residuals"] else {
        panic!("expected residuals")
    };
    assert!(matches!(details[0]["confirmedOwned"], Json::Bool(false)));
}
