//! Fixed managed-store CLI. Caller-selected paths apply only to capture inputs;
//! there is no managed-root, owner, clock or workload-lock bypass.
use serde::Serialize;
use simplestchat_ops::release::assets::{Capture, Failure, Store, Workload};
use std::{ffi::OsString, io::Write, path::PathBuf, process::ExitCode};

enum Request {
    Initialize(Option<String>),
    Prune,
    Inspect,
    Select(String),
    Capture(PathBuf),
    Publish(Vec<(String, PathBuf)>),
}
fn parse(args: &[OsString]) -> Option<Request> {
    let token = |index: usize| args.get(index).and_then(|value| value.to_str());
    match token(0)? {
        "initialize" if args.len() == 1 => Some(Request::Initialize(None)),
        "initialize" if args.len() == 3 && token(1) == Some("--revision") => {
            Some(Request::Initialize(Some(token(2)?.into())))
        }
        "prune" if args.len() == 1 => Some(Request::Prune),
        "inspect" if args.len() == 1 => Some(Request::Inspect),
        "select" if args.len() == 3 && token(1) == Some("--revision") => {
            Some(Request::Select(token(2)?.into()))
        }
        "capture" if args.len() == 3 && token(1) == Some("--path") => {
            Some(Request::Capture((&args[2]).into()))
        }
        "publish" if matches!(args.len(), 4 | 7) => {
            let mut captures = vec![];
            for offset in (1..args.len()).step_by(3) {
                if token(offset) != Some("--capture") {
                    return None;
                }
                captures.push((token(offset + 1)?.into(), PathBuf::from(&args[offset + 2])));
            }
            Some(Request::Publish(captures))
        }
        _ => None,
    }
}
#[derive(Serialize)]
struct Passed {
    passed: bool,
}
#[derive(Serialize)]
struct Changed {
    passed: bool,
    changed: bool,
}
fn value<T: Serialize>(value: T) -> Vec<u8> {
    // Every value is a bounded fixed schema without fallible custom serializers.
    serde_json::to_vec(&value).expect("fixed asset receipt")
}
fn managed(request: Request) -> Result<Vec<u8>, Failure> {
    let mut guard = Workload::acquire()?;
    let result = (|| {
        let mut store = Store::open(&mut guard)?;
        Ok(match request {
            Request::Initialize(revision) => {
                let mut changed = store.initialize()?;
                if let Some(revision) = revision {
                    changed = store.select_version(&revision)? || changed;
                }
                value(Changed {
                    passed: true,
                    changed,
                })
            }
            Request::Prune => {
                store.initialize()?;
                store.prune()?;
                value(Passed { passed: true })
            }
            Request::Inspect => value(store.inspect()?),
            Request::Select(revision) => value(Changed {
                passed: true,
                changed: store.select_version(&revision)?,
            }),
            Request::Publish(inputs) => {
                store.initialize()?;
                let captures = inputs
                    .into_iter()
                    .map(|(revision, path)| Capture::read(&path).map(|capture| (revision, capture)))
                    .collect::<Result<Vec<_>, _>>()?;
                value(store.publish(captures)?)
            }
            Request::Capture(_) => unreachable!("capture has no managed authority"),
        })
    })();
    let finished = guard.finish();
    result.and_then(|receipt| finished.map(|()| receipt))
}
pub(super) fn run(args: &[OsString]) -> ExitCode {
    let Some(request) = parse(args) else {
        return ExitCode::from(2);
    };
    let result = match request {
        Request::Capture(path) => Capture::read(&path).map(|capture| {
            #[derive(Serialize)]
            struct Counts {
                passed: bool,
                files: usize,
                bytes: u64,
            }
            value(Counts {
                passed: true,
                files: capture.files(),
                bytes: capture.bytes(),
            })
        }),
        request => managed(request),
    };
    let (bytes, code) = match result {
        Ok(receipt) => (receipt, ExitCode::SUCCESS),
        Err(error) if error.busy() => {
            return publish(
                b"{\"passed\":true,\"busy\":true,\"changed\":false}\n",
                ExitCode::SUCCESS,
            );
        }
        Err(error) => {
            #[derive(Serialize)]
            struct Rejected {
                passed: bool,
                failure: &'static str,
            }
            (
                value(Rejected {
                    passed: false,
                    failure: error.code(),
                }),
                ExitCode::FAILURE,
            )
        }
    };
    let mut bytes = bytes;
    bytes.push(b'\n');
    publish(&bytes, code)
}
fn publish(bytes: &[u8], code: ExitCode) -> ExitCode {
    if std::io::stdout().lock().write_all(bytes).is_ok() {
        code
    } else {
        ExitCode::FAILURE
    }
}
