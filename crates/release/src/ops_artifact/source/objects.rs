//! Verify raw commit/tree hashes explicitly: recursive ls-tree does not verify
//! nested loose-tree payloads against their filenames. No Git object parser is copied.

use super::*;
use std::{collections::BTreeMap, fs::Metadata};

struct Record {
    sha256: String,
    stamp: Metadata,
}
pub(super) struct Objects {
    directory: Held,
    records: BTreeMap<String, Record>,
    bytes: u64,
}
impl Objects {
    pub fn create(run: &OwnedRun) -> Result<Self> {
        Ok(Self {
            directory: run.directory("object-checks")?,
            records: BTreeMap::new(),
            bytes: 0,
        })
    }
    pub async fn check<E: Executor, C: Clock>(
        &mut self,
        context: &Context<'_, C>,
        executor: &mut E,
        kind: &str,
        oid: &str,
    ) -> Result<Vec<u8>> {
        let maximum = if kind == "commit" {
            1024 * 1024
        } else {
            catalog::CATALOG
        };
        let raw = context
            .call(executor, &["cat-file", kind, oid], maximum)
            .await?;
        let name = format!("{kind}-{oid}");
        if let Some(record) = self.records.get(&name) {
            if sha(&raw) != record.sha256 {
                return Err(failure(1));
            }
        } else {
            self.bytes = self
                .bytes
                .checked_add(raw.len() as u64)
                .ok_or_else(|| failure(1))?;
            if self.bytes > 8 * 1024 * 1024 || self.records.len() >= catalog::ENTRIES + 2 {
                return Err(failure(1));
            }
            let stamp = namespace::write(self.directory.file(), &name, &raw, context.run.budget())?;
            self.records.insert(
                name.clone(),
                Record {
                    sha256: sha(&raw),
                    stamp,
                },
            );
        }
        let path = context.run.path.join("object-checks").join(&name);
        let record = &self.records[&name];
        let before =
            namespace::read_bound(&path, maximum, context.run.budget(), Some(&record.stamp))?;
        if before != raw {
            return Err(failure(1));
        }
        let result = context
            .call(
                executor,
                &[
                    "hash-object",
                    "--no-filters",
                    "-t",
                    kind,
                    "--",
                    path.to_str().ok_or_else(|| failure(1))?,
                ],
                64,
            )
            .await?;
        let after =
            namespace::read_bound(&path, maximum, context.run.budget(), Some(&record.stamp))?;
        if before != after || catalog::oid(&result)? != oid {
            return Err(failure(1));
        }
        Ok(raw)
    }
    pub fn finish(self) -> Result<()> {
        self.directory.finish()
    }
}
