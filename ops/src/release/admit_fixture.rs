//! An inert, valid export and the checkout it was made from, for tests of
//! whatever handles one. Nothing here runs, signs or deploys anything.

use serde_json::{Map, Value};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

/// The revision the inert export is a build of.
pub const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub(crate) const RECORDED: &str = include_str!("recorded_authorize.json");

pub(crate) fn hex(text: &str) -> Vec<u8> {
    let digits = text.as_bytes().chunks(2);
    let byte = |pair: &[u8]| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    digits.map(byte).collect()
}

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, PermissionsExt::from_mode(mode)).unwrap();
}

/// Build one recorded tree under `base`: entries in name order, modes last
/// and deepest first.
pub(crate) fn planted(base: &Path, blobs: &Value, tree: &Map<String, Value>) {
    let mut names: Vec<&String> = tree.keys().collect();
    names.sort();
    for name in &names {
        let (entry, path) = (&tree[*name], base.join(name));
        if let Some(link) = entry["link"].as_str() {
            std::os::unix::fs::symlink(base.join(link), &path).unwrap();
        } else if entry["directory"] == true {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
        } else if let Some(length) = entry["sparse"].as_u64() {
            // A size without the bytes: nothing may read it.
            std::fs::File::create(&path)
                .unwrap()
                .set_len(length)
                .unwrap();
        } else if let Some(blob) = entry["blob"].as_str() {
            std::fs::write(&path, hex(blobs[blob].as_str().unwrap())).unwrap();
        } else {
            // Padding in front fills a file to an exact size.
            let pad = usize::try_from(entry["pad"].as_u64().unwrap_or_default()).unwrap();
            let mut bytes = vec![b' '; pad];
            let spelled = &entry["text"];
            match spelled.as_str() {
                Some(text) => bytes.extend_from_slice(text.as_bytes()),
                None => {
                    let count = usize::try_from(spelled["count"].as_u64().unwrap()).unwrap();
                    bytes.extend(spelled["repeat"].as_str().unwrap().repeat(count).bytes());
                }
            }
            std::fs::write(&path, bytes).unwrap();
        }
    }
    for name in names.iter().rev() {
        let entry = &tree[*name];
        if !entry["link"].is_string() {
            let spelled = entry["mode"].as_str().unwrap().strip_prefix("0o").unwrap();
            chmod(&base.join(name), u32::from_str_radix(spelled, 8).unwrap());
        }
    }
}

/// Write the valid checkout and its export under `base`, as `base/checkout`
/// and `base/artifact`: five source inputs, one migration, and a passed
/// build of `REVISION` whose image archive its manifest describes.
pub fn plant(base: &Path) {
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    let tree = recorded["cases"][0]["tree"].as_object().unwrap();
    planted(base, &recorded["blobs"], tree);
}
