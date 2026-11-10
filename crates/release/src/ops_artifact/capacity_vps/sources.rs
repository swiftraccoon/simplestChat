//! Original committed sources and one closed execution projection. The selected
//! Ansible runtime remains trusted; checkout siblings never become import paths.

use super::super::namespace::{Budget, Chain, ChainWitness};
use super::{
    Result, failure,
    files::{Original, Output},
    projection::Projection,
    worker::Checkpoint,
};
use std::{collections::BTreeMap, path::Path};

pub(super) const NAMES: &[&str] = &[
    "Cargo.toml",
    "build/vps_ops_bootstrap.py",
    "build/benchmark-container.sh",
    "build/cleanup-container-benchmark.sh",
    "build/capacity.py",
    "build/capacity_artifacts.py",
    "ops/ansible/ansible.cfg",
    "ops/ansible/site.yml",
    "ops/ansible/group_vars/benchmark_hosts.yml",
    "ops/ansible/tasks/host.yml",
    "ops/ansible/tasks/docker.yml",
    "ops/ansible/tasks/source.yml",
    "ops/ansible/tasks/benchmark.yml",
    "ops/ansible/templates/build-images.sh.j2",
    "ops/ansible/templates/run-benchmark.sh.j2",
    "ops/ansible/templates/image-build.service.j2",
    "ops/ansible/templates/benchmark.service.j2",
    "ops/ansible/templates/docker.sources.j2",
    "ops/ansible/templates/docker.preferences.j2",
    "ops/ansible/templates/containerd-runtime.conf.j2",
    "ops/ansible/filter_plugins/sizing.py",
    "ops/ansible/callback_plugins/release_timing.py",
    "ops/ansible/files/bounded_process.py",
];

pub(super) struct Sources {
    originals: BTreeMap<String, Original>,
    git_originals: Vec<Original>,
    bodies: BTreeMap<String, Vec<u8>>,
    projection: Projection,
    caller: ChainWitness,
    budget: Budget,
}
impl Sources {
    pub fn open(
        caller: &Path,
        output: &Output,
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<Self> {
        let original = Chain::open(caller, budget)?;
        let witness = original.witness();
        let closed = original.finish();
        let witness = witness.and_then(|v| closed.map(|()| v))?;
        let mut originals = BTreeMap::new();
        let mut bodies = BTreeMap::new();
        let mut projection_bodies = Vec::new();
        let mut count = 0usize;
        for name in NAMES {
            let (original, body) =
                Original::open(&caller.join(name), 256 * 1024, budget, checkpoint)?;
            count = count.checked_add(body.len()).ok_or_else(|| failure(1))?;
            if count > 3 * 1024 * 1024 {
                return Err(failure(1));
            }
            originals.insert((*name).into(), original);
            bodies.insert((*name).into(), body.clone());
            projection_bodies.push(body);
        }
        let mut git_originals = Vec::new();
        let (head, head_bytes) =
            Original::open(&caller.join(".git/HEAD"), 65536, budget, checkpoint)?;
        git_originals.push(head);
        if let Some(reference) = head_bytes.strip_prefix(b"ref: ") {
            let reference = std::str::from_utf8(reference)
                .map_err(|_| failure(1))?
                .trim_end_matches('\n');
            let reference = Path::new(reference);
            if !reference.starts_with("refs")
                || reference
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(failure(1));
            }
            let loose = caller.join(".git").join(reference);
            let selected = if loose.exists() {
                loose
            } else {
                caller.join(".git/packed-refs")
            };
            git_originals.push(Original::open(&selected, 4 * 1024 * 1024, budget, checkpoint)?.0);
        }
        git_originals
            .push(Original::open(&caller.join(".git/config"), 65536, budget, checkpoint)?.0);
        let projection = Projection::create(
            &output.path("source"),
            output.witness(),
            &projection_bodies,
            budget,
            checkpoint,
        )?;
        Ok(Self {
            originals,
            git_originals,
            bodies,
            projection,
            caller: witness,
            budget: budget.clone(),
        })
    }
    pub fn body(&self, name: &str) -> Result<&[u8]> {
        self.bodies
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| failure(1))
    }
    pub fn check(&self, checkpoint: &Checkpoint) -> Result<()> {
        let caller = self.caller.reopen(&self.budget)?;
        let result = (|| {
            for original in self.originals.values() {
                original.check(&self.budget, checkpoint)?;
            }
            for original in &self.git_originals {
                original.check(&self.budget, checkpoint)?;
            }
            self.projection.check(checkpoint)
        })();
        result.and(caller.finish())
    }
    pub fn directory(&self) -> &Path {
        self.projection.path()
    }
    pub fn bootstrap(&self) -> Result<&str> {
        std::str::from_utf8(self.body("build/vps_ops_bootstrap.py")?).map_err(|_| failure(1))
    }
    pub fn package_version(&self) -> Result<String> {
        let source = std::str::from_utf8(self.body("Cargo.toml")?).map_err(|_| failure(1))?;
        let parsed: toml::Value = toml::from_str(source).map_err(|_| failure(1))?;
        parsed
            .get("workspace")
            .and_then(|v| v.get("package"))
            .and_then(|v| v.get("version"))
            .and_then(toml::Value::as_str)
            .filter(|v| !v.is_empty() && v.len() <= 128)
            .map(str::to_owned)
            .ok_or_else(|| failure(1))
    }
}
