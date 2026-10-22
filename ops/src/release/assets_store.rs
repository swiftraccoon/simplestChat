//! Validated complete store snapshots and fixed operations under one guard.
use super::{
    MAX_FILES, MAX_GENERATIONS, MAX_INDEX, MAX_TOTAL, MAX_VERSION, Result, commit, fail,
    files::{Dir, Entry, Fs, Pointer},
    lock::Workload,
    model::{self, Capture, Generation, Record},
    require,
};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Serialize)]
pub struct Summary {
    pub generations: usize,
    pub files: usize,
    pub bytes: u64,
}
#[derive(Serialize)]
pub struct Published {
    pub generations: usize,
    pub files: usize,
    pub bytes: u64,
    #[serde(rename = "maximumAgeSeconds")]
    pub maximum_age: u64,
}
impl Summary {
    fn new(generations: &[Generation]) -> Self {
        Self {
            generations: generations.len(),
            files: generations.iter().map(|value| value.assets.len()).sum(),
            bytes: generations.iter().map(Generation::bytes).sum(),
        }
    }
}
pub(super) struct State {
    pub root: Dir,
    pub generations: Vec<Generation>,
    pub original: Option<Vec<u8>>,
    pub index: Option<Entry>,
    pub directories: Vec<(String, Dir)>,
    pub union: Option<Dir>,
    pub public: Option<Dir>,
    pub pointer: Option<Pointer>,
    pub version: Option<Entry>,
}
struct Inspect<'a> {
    fs: &'a mut Fs,
    root: Dir,
    dirs: Vec<(String, Dir)>,
}
impl model::Inspect for Inspect<'_> {
    fn directory(&mut self, revision: &str) -> Result<()> {
        let directory = self.fs.directory(self.root, revision, None)?;
        self.dirs.push((revision.into(), directory));
        Ok(())
    }
    fn members(&mut self, revision: &str, names: &[&str]) -> Result<()> {
        let directory = self
            .dirs
            .iter()
            .find(|value| value.0 == revision)
            .unwrap()
            .1;
        let actual = self
            .fs
            .list(directory, MAX_FILES + 1)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        require(
            actual == names.iter().map(|name| name.to_string()).collect(),
            "asset_generation_files",
        )
    }
    fn asset(&mut self, revision: &str, name: &str, record: &Record) -> Result<()> {
        let directory = self
            .dirs
            .iter()
            .find(|value| value.0 == revision)
            .unwrap()
            .1;
        let entry = self.fs.metadata(directory, name)?;
        require(
            self.fs.stamp(entry).len() == record.size,
            "asset_store_file_size",
        )?;
        let (hash, _, _) = self.fs.hash(directory, name, super::MAX_FILE)?;
        require(hash == record.sha256, "asset_store_digest")
    }
}
pub(super) fn read(fs: &mut Fs, root: Dir) -> Result<State> {
    fs.protected_dir(root, None)?;
    if fs.absent(root, "index.json")? {
        require(fs.list(root, 1)?.is_empty(), "asset_store_unindexed")?;
        return Ok(State {
            root,
            generations: vec![],
            original: None,
            index: None,
            directories: vec![],
            union: None,
            public: None,
            pointer: None,
            version: None,
        });
    }
    let index = fs.metadata(root, "index.json")?;
    require(fs.stamp(index).len() <= MAX_INDEX, "asset_index_size")?;
    let (bytes, _) = fs.read(root, "index.json", MAX_INDEX)?;
    let mut inspect = Inspect {
        fs,
        root,
        dirs: vec![],
    };
    let parsed = model::parse(&bytes, &mut inspect)?;
    let directories = inspect.dirs;
    let generations = parsed.generations;
    let bytes = parsed.original;
    let union_name = format!("union-{}", model::sha(&bytes));
    let mut expected = generations
        .iter()
        .map(|generation| generation.revision.clone())
        .collect::<BTreeSet<_>>();
    expected.extend(["index.json".into(), "public".into(), union_name.clone()]);
    require(
        fs.list(root, MAX_GENERATIONS + 4)?
            .into_iter()
            .collect::<BTreeSet<_>>()
            == expected,
        "asset_store_foreign",
    )?;
    let public = fs.directory(root, "public", None)?;
    let public_names = fs.list(public, 3)?.into_iter().collect::<BTreeSet<_>>();
    require(
        public_names == BTreeSet::from(["assets".into()])
            || public_names == BTreeSet::from(["assets".into(), "app-version.json".into()]),
        "asset_public_files",
    )?;
    let (target, pointer) = fs.pointer(public, "assets")?;
    require(
        target == format!("../{union_name}").as_bytes(),
        "asset_public_target",
    )?;
    let union = fs.directory(root, &union_name, None)?;
    let identities = model::identities(&generations)?;
    let names = identities
        .iter()
        .map(|value| value.0.clone())
        .collect::<BTreeSet<_>>();
    require(
        fs.list(union, MAX_FILES * MAX_GENERATIONS + 1)?
            .into_iter()
            .collect::<BTreeSet<_>>()
            == names,
        "asset_union_files",
    )?;
    for (name, _, record) in identities {
        let entry = fs.metadata(union, &name)?;
        require(fs.stamp(entry).len() == record.size, "asset_union_size")?;
        let (hash, _, _) = fs.hash(union, &name, super::MAX_FILE)?;
        require(hash == record.sha256, "asset_union_digest")?;
    }
    let version = if public_names.contains("app-version.json") {
        let entry = fs.metadata(public, "app-version.json")?;
        require(fs.stamp(entry).len() <= MAX_VERSION, "asset_version_size")?;
        let (bytes, _) = fs.read(public, "app-version.json", MAX_VERSION)?;
        model::version(&bytes)?;
        Some(entry)
    } else {
        None
    };
    require(
        generations.iter().map(Generation::bytes).sum::<u64>() <= MAX_TOTAL,
        "asset_index_total",
    )?;
    fs.verify()?;
    Ok(State {
        root,
        generations,
        original: Some(bytes),
        index: Some(index),
        directories,
        union: Some(union),
        public: Some(public),
        pointer: Some(pointer),
        version,
    })
}
/// Access the fixed managed store only while borrowing its live workload guard.
/// No operation announces a selected version unless select_version is requested.
pub struct Store<'a> {
    workload: &'a mut Workload,
    root: Option<Dir>,
}
impl<'a> Store<'a> {
    pub fn open(workload: &'a mut Workload) -> Result<Self> {
        workload.check()?;
        let root = if workload.fs.absent(workload.root, "browser-assets")? {
            None
        } else {
            Some(
                workload
                    .fs
                    .directory(workload.root, "browser-assets", None)?,
            )
        };
        workload.store_root = root;
        Ok(Self { workload, root })
    }
    pub fn initialize(&mut self) -> Result<bool> {
        self.workload.check()?;
        let root = match self.root {
            Some(root) => root,
            None => {
                let root = self
                    .workload
                    .fs
                    .mkdir(self.workload.root, "browser-assets", 0o755)?;
                self.root = Some(root);
                self.workload.store_root = Some(root);
                root
            }
        };
        let prior = read(&mut self.workload.fs, root)?;
        if prior.index.is_some() {
            return Ok(false);
        }
        commit::run(&mut self.workload.fs, prior, &[], &[])?;
        Ok(true)
    }
    pub fn inspect(&mut self) -> Result<Summary> {
        self.workload.check()?;
        let root = self.root.ok_or_else(|| fail("asset_store_ownership"))?;
        let prior = read(&mut self.workload.fs, root)?;
        Ok(Summary::new(&prior.generations))
    }
    pub fn publish(&mut self, captures: Vec<(String, Capture)>) -> Result<Published> {
        let now = timestamp()?;
        self.publish_at(captures, now)
    }
    pub(super) fn publish_at(
        &mut self,
        captures: Vec<(String, Capture)>,
        now: u64,
    ) -> Result<Published> {
        require(
            !captures.is_empty()
                && captures.len() <= super::MAX_CAPTURES
                && now.checked_add(super::MAX_AGE).is_some(),
            "asset_capture_count",
        )?;
        self.initialize()?;
        let root = self.root.unwrap();
        let prior = read(&mut self.workload.fs, root)?;
        let selected = model::select(&prior.generations, &captures, now)?;
        let summary = Summary::new(&selected);
        commit::run(&mut self.workload.fs, prior, &selected, &captures)?;
        Ok(Published {
            generations: summary.generations,
            files: summary.files,
            bytes: summary.bytes,
            maximum_age: super::MAX_AGE,
        })
    }
    pub fn prune(&mut self) -> Result<()> {
        self.prune_at(timestamp()?)
    }
    pub(super) fn prune_at(&mut self, now: u64) -> Result<()> {
        self.workload.check()?;
        let root = self.root.ok_or_else(|| fail("asset_store_ownership"))?;
        let prior = read(&mut self.workload.fs, root)?;
        let selected = prior
            .generations
            .iter()
            .filter(|entry| entry.expires > now)
            .cloned()
            .collect::<Vec<_>>();
        commit::run(&mut self.workload.fs, prior, &selected, &[])
    }
    pub fn select_version(&mut self, revision: &str) -> Result<bool> {
        require(model::revision(revision), "asset_selected_revision")?;
        self.workload.check()?;
        let root = self.root.ok_or_else(|| fail("asset_store_ownership"))?;
        let prior = read(&mut self.workload.fs, root)?;
        let public = prior.public.ok_or_else(|| fail("asset_operation_failed"))?;
        let mut bytes = serde_json::to_vec(&serde_json::json!({"revision":revision}))
            .map_err(|_| fail("asset_operation_failed"))?;
        bytes.push(b'\n');
        if let Some(version) = prior.version {
            let (existing, _) = self
                .workload
                .fs
                .read(public, "app-version.json", MAX_VERSION)?;
            if existing == bytes && self.workload.fs.mode(version) == 0o644 {
                return Ok(false);
            }
        }
        let name = format!(".version-{}", commit::nonce()?);
        let temporary = self.workload.fs.write(public, &name, &bytes, 0o644)?;
        let result = self
            .workload
            .fs
            .rename_entry(temporary, public, "app-version.json", prior.version)
            .and_then(|()| self.workload.fs.sync(public));
        let cleanup = self.workload.fs.absent(public, &name).and_then(|absent| {
            if absent {
                Ok(())
            } else {
                self.workload.fs.unlink(temporary)
            }
        });
        result.and(cleanup)?;
        self.workload.check()?;
        Ok(true)
    }
}
fn timestamp() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|_| fail("asset_operation_failed"))
}
