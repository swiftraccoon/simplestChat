//! Inert selected tools, synthetic tar bytes and a bounded fake engine only.
use super::*;
use crate::{
    ci::{
        command::{Command, Program},
        process::{self, Executor},
    },
    tooling::{Environment, Failure, Result},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Cursor, Read, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
pub const REV: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const ENDPOINT: &str = "unix:///owned/manual-docker.sock";
pub const CID: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub struct Fixture {
    pub root: PathBuf,
    pub context: plan::Context,
    pub raw: Vec<u8>,
    pub image: String,
}
impl Fixture {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "manual image {} {} literal $(x)",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        for name in ["temporary", "bin"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        for name in ["git", "docker", "zstd"] {
            let path = root.join("bin").join(name);
            fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let environment = Environment::from([
            ("GITHUB_ACTIONS".into(), "true".into()),
            ("RUNNER_ENVIRONMENT".into(), "github-hosted".into()),
            ("GITHUB_SHA".into(), REV.into()),
            (
                "RUNNER_TEMP".into(),
                root.join("temporary").into_os_string(),
            ),
            ("PATH".into(), root.join("bin").into_os_string()),
        ]);
        let context = plan::Context::new(&root, environment, "Linux", Instant::now()).unwrap();
        context.time.set(Duration::ZERO);
        let layer = b"inert saved raw layer";
        let config = serde_json::to_vec(&json!({"os":"linux","architecture":"amd64",
            "rootfs":{"type":"layers","diff_ids":[format!("sha256:{}",digest(layer))]},
            "config":{"User":"10001:10001","WorkingDir":"/results",
            "Entrypoint":["/app/load_test"],"Cmd":["--help"],
            "Env":[format!("SOURCE_REVISION={REV}"),"PRIVATE_FIXTURE=never_project"],
            "Labels":{"org.opencontainers.image.revision":REV}}}))
        .unwrap();
        let image = format!("sha256:{}", digest(&config));
        let manifest = serde_json::to_vec(
            &json!([{"Config":"config.json","RepoTags":[plan::TAG],"Layers":["layer.tar"]}]),
        )
        .unwrap();
        let mut tar = tar::Builder::new(Vec::new());
        for (name, bytes) in [
            ("manifest.json", manifest.as_slice()),
            ("config.json", config.as_slice()),
            ("layer.tar", layer.as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            tar.append_data(&mut header, name, Cursor::new(bytes))
                .unwrap();
        }
        let raw = tar.into_inner().unwrap();
        Self {
            root,
            context,
            raw,
            image,
        }
    }
    pub fn files(&self) -> files::Owned {
        files::Owned::new(&self.context).unwrap()
    }
    pub fn tools(&self) -> tools::Tools {
        tools::Tools::select(&self.root, &self.context.environment).unwrap()
    }
    pub fn fake(&self) -> Fake {
        Fake {
            raw: self.raw.clone(),
            image: self.image.clone(),
            time: self.context.time.clone(),
            ..Fake::default()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[derive(Clone)]
pub struct Call {
    pub program: Program,
    pub arguments: Vec<String>,
    pub cleanup: bool,
    pub timeout: Duration,
    pub grace: Duration,
}
pub struct Fake {
    pub raw: Vec<u8>,
    pub image: String,
    pub time: plan::Time,
    pub calls: Vec<Call>,
    builder: Option<(String, String, String)>,
    volume: Option<String>,
    container: bool,
    token: String,
    pub fault: &'static str,
    pub primary: u8,
    pub pending: Option<u8>,
    pub clean: bool,
    pub replayed: Vec<u8>,
    pub cleanup_ticks: bool,
    pub final_jump: bool,
    pub build_limit: Option<Duration>,
    pub reference_volume: bool,
}
impl Default for Fake {
    fn default() -> Self {
        let mut environment = Environment::new();
        environment.insert("RUNNER_TEMP".into(), "/tmp".into());
        // Time does not acquire resources; create it through the same context path.
        environment.insert("GITHUB_SHA".into(), REV.into());
        environment.insert("GITHUB_ACTIONS".into(), "true".into());
        environment.insert("RUNNER_ENVIRONMENT".into(), "github-hosted".into());
        let time = plan::Context::new(
            std::path::Path::new("/tmp"),
            environment,
            "Linux",
            Instant::now(),
        )
        .unwrap()
        .time;
        time.set(Duration::ZERO);
        Self {
            raw: vec![],
            image: String::new(),
            time,
            calls: vec![],
            builder: None,
            volume: None,
            container: false,
            token: String::new(),
            fault: "",
            primary: 42,
            pending: None,
            clean: true,
            replayed: vec![],
            cleanup_ticks: false,
            final_jump: false,
            build_limit: None,
            reference_volume: false,
        }
    }
}
impl Fake {
    fn respond(
        &mut self,
        program: Program,
        args: &[String],
        input: &[u8],
        cleanup: bool,
    ) -> Result<Vec<u8>> {
        let a = args.iter().map(String::as_str).collect::<Vec<_>>();
        if program == Program::Git {
            return if a[0] == "rev-parse" {
                Ok(format!("{REV}\n").into())
            } else {
                Ok(vec![])
            };
        }
        if program == Program::Zstd {
            if self.fault == "zstd" {
                return Err(Failure::new(self.primary, "fixed fake"));
            }
            return if a[0] == "--decompress" {
                let mut data = input
                    .strip_prefix(b"inert-zstd:")
                    .ok_or_else(plan::failure)?
                    .to_vec();
                if self.fault == "roundtrip" {
                    data[0] ^= 1;
                }
                Ok(data)
            } else {
                Ok([b"inert-zstd:".as_slice(), input].concat())
            };
        }
        let a = if a.first() == Some(&"--host") {
            &a[2..]
        } else {
            &a[..]
        };
        let answer = match a {
            ["info", ..] => b"owned-daemon\n".to_vec(),
            ["buildx", "ls", ..] => {
                let mut rows = vec![json!({"Name":"default","Driver":"docker","Nodes":[]})];
                if let Some((name, node, endpoint)) = &self.builder {
                    let error = if self.fault == "builder-err" {
                        "private refused"
                    } else {
                        ""
                    };
                    rows.push(json!({"Name":name,"Driver":"docker-container","Err":error,
                        "Nodes":[{"Name":node,"Endpoint":endpoint,
                        "DriverOpts":{format!("env.{}",plan::MARKER):self.token}}]}));
                }
                rows.iter()
                    .map(|v| format!("{v}\n"))
                    .collect::<String>()
                    .into_bytes()
            }
            ["volume", "ls", ..] => self
                .volume
                .as_ref()
                .map_or(vec![], |name| format!("{name}\n").into()),
            ["volume", "create", ..] => {
                let name = a.last().unwrap().to_string();
                self.token = a[5]
                    .strip_prefix(&format!("{}=", plan::OWNER))
                    .unwrap()
                    .into();
                self.volume = Some(name.clone());
                if self.fault == "volume-lost" {
                    self.volume = None;
                    return Err(Failure::new(self.primary, "fixed fake"));
                }
                format!("{name}\n").into()
            }
            ["volume", "inspect", ..] => {
                let name = self.volume.as_ref().ok_or_else(plan::failure)?;
                let owner = if cleanup && self.fault == "volume-owner" {
                    "foreign"
                } else {
                    &self.token
                };
                serde_json::to_vec(&json!({"Name":name,"Driver":"local","Scope":"local",
                    "Mountpoint":"/owned/volume/_data","CreatedAt":"2026-10-22T00:00:00Z",
                    "Labels":{plan::OWNER:owner},"Options":null}))
                .unwrap()
            }
            ["buildx", "create", ..] => {
                let get = |key: &str| {
                    a.iter()
                        .position(|v| *v == key)
                        .map(|i| a[i + 1].to_owned())
                        .unwrap()
                };
                let name = get("--name");
                let node = get("--node");
                let endpoint = a.last().unwrap().to_string();
                self.builder = Some((name.clone(), node, endpoint));
                if self.fault == "builder-lost" {
                    return Err(Failure::new(self.primary, "fixed fake"));
                }
                format!("{name}\n").into()
            }
            ["buildx", "build", ..] => {
                self.container = true;
                if self.fault == "cancel-build" {
                    self.pending = Some(self.primary);
                }
                if self.fault == "bootstrap-empty" {
                    self.container = false;
                    return Err(Failure::new(self.primary, "fixed fake"));
                }
                if self.fault == "build" {
                    return Err(Failure::new(self.primary, "fixed fake"));
                }
                vec![]
            }
            ["container", "ls", ..] => {
                if a.iter().any(|v| v.starts_with("volume=")) {
                    if self.reference_volume {
                        return Ok(format!("{}\n", "d".repeat(64)).into());
                    }
                    if self.container {
                        format!("{CID}\n").into()
                    } else {
                        vec![]
                    }
                } else if self.container {
                    let node = &self.builder.as_ref().unwrap().1;
                    let name = format!("buildx_buildkit_{node}");
                    let id = if self.fault == "foreign-name" && cleanup {
                        "d".repeat(64)
                    } else {
                        CID.into()
                    };
                    format!(
                        "{}\n",
                        json!({"id":id,"name":format!("{name},parent/alias")})
                    )
                    .into()
                } else if self.fault == "replace-after-rm"
                    && cleanup
                    && let Some((_, node, _)) = &self.builder
                {
                    let name = format!("buildx_buildkit_{node}");
                    format!("{}\n", json!({"id":"d".repeat(64),"name":name})).into()
                } else {
                    vec![]
                }
            }
            ["container", "inspect", ..] => {
                let node = &self.builder.as_ref().unwrap().1;
                let name = format!("buildx_buildkit_{node}");
                let marker = if self.fault == "marker" {
                    "foreign"
                } else {
                    &self.token
                };
                serde_json::to_vec(&json!({"Id":CID,"Name":format!("/{name}"),
                    "Image":format!("sha256:{}","b".repeat(64)),
                    "Config":{"Image":"moby/buildkit:buildx-stable-1",
                    "Env":[format!("{}={marker}",plan::MARKER)]},
                    "Mounts":[{"Type":"volume","Name":self.volume,
                    "Destination":"/var/lib/buildkit","RW":true,
                    "Source":"/owned/volume/_data"}]}))
                .unwrap()
            }
            ["container", "rm", ..] => {
                assert_eq!(a.last(), Some(&CID));
                self.container = false;
                vec![]
            }
            ["volume", "rm", ..] => {
                self.volume = None;
                vec![]
            }
            ["buildx", "rm", ..] => {
                assert!(a.contains(&"--keep-daemon") && a.contains(&"--keep-state"));
                self.builder = None;
                vec![]
            }
            ["image", "inspect", ..] => serde_json::to_vec(&json!({"Id":self.image,
                "Os":"linux","Architecture":"amd64","Config":{"User":"10001:10001",
                "WorkingDir":"/results","Entrypoint":["/app/load_test"],"Cmd":["--help"],
                "Labels":{"org.opencontainers.image.revision":REV},
                "Env":[format!("SOURCE_REVISION={REV}")]}}))
            .unwrap(),
            ["save", ..] => self.raw.clone(),
            _ => panic!("unexpected fixed fake command"),
        };
        Ok(answer)
    }
}
impl Executor for Fake {
    async fn run(&mut self, mut request: Command) -> Result<Vec<u8>> {
        let args = request
            .arguments
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        if request.program == Program::Git || request.program == Program::Docker {
            request
                .load_image
                .as_ref()
                .unwrap()
                .validate(request.program)?;
        }
        if request.program == Program::Zstd {
            request.zstd.as_ref().unwrap().validate()?;
        }
        if plan::build_command(&request) {
            self.build_limit = Some(request.timeout);
        }
        self.calls.push(Call {
            program: request.program,
            arguments: args.clone(),
            cleanup: request.cleanup,
            timeout: request.timeout,
            grace: request.grace,
        });
        let mut input = vec![];
        if let Some(file) = &request.stdin {
            let mut file = file.try_clone().unwrap();
            file.read_to_end(&mut input).unwrap();
            files::close(file).unwrap();
        }
        let result = self.respond(request.program, &args, &input, request.cleanup);
        if self.cleanup_ticks && request.cleanup {
            self.time.set(self.time.elapsed() + Duration::from_secs(26));
        }
        let emitted = result.as_deref().unwrap_or(b"");
        if let Some(log) = &request.combined_log {
            let mut writer = log.writer()?;
            writer.write_all(emitted).unwrap();
            files::close(writer)?;
        }
        if let Some(logs) = &request.separate_logs {
            let mut writer = logs.stdout.writer()?;
            writer.write_all(emitted).unwrap();
            files::close(writer)?;
            let mut writer = logs.stderr.writer()?;
            writer.write_all(b"private fixture diagnostics").unwrap();
            files::close(writer)?;
        }
        request.load_image = None;
        request.zstd = None;
        let captured = request.capture.is_some();
        let settled = process::settle_unforwarded(request).await;
        let bytes = result?;
        settled?;
        Ok(if captured { bytes } else { vec![] })
    }
    async fn ready(&mut self) -> Result<()> {
        if let Some(code) = self.pending {
            Err(Failure::new(code, "fixed fake cancellation"))
        } else {
            Ok(())
        }
    }
    fn cleanup_passed(&self) -> bool {
        self.clean
    }
    async fn replay(&mut self, bytes: &[u8], _: bool, _: Instant) -> Result<()> {
        self.replayed.extend(bytes);
        if self.final_jump {
            self.time.set(plan::ENTRY);
        }
        Ok(())
    }
}
