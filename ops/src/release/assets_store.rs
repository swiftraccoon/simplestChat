//! Validated complete store snapshots and fixed operations under one guard.
use super::{
    MAX_FILES, MAX_GENERATIONS, MAX_INDEX, MAX_TOTAL, MAX_VERSION, Result, commit, fail,
    files::{Dir, Entry, Fs, Pointer, ScopedFs},
    lock::Workload,
    model::{self, Capture, Generation, Record},
    require,
    scope::Scope,
};
use serde::Serialize;
use std::{borrow::BorrowMut, collections::BTreeSet};

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
struct Inspect<'a, 'flow, 'owner, T> {
    fs: &'a mut ScopedFs<'flow, 'owner, T>,
    root: Dir,
    dirs: Vec<(String, Dir)>,
}
impl<T: BorrowMut<Fs>> model::Inspect for Inspect<'_, '_, '_, T> {
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
    read_scoped(&mut ScopedFs::new(fs, &Scope::standalone()), root)
}
pub(super) fn read_scoped<T: BorrowMut<Fs>>(
    fs: &mut ScopedFs<'_, '_, T>,
    root: Dir,
) -> Result<State> {
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
    let union_name = format!("union-{}", model::sha_scoped(&bytes, fs.scope())?);
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
        Self::open_scoped(workload, &Scope::standalone())
    }
    pub(super) fn open_scoped(workload: &'a mut Workload, scope: &Scope<'_>) -> Result<Self> {
        workload.check_scoped(scope)?;
        let mut fs = ScopedFs::new(&mut workload.fs, scope);
        let root = if fs.absent(workload.root, "browser-assets")? {
            None
        } else {
            Some(fs.directory(workload.root, "browser-assets", None)?)
        };
        workload.store_root = root;
        Ok(Self { workload, root })
    }
    pub fn initialize(&mut self) -> Result<bool> {
        self.initialize_scoped(&Scope::standalone())
    }
    fn initialize_scoped(&mut self, scope: &Scope<'_>) -> Result<bool> {
        self.workload.check_scoped(scope)?;
        let mut fs = ScopedFs::new(&mut self.workload.fs, scope);
        let root = match self.root {
            Some(root) => root,
            None => {
                let root = fs.mkdir(self.workload.root, "browser-assets", 0o755)?;
                self.root = Some(root);
                self.workload.store_root = Some(root);
                root
            }
        };
        let prior = read_scoped(&mut fs, root)?;
        if prior.index.is_some() {
            return Ok(false);
        }
        commit::run_scoped(&mut fs, prior, &[], &[])?;
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
        self.publish_at_scoped(captures, now, &Scope::standalone())
    }
    pub(super) fn publish_at_scoped(
        &mut self,
        captures: Vec<(String, Capture)>,
        now: u64,
        scope: &Scope<'_>,
    ) -> Result<Published> {
        let result = (|| {
            scope.check()?;
            require(
                !captures.is_empty()
                    && captures.len() <= super::MAX_CAPTURES
                    && now.checked_add(super::MAX_AGE).is_some(),
                "asset_capture_count",
            )?;
            self.initialize_scoped(scope)?;
            let root = self.root.unwrap();
            let mut fs = ScopedFs::new(&mut self.workload.fs, scope);
            let prior = read_scoped(&mut fs, root)?;
            let selected = model::select_scoped(&prior.generations, &captures, now, scope)?;
            let summary = Summary::new(&selected);
            commit::run_scoped(&mut fs, prior, &selected, &captures)?;
            Ok(Published {
                generations: summary.generations,
                files: summary.files,
                bytes: summary.bytes,
                maximum_age: super::MAX_AGE,
            })
        })();
        scope.result(result)
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
        self.select_version_scoped(revision, &Scope::standalone())
    }
    pub(super) fn select_version_scoped(
        &mut self,
        revision: &str,
        scope: &Scope<'_>,
    ) -> Result<bool> {
        let result = (|| {
            scope.check()?;
            require(model::revision(revision), "asset_selected_revision")?;
            self.workload.check_scoped(scope)?;
            let root = self.root.ok_or_else(|| fail("asset_store_ownership"))?;
            let mut fs = ScopedFs::new(&mut self.workload.fs, scope);
            let prior = read_scoped(&mut fs, root)?;
            let public = prior.public.ok_or_else(|| fail("asset_operation_failed"))?;
            let mut bytes = serde_json::to_vec(&serde_json::json!({"revision":revision}))
                .map_err(|_| fail("asset_operation_failed"))?;
            bytes.push(b'\n');
            if let Some(version) = prior.version {
                let (existing, _) = fs.read(public, "app-version.json", MAX_VERSION)?;
                if existing == bytes && fs.mode(version) == 0o644 {
                    return Ok(false);
                }
            }
            let name = format!(".version-{}", commit::nonce_scoped(scope)?);
            let temporary = fs.write(public, &name, &bytes, 0o644)?;
            let result = fs
                .rename_entry(temporary, public, "app-version.json", prior.version)
                .and_then(|()| fs.sync(public));
            scope.observed(&result);
            let cleanup = {
                let _recovery = scope.recovery_if(result.is_err());
                fs.absent(public, &name)
                    .and_then(|absent| if absent { Ok(()) } else { fs.unlink(temporary) })
            };
            result.and(cleanup)?;
            self.workload.check_scoped(scope)?;
            Ok(true)
        })();
        scope.result(result)
    }
}
fn timestamp() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(|_| fail("asset_operation_failed"))
}
