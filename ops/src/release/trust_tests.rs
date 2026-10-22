//! Invoke actual retained pure validators. Every subprocess/host boundary inside
//! the imported Python helpers is forbidden; journal fixtures use private files.

use super::*;
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn selection() -> Selection {
    Selection::new("owner/repo", &"a".repeat(40), 789, 789, 1).unwrap()
}
fn predicate() -> Value {
    json!({"schemaVersion":1,"repository":"owner/repo","revision":"a".repeat(40),
        "runId":789,"runAttempt":1,"workflow":WORKFLOW,"ref":REF,"event":"push",
        "platform":"linux/amd64","imageId":format!("sha256:{}","b".repeat(64)),
        "securityPassed":true,"fileDigests":BOUND_FILES.into_iter().map(|k|(k,"c".repeat(64)))
            .collect::<BTreeMap<_,_>>()})
}
fn proof() -> Value {
    json!({"predicate":predicate(),"bundleSha256":"d".repeat(64),
    "verificationSha256":SUBJECTS.into_iter().map(|k|(k,"e".repeat(64))).collect::<BTreeMap<_,_>>()})
}
fn envelope() -> Value {
    json!({"schemaVersion":1,"repository":"owner/repo","revision":"a".repeat(40),
    "artifactId":123,"buildRunId":789,"ciRunId":789,"runAttempt":1,"artifactZipBytes":100,
    "zipSha256":"f".repeat(64),"attestation":proof()})
}
fn receipt() -> Value {
    json!({"schemaVersion":1,"status":"complete","passed":true,"settled":true,
    "phase":"complete","failureClass":null,"evidence":format!(
        "/srv/simplestchat-public/releases/{}/download.fixture","a".repeat(40)),
    "revision":"a".repeat(40),"artifactId":123,"zipSha256":"f".repeat(64),"artifactZipBytes":100,
    "downloadedBytes":100,"archiveSha256":"c".repeat(64),"manifestSha256":"c".repeat(64),
    "elapsedSeconds":1.25,"finalized":true})
}
fn helpers() -> Value {
    json!(
        receipts::BASE
            .into_iter()
            .map(|k| (k, "a".repeat(64)))
            .collect::<BTreeMap<_, _>>()
    )
}
fn project<T: Serialize>(document: Document<T>) -> Result<Value, Error> {
    serde_json::to_value(document.value()).map_err(|_| Error::Json)
}
fn native(case: &Value) -> Result<Value, Error> {
    let bytes = case.get("raw").and_then(Value::as_str).map_or_else(
        || serde_json::to_vec(&case["value"]).unwrap(),
        |s| s.as_bytes().to_vec(),
    );
    match case["op"].as_str().unwrap() {
        "predicate" => project(Predicate::decode(&bytes, &selection())?),
        "proof" => project(Proof::decode(&bytes, &selection())?),
        "envelope" => project(Envelope::decode(&bytes)?),
        "line" => project(Envelope::decode_line(&bytes)?),
        "helpers" => project(HelperSet::decode(&bytes)?),
        "preflight_producer" => {
            let helpers = HelperSet::decode(&bytes)?;
            let receipt = json!({"schemaVersion":1,"runId":"b".repeat(32),
                "prepared":helpers.value().prepared()});
            project(PreflightReceipt::decode(
                &serde_json::to_vec(&receipt).unwrap(),
                helpers.value(),
            )?)
        }
        "journal" => project(FinalizedJournal::decode(&bytes)?),
        "receipt" => project(ReceiverReceipt::decode(
            &bytes,
            Envelope::decode(&serde_json::to_vec(&envelope()).unwrap())?.value(),
        )?),
        _ => panic!("unknown pure operation"),
    }
}
fn compare(cases: &[Value]) {
    let script = r#"
import io,json,pathlib,sys,tempfile,subprocess,hashlib,types
from unittest.mock import patch
root=pathlib.Path(sys.argv[1]);sys.path[:0]=[str(root/'build'),str(root/'ops/ansible/files')]
import release_trust as t,release_fetch_receiver as r,release_fetch_controller as c,release_preflight as p
from release_json import decode_json,DuplicateJsonError

def forbidden(*args,**kwargs):raise AssertionError('operational boundary forbidden')
subprocess.Popen=forbidden;subprocess.run=forbidden
context=json.loads(sys.argv[2]);selected=json.loads(sys.argv[3]);results=[]
for case in json.load(sys.stdin):
 try:
  value=decode_json(case['raw']) if 'raw'in case else case['value']
  op=case['op']
  if op=='predicate': result=t.predicate(value,context)
  elif op=='proof':result=t.proof(value,context)
  elif op=='envelope':result=r.validate_envelope(value)
  elif op=='line': result=r.validate_envelope(r.read_line(io.BytesIO(case['raw'].encode())))
  elif op=='helpers':result=p._expected_helpers(value)
  elif op=='preflight_producer':
   with tempfile.TemporaryDirectory(prefix='ops-preflight-oracle-') as directory:
    directory=pathlib.Path(directory);expected={}
    for name in value:
     content=b'owned public fixture';(directory/name).write_bytes(content)
     expected[name]=hashlib.sha256(content).hexdigest()
    with patch.object(p,'HELPERS',directory),patch.object(p,'protected',lambda *a,**kw:None), \
         patch.object(p.os,'geteuid',lambda:0),patch.object(p.platform,'system',lambda:'Linux'), \
         patch.object(p.platform,'machine',lambda:'x86_64'), \
         patch.object(p.platform,'freedesktop_os_release',lambda:{'ID':'debian','VERSION_ID':'13'}), \
         patch.object(p.uuid,'uuid4',lambda:types.SimpleNamespace(hex='b'*32)):
     result=p.check(expected)
  elif op=='receipt':result=c.verified_receipt(value,selected)
  elif op=='journal':
   with tempfile.TemporaryDirectory(prefix='ops-journal-oracle-') as directory:
    path=pathlib.Path(directory)/'current.json';path.write_text(json.dumps(value))
    r.protected=lambda *a,**kw:None;r.finished_journal(path);r.protected=forbidden
    result={'schemaVersion':1,'finalized':True}
  else:raise AssertionError('unexpected pure operation')
  results.append({'ok':True,'value':result})
 except DuplicateJsonError:results.append({'ok':False,'error':'ambiguous_json'})
 except (t.TrustError,r.FetchError,c.FetchError,p.PreflightError) as error:
  results.append({'ok':False,'error':str(error)})
 except KeyError:results.append({'ok':False,'error':'receiver_receipt_invalid'})
 except ValueError as error:
  results.append({'ok':False,'error':'expected_object' if str(error)=='Expected a JSON object' else 'invalid_json'})
json.dump(results,sys.stdout)
"#;
    let root = std::env::var_os("OPS_REFERENCE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .to_owned()
        });
    let mut child = Command::new("python3")
        .args(["-I", "-c", script])
        .arg(root)
        .arg(serde_json::to_string(&selection()).unwrap())
        .arg(envelope().to_string())
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
    eprintln!("checked {} retained Python policy cases", cases.len());
    for (index, (case, expected)) in cases.iter().zip(expected).enumerate() {
        let actual = match native(case) {
            Ok(value) => json!({"ok":true,"value":value}),
            Err(e) => json!({"ok":false,"error":e.code()}),
        };
        assert_eq!(actual, expected, "case{index}: {case}");
    }
}
fn mutations(op: &str, value: Value) -> Vec<Value> {
    let mut cases = vec![json!({"op":op,"value":value})];
    for key in value.as_object().unwrap().keys() {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(key);
        cases.push(json!({"op":op,"value":missing}));
        for replacement in [
            Value::Null,
            json!(true),
            json!(false),
            json!(0),
            json!(1.0),
            json!("PRIVATE"),
            json!([]),
            json!({}),
        ] {
            let mut changed = value.clone();
            changed[key] = replacement;
            cases.push(json!({"op":op,"value":changed}));
        }
    }
    let mut extra = value;
    extra["private_unrecognized"] = json!("PRIVATE_SENTINEL");
    cases.push(json!({"op":op,"value":extra}));
    cases
}

#[test]
fn predicates_and_proofs_match_selected_source_shapes_and_exact_hash_sets() {
    let mut cases = mutations("predicate", predicate());
    cases.extend(mutations("proof", proof()));
    for field in ["fileDigests", "verificationSha256"] {
        let (op, base) = if field == "fileDigests" {
            ("predicate", predicate())
        } else {
            ("proof", proof())
        };
        for key in base[field].as_object().unwrap().keys() {
            for value in [
                Value::Null,
                json!(true),
                json!("A".repeat(64)),
                json!("b".repeat(63)),
                json!("b".repeat(65)),
            ] {
                let mut changed = base.clone();
                changed[field][key] = value;
                cases.push(json!({"op":op,"value":changed}));
            }
        }
    }
    compare(&cases);
}
#[test]
fn envelopes_reject_mixed_types_invalid_selectors_and_unbound_claims() {
    let mut cases = mutations("envelope", envelope());
    for (key, values) in [
        (
            "repository",
            vec![
                json!("../repo"),
                json!("a/b/c"),
                json!("a/"),
                json!("a/under_score"),
                json!("owner/re.po-"),
            ],
        ),
        (
            "artifactId",
            vec![
                json!(-1),
                json!(9223372036854775807_i64),
                json!(9223372036854775808_u64),
            ],
        ),
        (
            "artifactZipBytes",
            vec![json!(-1), json!(2147483648_i64), json!(2147483649_i64)],
        ),
        ("buildRunId", vec![json!(788)]),
        ("ciRunId", vec![json!(790)]),
        ("runAttempt", vec![json!(2)]),
    ] {
        for value in values {
            let mut changed = envelope();
            changed[key] = value;
            cases.push(json!({"op":"envelope","value":changed}));
        }
    }
    let line = format!("{}\n", envelope());
    cases.push(json!({"op":"line","raw":line}));
    for raw in ["", "{}", "[]\n", "{}\n", "{\"x\":1,\"x\":2}\n"] {
        // Empty/unterminated input is refused by read_line before decoding.
        if !raw.is_empty() && raw.ends_with('\n') {
            cases.push(json!({"op":"line","raw":raw}));
        }
    }
    compare(&cases);
    assert!(Envelope::decode_line(b"{}").is_err());
    assert!(Envelope::decode_line(b"").is_err());
}
#[test]
fn receipt_consistency_and_redaction_match_actual_controller_projection() {
    let mut cases = mutations("receipt", receipt());
    for value in [
        json!(-0.0),
        json!(330),
        json!(330.0),
        json!(330.001),
        json!(-0.001),
        json!(true),
    ] {
        let mut changed = receipt();
        changed["elapsedSeconds"] = value;
        cases.push(json!({"op":"receipt","value":changed}));
    }
    for phase in ["failed", "preflight"] {
        for evidence in [Value::Null, receipt()["evidence"].clone()] {
            let mut failed = receipt();
            failed["passed"] = false.into();
            failed["status"] = "failed".into();
            failed["failureClass"] = "download_size_mismatch".into();
            failed["phase"] = phase.into();
            failed["evidence"] = evidence;
            failed["settled"] = false.into();
            failed["finalized"] = false.into();
            failed["downloadedBytes"] = 0.into();
            failed["archiveSha256"] = Value::Null;
            failed["manifestSha256"] = Value::Null;
            cases.push(json!({"op":"receipt","value":failed.clone()}));
            for count in [json!(false), json!(0.0), json!(-1), json!(101)] {
                let mut changed = failed.clone();
                changed["downloadedBytes"] = count;
                cases.push(json!({"op":"receipt","value":changed}));
            }
        }
    }
    compare(&cases);
    let mut private = receipt();
    private["private_unrecognized"] = json!({"token":"PRIVATE_SENTINEL"});
    let document = ReceiverReceipt::decode(
        &serde_json::to_vec(&private).unwrap(),
        Envelope::decode(&serde_json::to_vec(&envelope()).unwrap())
            .unwrap()
            .value(),
    )
    .unwrap();
    assert!(document.value().completed());
    assert!(
        !serde_json::to_string(document.value())
            .unwrap()
            .contains("PRIVATE_SENTINEL")
    );
    assert!(String::from_utf8_lossy(document.as_json()).contains("PRIVATE_SENTINEL"));
}
#[test]
fn helper_requests_and_finished_claims_preserve_the_operational_policy() {
    let mut cases = mutations("helpers", helpers());
    cases.push(json!({"op":"helpers","value":{}}));
    for extension in [receipts::FETCH.to_vec(), vec!["force_release.py"]] {
        let mut selected = helpers();
        for name in extension {
            selected[name] = json!("b".repeat(64));
        }
        cases.push(json!({"op":"helpers","value":selected}));
    }
    for value in [
        json!({"schemaVersion":1,"finalized":true}),
        json!({"schemaVersion":true,"finalized":true}),
        json!({"schemaVersion":1.0,"finalized":true}),
        json!({"schemaVersion":1,"finalized":false}),
        json!({"schemaVersion":1,"finalized":true,"private":"PRIVATE"}),
        Value::Null,
    ] {
        cases.push(json!({"op":"journal","value":value}));
    }
    cases.push(json!({"op":"preflight_producer","value":{}}));
    cases.push(json!({"op":"preflight_producer","value":helpers()}));
    compare(&cases);
}
#[test]
fn originals_limits_and_preflight_producer_conformance_are_explicit() {
    let source = serde_json::to_vec_pretty(&proof()).unwrap();
    let document = Proof::decode(&source, &selection()).unwrap();
    assert_eq!(document.as_json(), source);
    assert_eq!(
        document.sha256(),
        Sha256::digest(&source)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let helpers = HelperSet::decode(b"{}").unwrap();
    let receipt = json!({"schemaVersion":1,"runId":"b".repeat(32),"prepared":false});
    assert!(
        PreflightReceipt::decode(&serde_json::to_vec(&receipt).unwrap(), helpers.value()).is_ok()
    );
    for (key, value) in [
        ("schemaVersion", json!(true)),
        ("prepared", json!(0)),
        ("runId", json!("B".repeat(32))),
    ] {
        let mut changed = receipt.clone();
        changed[key] = value;
        assert!(
            PreflightReceipt::decode(&serde_json::to_vec(&changed).unwrap(), helpers.value())
                .is_err()
        );
    }
    assert!(Envelope::decode(&vec![b' '; MAX_LINE + 1]).is_err());
}
#[test]
fn json_truthiness_equality_and_lexical_serialization_keep_python_numeric_semantics() {
    for (a, b, expected) in [
        ("2", "2.0", true),
        ("true", "1.0", true),
        ("false", "-0.0", true),
        ("9007199254740993", "9007199254740992.0", false),
        ("9007199254740992", "9007199254740992.0", true),
        ("[true,2.0]", "[1,2]", true),
        ("{\"a\":-0}", "{\"a\":false}", true),
        ("null", "0", false),
        ("\"0\"", "0", false),
    ] {
        assert_eq!(
            json::decode(a.as_bytes())
                .unwrap()
                .semantic_eq(&json::decode(b.as_bytes()).unwrap()),
            expected
        );
    }
    for source in ["null", "false", "0", "-0", "-0.0", "[]", "{}", "\"\""] {
        assert!(!json::decode(source.as_bytes()).unwrap().truthiness());
    }
    let source = b"[1e+00,-0.0,9007199254740993,true,null]";
    assert_eq!(
        serde_json::to_vec(&json::decode(source).unwrap()).unwrap(),
        source
    );
}

#[test]
fn per_document_budgets_allow_whitespace_and_encoded_expansion_without_changing_manifest_limits() {
    let mut bytes = serde_json::to_vec(&predicate()).unwrap();
    bytes.resize(3 * 1024 * 1024, b' ');
    assert!(Predicate::decode(&bytes, &selection()).is_ok());
    assert!(super::super::manifest::Manifest::decode(&bytes).is_err());
    let source = "\"ࠀࠀࠀࠀ\"";
    let bytes: Vec<u8> = source.encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert_eq!(bytes.len(), 12);
    assert_eq!(source.len(), 14);
    assert!(json::decode_limited(&bytes, 12).is_ok());
    assert!(json::decode_limited(&bytes, 11).is_err());
    let mut helpers = b"{}".to_vec();
    helpers.resize(4096, b' ');
    assert!(HelperSet::decode(&helpers).is_ok());
    helpers.push(b' ');
    assert_eq!(
        HelperSet::decode(&helpers).unwrap_err(),
        Error::Policy(Failure::Request)
    );
}
