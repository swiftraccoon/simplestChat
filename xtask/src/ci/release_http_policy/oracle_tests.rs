//! Retained actual fifteen tests plus policy matrices executed without exec/socket APIs.

use super::{fixture::*, *};
use crate::{
    ci::process::{Executor, Native},
    owned_process::Signals,
};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::Arc,
};

async fn reference(cases: &[Value]) -> Result<Value> {
    reference_pins::check()?;
    let f = Fixture::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let python = crate::ci::release_checks::files::select(
        root,
        &crate::tooling::inherited_env(),
        std::ffi::OsStr::new("python3"),
    )?;
    let tools = tools::Tools::selected(python);
    let scratch = files::Scratch::open(&f.root)?;
    let input = f.root.join("oracle-input");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&input)
        .unwrap();
    let bytes = serde_json::to_vec(&json!({"cases":cases})).unwrap();
    assert!(bytes.len() <= 4 * 1024 * 1024);
    file.write_all(&bytes).unwrap();
    files::close(file).unwrap();
    let file = File::open(&input).unwrap();
    fs::remove_file(input).unwrap();
    let mut capture = scratch.capture().await?;
    let time = plan::Time::new(Clock::new());
    let (mut command, proof) = tools.oracle(&time)?;
    command.stdin = Some(Arc::new(file));
    capture.attach(&mut command)?;
    let (_, rx) = tokio::sync::mpsc::unbounded_channel();
    let environment = crate::tooling::Environment::new();
    let mut native = Native::new(root, &environment, Signals::injected(rx));
    let outcome = native.run(command).await;
    let clean = native.cleanup_passed();
    drop(native);
    let bytes = capture.finish(proof.seals());
    drop(proof);
    let tool = tools.finish();
    let scratch = scratch.finish();
    outcome?;
    tool?;
    scratch?;
    if !clean {
        return Err(fail(1, "oracle_cleanup"));
    }
    serde_json::from_slice(&bytes?).map_err(|_| fail(1, "oracle_output"))
}
fn case(index: usize) -> Value {
    let response = response(index);
    json!({"kind":"response","index":index,"status":response.status,
        "headers":response.headers.into_iter().collect::<Vec<_>>(),"body":response.body})
}
fn native_case(case: &Value) -> Value {
    let pairs = case
        .get("headers")
        .map(|v| serde_json::from_value::<Vec<(String, String)>>(v.clone()).unwrap());
    let outcome = match case["kind"].as_str().unwrap() {
        "headers" => policy::headers(pairs.as_ref().unwrap()).map(|_| ()),
        "response" => {
            let body: Vec<u8> = serde_json::from_value(case["body"].clone()).unwrap();
            policy::headers(pairs.as_ref().unwrap()).and_then(|h| {
                policy::validate(
                    case["index"].as_u64().unwrap() as usize,
                    case["status"].as_u64().unwrap() as u16,
                    &h,
                    &body,
                )
            })
        }
        "length" => policy::headers(&[(
            "content-length".into(),
            case["value"].as_str().unwrap().into(),
        )])
        .and_then(|h| {
            native::content_length(
                &h,
                policy::ENDPOINTS[case["index"].as_u64().unwrap() as usize].limit,
            )
        }),

        "inspection" => {
            let f = Fixture::new();
            policy::owned(
                &f.config,
                0,
                case["records"][0].as_str().unwrap().as_bytes(),
            )
            .and_then(|()| {
                policy::owned(
                    &f.config,
                    1,
                    case["records"][1].as_str().unwrap().as_bytes(),
                )
            })
        }
        "ownership" => {
            let f = Fixture::new();
            let records = case["records"].as_array().unwrap();
            policy::owned(&f.config, 0, &serde_json::to_vec(&records[0]).unwrap()).and_then(|()| {
                policy::owned(&f.config, 1, &serde_json::to_vec(&records[1]).unwrap())
            })
        }
        _ => panic!("closed oracle case"),
    };
    match outcome {
        Ok(()) => json!({"passed":true}),
        Err(e) => json!({"passed":false,"code":e.message}),
    }
}
fn cases() -> Vec<Value> {
    let mut cases = Vec::new();
    for index in 0..5 {
        cases.push(case(index));
        for field in ["status", "body"] {
            let mut c = case(index);
            c[field] = if field == "status" {
                json!(503)
            } else {
                json!([])
            };
            cases.push(c);
        }
        let mut c = case(index);
        let mut bytes = response(index).body;
        bytes.resize(policy::ENDPOINTS[index].limit, b' ');
        c["body"] = json!(bytes);
        cases.push(c.clone());
        bytes.push(b' ');
        c["body"] = json!(bytes);
        cases.push(c);
    }
    for (key, value) in [
        ("server", "private"),
        ("set-cookie", "private"),
        ("strict-transport-security", "max-age=1"),
        ("content-type", "application/json"),
        ("content-encoding", "gzip"),
        ("x-frame-options", "SAMEORIGIN"),
        ("content-security-policy", "default-src *"),
    ] {
        let mut c = case(0);
        let rows = c["headers"].as_array_mut().unwrap();
        rows.retain(|row| row[0] != key);
        rows.push(json!([key, value]));
        cases.push(c);
    }
    let original = response(0).headers;
    for key in original.keys() {
        let mut c = case(0);
        c["headers"]
            .as_array_mut()
            .unwrap()
            .retain(|row| row[0] != *key);
        cases.push(c);
    }
    for pair in [
        ["X-DUP", "one"],
        ["X-DUP", "two"],
        ["X-DUP", "bad\ncontrol"],
    ] {
        let mut c = case(0);
        c["headers"]
            .as_array_mut()
            .unwrap()
            .extend([json!(["x-dup", "first"]), json!(pair)]);
        cases.push(c);
    }
    for count in [64, 65] {
        cases.push(json!({"kind":"headers","headers":(0..count)
        .map(|i|(format!("x-{i}"),"value")).collect::<Vec<_>>()}));
    }
    for n in [16380, 16381] {
        cases.push(json!({"kind":"headers","headers":[["", "x".repeat(n)]]}));
    }
    for raw in [
        "null",
        "[]",
        "true",
        r#"{"version":1,"version":1}"#,
        r#"{"outer":{"private":1,"private":2}}"#,
        r#"{"a":NaN}"#,
        r#"{"a":1e999}"#,
    ] {
        let mut c = case(1);
        c["body"] = json!(raw.as_bytes());
        cases.push(c);
    }
    let source = String::from_utf8(response(1).body).unwrap();
    for value in ["true", "1.0", "1e0", "null", "\"1\""] {
        let mut c = case(1);
        c["body"] = json!(
            source
                .replace("\"version\":1", &format!("\"version\":{value}"))
                .as_bytes()
        );
        cases.push(c);
    }
    for index in [1, 2] {
        let source = String::from_utf8(response(index).body).unwrap();
        let mut c = case(index);
        let mut bom = vec![239, 187, 191];
        bom.extend_from_slice(source.as_bytes());
        c["body"] = json!(bom);
        cases.push(c);
        for (width, little, bom) in [
            (2, true, false),
            (2, false, false),
            (2, true, true),
            (2, false, true),
            (4, true, false),
            (4, false, false),
            (4, true, true),
            (4, false, true),
        ] {
            let mut bytes = Vec::new();
            if width == 2 {
                if bom {
                    bytes.extend(if little { [255, 254] } else { [254, 255] });
                }
                for c in source.encode_utf16() {
                    bytes.extend(if little {
                        c.to_le_bytes()
                    } else {
                        c.to_be_bytes()
                    });
                }
            } else {
                if bom {
                    bytes.extend(if little {
                        [255, 254, 0, 0]
                    } else {
                        [0, 0, 254, 255]
                    });
                }
                for c in source.chars() {
                    bytes.extend(if little {
                        (c as u32).to_le_bytes()
                    } else {
                        (c as u32).to_be_bytes()
                    });
                }
            }
            let mut c = case(index);
            c["body"] = json!(bytes);
            cases.push(c);
        }
    }
    for value in [
        "0",
        "1",
        "128",
        "131072",
        "131073",
        "-1",
        "+1",
        "1.0",
        "1e2",
        "",
        " 1",
        "١٢٨",
        "１２８",
    ] {
        cases.push(json!({"kind":"length","index":0,"value":value}));
    }
    for digits in [4300, 4301] {
        for first in ['0', '9'] {
            let mut value = first.to_string();
            value.push_str(&"0".repeat(digits - 3));
            value.push_str("15");
            cases.push(json!({"kind":"length","index":0,"value":value}));
        }
    }
    let f = Fixture::new();
    let a: Value = serde_json::from_slice(&inspection(&f.config, 0)).unwrap();
    let b: Value = serde_json::from_slice(&inspection(&f.config, 1)).unwrap();
    cases.push(json!({"kind":"ownership","records":[a,b]}));
    for pointer in [
        "/id",
        "/running",
        "/labels/simplestchat.release-test",
        "/labels/com.docker.compose.project",
        "/labels/com.docker.compose.service",
        "/ports/443~1tcp/0/HostIp",
        "/ports/443~1tcp/0/HostPort",
    ] {
        let mut a = a.clone();
        *a.pointer_mut(pointer).unwrap() = json!("foreign");
        cases.push(json!({"kind":"ownership","records":[a,b]}));
    }
    for key in ["id", "running", "labels", "ports"] {
        let mut row = a.clone();
        row.as_object_mut().unwrap().remove(key);
        cases.push(json!({"kind":"ownership","records":[row,b]}));
    }
    for key in ["labels", "ports"] {
        for value in [json!(null), json!(false), json!([]), json!("private")] {
            let mut row = a.clone();
            row[key] = value;
            cases.push(json!({"kind":"ownership","records":[row,b]}));
        }
    }
    let raw = String::from_utf8(inspection(&f.config, 0)).unwrap();
    let other = String::from_utf8(inspection(&f.config, 1)).unwrap();
    for private in [
        "7".repeat(4000),
        "1e999".into(),
        "{\"duplicate\":1,\"duplicate\":2}".into(),
    ] {
        let row = raw.replace(
            "\"running\":true",
            &format!("\"private\":{private},\"running\":true"),
        );
        cases.push(json!({"kind":"inspection","records":[row,other]}));
    }

    cases
}
#[tokio::test]
async fn actual_fifteen_original_tests_and_bounded_policy_matrix_match() {
    let cases = cases();
    let old = reference(&cases).await.unwrap();
    assert_eq!(old["originalTests"], 15);
    assert_eq!(old["report"], policy::report());
    assert_eq!(old["results"].as_array().unwrap().len(), cases.len());
    for (case, expected) in cases.iter().zip(old["results"].as_array().unwrap()) {
        assert_eq!(&native_case(case), expected, "{case}");
    }
}

#[tokio::test]
async fn original_positive_surrogate_and_depth_inputs_are_named_bounded_domain_refusals() {
    let f = Fixture::new();
    let raw = String::from_utf8(inspection(&f.config, 0)).unwrap();
    let other = String::from_utf8(inspection(&f.config, 1)).unwrap();
    let mut cases = Vec::new();
    for private in [
        r#""\ud800""#.to_owned(),
        format!("{}0{}", "[".repeat(129), "]".repeat(129)),
    ] {
        let row = raw.replace(
            "\"running\":true",
            &format!("\"private\":{private},\"running\":true"),
        );
        cases.push(json!({"kind":"inspection","records":[row,other]}));
    }
    let old = reference(&cases).await.unwrap();
    assert_eq!(old["originalTests"], 15);
    assert_eq!(old["report"], policy::report());
    assert_eq!(old["results"].as_array().unwrap().len(), cases.len());
    for (index, case) in cases.iter().enumerate() {
        assert_eq!(old["results"][index], json!({"passed":true}));
        assert_eq!(
            native_case(case),
            json!({"passed":false,"code":"http_policy_failed"})
        );
    }
}
