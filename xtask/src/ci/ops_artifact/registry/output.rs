//! Fresh descriptor-bound output, exact writer stamps and independent inventory.

use super::*;
use rustix::fs::{Mode, OFlags};
use std::{collections::BTreeMap, ffi::OsStr, fs::Metadata};

struct Node {
    stamp: Metadata,
    directory: bool,
    executable: bool,
    bytes: u64,
    digest: [u8; 32],
}
pub(super) struct Output {
    root: Held,
    nodes: BTreeMap<String, Node>,
    budget: Budget,
}
fn close(values: Vec<Held>) -> Result<()> {
    let mut result = Ok(());
    for value in values.into_iter().rev() {
        result = result.and(value.finish());
    }
    result
}
impl Output {
    pub fn new(run: &OwnedRun) -> Result<Self> {
        let root = run.directory("registry")?;
        let stamp = namespace::private_directory(&root)?;
        Ok(Self {
            root,
            nodes: BTreeMap::from([(
                String::new(),
                Node {
                    stamp,
                    directory: true,
                    executable: false,
                    bytes: 0,
                    digest: [0; 32],
                },
            )]),
            budget: run.budget().clone(),
        })
    }
    fn parents(&self, name: &str) -> Result<Vec<Held>> {
        let mut result = Vec::new();
        let mut prefix = String::new();
        let parent = name.rsplit_once('/').map_or("", |(p, _)| p);
        for part in parent.split('/').filter(|p| !p.is_empty()) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let ancestor = result.last().map_or(self.root.file(), Held::file);
            let held = Held::open(
                ancestor,
                OsStr::new(part),
                namespace::FLAGS | OFlags::DIRECTORY,
                &self.budget,
            )?;
            let stamp = namespace::private_directory(&held)?;
            if !namespace::binding(
                &self.nodes.get(&prefix).ok_or_else(|| failure(1))?.stamp,
                &stamp,
            ) {
                return Err(failure(1));
            }
            result.push(held);
        }
        Ok(result)
    }
    pub fn directory(&mut self, name: &str) -> Result<()> {
        if self.nodes.contains_key(name) {
            return Err(failure(1));
        }
        let parents = self.parents(name)?;
        let parent = parents.last().map_or(self.root.file(), Held::file);
        let basename = name.rsplit('/').next().unwrap();
        let result = (|| {
            rustix::fs::mkdirat(parent, basename, Mode::from_bits_truncate(0o700))
                .map_err(|_| failure(1))?;
            let directory = Held::open(
                parent,
                OsStr::new(basename),
                namespace::FLAGS | OFlags::DIRECTORY,
                &self.budget,
            )?;
            let stamp = namespace::private_directory(&directory);
            let closed = directory.finish();
            let stamp = stamp?;
            closed?;
            self.nodes.insert(
                name.into(),
                Node {
                    stamp,
                    directory: true,
                    executable: false,
                    bytes: 0,
                    digest: [0; 32],
                },
            );
            Ok(())
        })();
        result.and(close(parents))
    }
    pub fn write(&mut self, name: &str, bytes: &[u8], executable: bool) -> Result<()> {
        if self.nodes.contains_key(name) {
            return Err(failure(1));
        }
        let parents = self.parents(name)?;
        let parent = parents.last().map_or(self.root.file(), Held::file);
        let stamp = namespace::write_mode(
            parent,
            name.rsplit('/').next().unwrap(),
            bytes,
            executable,
            &self.budget,
        );
        let closed = close(parents);
        let stamp = stamp?;
        closed?;
        self.nodes.insert(
            name.into(),
            Node {
                stamp,
                directory: false,
                executable,
                bytes: bytes.len() as u64,
                digest: Sha256::digest(bytes).into(),
            },
        );
        Ok(())
    }
    pub fn check(&self, run: &OwnedRun) -> Result<()> {
        run.check()?;
        let root = Held::open(
            run.file(),
            OsStr::new("registry"),
            namespace::FLAGS | OFlags::DIRECTORY,
            &self.budget,
        )?;
        let matched =
            namespace::binding(&self.nodes[""].stamp, &namespace::private_directory(&root)?);
        root.finish()?;
        if !matched {
            return Err(failure(1));
        }
        for (name, node) in self.nodes.iter().filter(|(name, _)| !name.is_empty()) {
            let parents = self.parents(name)?;
            let parent = parents.last().map_or(self.root.file(), Held::file);
            let result = (|| {
                let held = Held::open(
                    parent,
                    OsStr::new(name.rsplit('/').next().unwrap()),
                    namespace::FLAGS,
                    &self.budget,
                )?;
                let checked = (|| {
                    let stamp = if node.directory {
                        namespace::private_directory(&held)?
                    } else {
                        namespace::private_file(&held, node.executable)?
                    };
                    if if node.directory {
                        namespace::binding(&node.stamp, &stamp)
                    } else {
                        namespace::same(&node.stamp, &stamp)
                    } {
                        Ok(())
                    } else {
                        Err(failure(1))
                    }
                })();
                checked.and(held.finish())
            })();
            result.and(close(parents))?;
        }
        Ok(())
    }
    pub async fn measure<E: Executor, C: Clock>(
        &self,
        run: &OwnedRun,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<materials::Identity> {
        self.check(run)?;
        let identity = materials::measure_bounded(
            &run.path.join("registry"),
            run.budget(),
            executor,
            deadline,
            EXPANDED,
        )
        .await?;
        self.check(run)?;
        let mut frame = Sha256::new();
        frame.update(b"ops-producer-tree-v1\0");
        for (name, node) in &self.nodes {
            frame.update((name.len() as u32).to_be_bytes());
            frame.update(name.as_bytes());
            frame.update([u8::from(node.directory), u8::from(node.executable)]);
            frame.update(node.bytes.to_be_bytes());
            frame.update(node.digest);
        }
        if identity.sha256 != hex(&frame.finalize()) || identity.entries > MEMBERS {
            return Err(failure(1));
        }
        Ok(identity)
    }
    pub fn finish(self, run: &OwnedRun) -> Result<()> {
        self.check(run).and(self.root.finish())
    }
}
