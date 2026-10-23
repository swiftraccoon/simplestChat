//! Closed standalone inputs. No installed-path, UID or normal-cleanup bypass.
use super::{failure, plan};
use crate::{
    ci::container_engine::{image_id, safe_path},
    tooling::Result,
};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Settings {
    pub server_image: String,
    pub generator_image: String,
    pub output: PathBuf,
    pub workers: u32,
    pub clients: u32,
    pub rooms: u32,
    pub duration: u32,
    pub ramp_up: u32,
    pub warmup: u32,
}
#[derive(Clone, Debug)]
pub(super) enum Config {
    Run(Settings),
    Cleanup,
}
fn number(v: &str, lo: u32, hi: u32) -> Result<u32> {
    if v.is_empty() || v.len() > 3 || v.starts_with('0') || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(failure(2));
    }
    v.parse::<u32>()
        .ok()
        .filter(|n| (lo..=hi).contains(n))
        .ok_or_else(|| failure(2))
}
pub(super) fn path(path: &Path) -> bool {
    safe_path(path)
        && path.components().count() <= 20
        && path.to_str().is_some_and(|v| {
            !v.chars().any(char::is_control)
                && !v.contains(',')
                && path.components().collect::<PathBuf>() == path
        })
}
impl Config {
    pub fn parse(args: &[String]) -> Result<Self> {
        if args == ["cleanup"] {
            return Ok(Self::Cleanup);
        }
        if args.first().map(String::as_str) != Some("run") || args.len() % 2 != 1 {
            return Err(failure(2));
        }
        let mut s = Settings {
            server_image: String::new(),
            generator_image: String::new(),
            output: PathBuf::new(),
            workers: 1,
            clients: 10,
            rooms: 1,
            duration: 60,
            ramp_up: 5,
            warmup: 10,
        };
        let mut seen = BTreeSet::new();
        for pair in args[1..].as_chunks::<2>().0 {
            if !seen.insert(pair[0].as_str()) {
                return Err(failure(2));
            }
            let v = &pair[1];
            match pair[0].as_str() {
                "--server-image" => s.server_image = v.clone(),
                "--generator-image" => s.generator_image = v.clone(),
                "--output" => s.output = PathBuf::from(v),
                "--workers" => s.workers = number(v, 1, 4)?,
                "--clients" => s.clients = number(v, 2, 30)?,
                "--rooms" => s.rooms = number(v, 1, 4)?,
                "--duration" => s.duration = number(v, 3, 180)?,
                "--ramp-up" => s.ramp_up = number(v, 1, 120)?,
                "--warmup" => s.warmup = number(v, 10, 60)?,
                _ => return Err(failure(2)),
            }
        }
        if !image_id(&s.server_image)
            || !image_id(&s.generator_image)
            || !path(&s.output)
            || s.output.file_name().is_none()
            || s.rooms > s.clients
            || s.clients > s.rooms * 10
        {
            return Err(failure(2));
        }
        Ok(Self::Run(s))
    }
}
impl Settings {
    pub fn wait(&self) -> std::time::Duration {
        std::time::Duration::from_secs(u64::from(self.ramp_up + self.warmup + self.duration + 60))
    }
    pub fn selection(&self, run: &str) -> super::policy::SummarySelection {
        super::policy::SummarySelection {
            clients: self.clients,
            rooms: self.rooms,
            duration: self.duration,
            ramp_up: self.ramp_up,
            warmup: self.warmup,
            run_id: run.into(),
            server_image: self.server_image.clone(),
            generator_image: self.generator_image.clone(),
        }
    }
    pub fn create(
        &self,
        endpoint: &str,
        run: &str,
        role: super::io::Role,
        server: &str,
    ) -> crate::ci::command::Command {
        plan::create(endpoint, self, run, role, server)
    }
}
