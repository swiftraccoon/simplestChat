//! Owned temporary engines and virtual clocks; no container daemon is contacted.

use super::{
    clock::Clock,
    engine::Engine,
    process::{Cancellation, Interrupted, Output, Request},
    process_io::read_limited,
};
use anyhow::Result;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub(super) struct Directory(pub(super) PathBuf);
impl Directory {
    pub(super) fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "capacity-run-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        super::files::directory(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[derive(Default)]
pub(super) struct FakeClock {
    pub(super) seconds: AtomicU64,
    pub(super) interrupt_at: Option<f64>,
}
#[async_trait::async_trait]
impl Clock for FakeClock {
    fn now(&self) -> f64 {
        self.seconds.load(Ordering::SeqCst) as f64 / 1000.0
    }
    fn utc(&self) -> String {
        "2026-10-21T12:34:56+00:00".into()
    }
    async fn sleep(&self, seconds: f64, cancellation: &Cancellation) -> Result<()> {
        self.seconds
            .fetch_add((seconds * 1000.0) as u64, Ordering::SeqCst);
        if self.interrupt_at.is_some_and(|at| self.now() >= at) {
            cancellation.cancel(143);
        }
        if cancellation.code() != 0 {
            return Err(Interrupted(cancellation.code()).into());
        }
        Ok(())
    }
}
#[derive(Clone)]
struct Container {
    id: String,
    image: String,
    run: String,
    end: f64,
}
#[derive(Default)]
pub(super) struct State {
    pub(super) calls: Vec<(Vec<String>, bool)>,
    containers: BTreeMap<String, Container>,
    pub(super) fail_transfer: bool,
    pub(super) fail_remove_generator: bool,
    pub(super) failed_server_response: bool,
    pub(super) missing_marks: bool,
    pub(super) missing_loads: bool,
    pub(super) generator_oom: bool,
    pub(super) identity_mismatch: bool,
    pub(super) never_finish: bool,
    pub(super) cancel_on_transfer: Option<Cancellation>,
    pub(super) fail_probe: bool,
}
pub(super) struct FakeEngine {
    pub(super) clock: Arc<FakeClock>,
    pub(super) state: Mutex<State>,
    archive: Vec<u8>,
}
impl FakeEngine {
    pub(super) fn new(clock: Arc<FakeClock>) -> Self {
        let mut bundle = tar::Builder::new(Vec::new());
        for (name,bytes) in [("load_test_summary.json",br#"{"run":{"passed":true,"failureReasons":[]},"totalErrors":0,"failedConnections":0,"failedConsumers":0,"validatedConsumers":80,"totalDatagramsSent":1000000,"measurement":{"packetsReceived":100000,"bytesReceived":20000000,"durationMs":30000}}"#.as_slice()),
            ("load_test_results.json",b"[]".as_slice())] {
            let mut header=tar::Header::new_gnu();header.set_size(bytes.len() as u64);
            header.set_mode(0o600);header.set_cksum();bundle.append_data(&mut header,name,bytes).unwrap();
        }
        let archive = bundle.into_inner().unwrap();
        Self {
            clock,
            state: Mutex::new(State::default()),
            archive,
        }
    }
    fn response(&self, args: &[String], request: &Request) -> (i32, Vec<u8>, Vec<u8>) {
        let mut state = self.state.lock().unwrap();
        state.calls.push((args.to_vec(), request.cleanup));
        let now = self.clock.now();
        let value = |key: &str| {
            args.iter()
                .position(|arg| arg == key)
                .and_then(|index| args.get(index + 1))
                .cloned()
                .unwrap_or_default()
        };
        let mut status = 0;
        let mut errors = String::new();
        let output = match args[0].as_str() {
            "image" => {
                if args[3] == "{{.Id}}" {
                    "sha256:fixture".into()
                } else {
                    "0123456789abcdef0123456789abcdef".into()
                }
            }
            "run" => {
                if args
                    .last()
                    .is_some_and(|arg| arg.contains("cat /proc/cpuinfo"))
                {
                    if state.fail_probe {
                        return (3, Vec::new(), b"fixture probe failed".to_vec());
                    }
                    return (0, host_source().into_bytes(), Vec::new());
                }
                let name = value("--name");
                let run = value("--label").split_once('=').unwrap().1.to_owned();
                let index = args.iter().position(|arg| arg == "--pids-limit").unwrap() + 2;
                let image = if name.starts_with("capacity-server") {
                    args.last().unwrap().clone()
                } else {
                    args[index + 2].clone()
                };
                let end = now
                    + ["--ramp-up", "--warmup", "--duration"]
                        .into_iter()
                        .map(|flag| value(flag).parse::<f64>().unwrap_or(0.0))
                        .sum::<f64>();
                let id = format!("{:064x}", state.containers.len() + 1);
                state.containers.insert(
                    name.clone(),
                    Container {
                        id: id.clone(),
                        image,
                        run,
                        end,
                    },
                );
                if state.failed_server_response && name.starts_with("capacity-server") {
                    status = 1;
                    errors = "failed after creation".into();
                    String::new()
                } else {
                    id
                }
            }
            "inspect" => {
                let identity = args.last().unwrap();
                let found = state
                    .containers
                    .iter()
                    .find(|(name, container)| *name == identity || container.id == *identity);
                if let Some((name, container)) = found {
                    if args.len() > 2 {
                        if args[2].contains("OOMKilled") {
                            (state.generator_oom && name.starts_with("capacity-gen")).to_string()
                        } else {
                            (!name.starts_with("capacity-gen")
                                || now < container.end
                                || state.never_finish)
                                .to_string()
                        }
                    } else {
                        json!([{"Id":container.id,"Name":format!("/{name}"),
                        "Config":{"Image":container.image,"Labels":{"simplestchat.capacity.run":if state.identity_mismatch {"unrelated"} else {&container.run}}}}]).to_string()
                    }
                } else {
                    status = 1;
                    errors = "No such container".into();
                    String::new()
                }
            }
            "stop" => {
                let id = args.last().unwrap();
                if let Some(container) = state.containers.values_mut().find(|entry| entry.id == *id)
                {
                    container.end = now;
                } else {
                    status = 1;
                    errors = "No such container".into();
                }
                String::new()
            }
            "rm" => {
                let id = args.last().unwrap();
                let name = state
                    .containers
                    .iter()
                    .find(|(_, container)| container.id == *id)
                    .map(|(name, _)| name.clone())
                    .unwrap();
                if state.fail_remove_generator && name.starts_with("capacity-gen") {
                    status = 1;
                    errors = "fixture removal failed".into();
                } else {
                    state.containers.remove(&name);
                }
                String::new()
            }
            "cp" => {
                if let Some(cancellation) = &state.cancel_on_transfer {
                    cancellation.cancel(143);
                }
                if state.fail_transfer {
                    status = 7;
                }
                return (
                    status,
                    self.archive.clone(),
                    b"fixture transfer diagnostics".to_vec(),
                );
            }
            "logs" => "private fixture log\n".into(),
            "exec" => {
                let name = state
                    .containers
                    .iter()
                    .find(|(_, entry)| entry.id == args[1])
                    .map_or(args[1].as_str(), |(name, _)| name.as_str());
                let path = args.last().unwrap();
                if args.get(3).is_some_and(|arg| arg == "/bin/bash") {
                    let input = String::from_utf8_lossy(&request.input);
                    let body = if input.starts_with("GET /health ") {
                        "ok".into()
                    } else if state.missing_loads {
                        String::new()
                    } else {
                        (0..64)
                            .map(|worker| {
                                format!(
                                    "simplestchat_media_worker_cpu{{worker=\"{worker}\"}} 0.25\n"
                                )
                            })
                            .collect::<String>()
                    };
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    match path.as_str() {
                        "/sys/fs/cgroup/cpu.stat" => {
                            if state.missing_marks {
                                status = 1;
                                errors = "unreadable".into();
                                String::new()
                            } else {
                                format!(
                                    "usage_usec {:.0}\nnr_periods {:.0}\nnr_throttled 0\n",
                                    now * if name.starts_with("capacity-gen") {
                                        500000.0
                                    } else {
                                        250000.0
                                    },
                                    now * 10.0
                                )
                            }
                        }
                        "/proc/stat" => format!(
                            "cpu {:.0} 0 0 {:.0} 0 0 0 0\n",
                            1000.0 + now * 50.0,
                            1000.0 + now * 50.0
                        ),
                        "/proc/net/snmp" => format!(
                            "Udp: InDatagrams RcvbufErrors\nUdp: {:.0} 0\n",
                            10000.0 + now * 1000.0
                        ),
                        "/proc/net/udp" => String::new(),
                        "/sys/fs/cgroup/memory.max" => "1073741824".into(),
                        "/sys/fs/cgroup/memory.current" => "104857600".into(),
                        _ => panic!("unexpected fixture read {path}"),
                    }
                }
            }
            verb => panic!("unexpected fake engine verb {verb}"),
        };
        (status, output.into_bytes(), errors.into_bytes())
    }
}
#[async_trait::async_trait]
impl Engine for FakeEngine {
    fn executable(&self) -> &Path {
        Path::new("/owned-fixture/docker")
    }
    async fn call(&self, args: &[String], request: Request) -> Result<Output> {
        let (status, output, errors) = self.response(args, &request);
        let (stdout, stderr) = tokio::try_join!(
            read_limited(
                output.as_slice(),
                request.stdout,
                request.limits.stdout,
                "stdout"
            ),
            read_limited(
                errors.as_slice(),
                request.stderr,
                request.limits.stderr,
                "stderr"
            )
        )?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}
fn host_source() -> String {
    concat!(
        "model name: Fixture CPU\n@@\nMemTotal: 12582912 kB\n@@\n",
        "cpu 10 0 0 90 0 0 0 0\ncpu0 1\ncpu1 1\ncpu2 1\ncpu3 1\n@@\n",
        "cpu 20 0 0 180 0 0 0 0\n@@\n2097152\n@@\nfixture-kernel\n@@\nFixture\nModel"
    )
    .into()
}
