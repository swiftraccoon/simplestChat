//! Bounded, non-following input for offline reports. This never opens sockets or
//! follows result links; all failures become absent evidence without raw text.

use nix::fcntl::{OFlag, open};
use nix::sys::stat::Mode;
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub(super) const SAMPLE_BYTES: usize = 12 * 1024 * 1024;
const RESULTS_BYTES: u64 = 32 * 1024 * 1024;

/// Match the reference's immutable regular-file read, including an extra byte
/// to detect growth. The owned descriptor is explicitly closed on every path.
pub(super) fn results(path: &Path) -> Option<serde_json::Value> {
    let descriptor = open(
        path,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let mut file = File::from(descriptor);
    let result = (|| {
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > RESULTS_BYTES {
            return None;
        }
        let mut data = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(metadata.len() + 1)
            .read_to_end(&mut data)
            .ok()?;
        if data.len() as u64 != metadata.len() {
            return None;
        }
        serde_json::from_slice(&data).ok()
    })();
    nix::unistd::close(file).ok()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn result_reader_refuses_links_fifo_directories_and_oversized_files() {
        let root = std::env::temp_dir().join(format!("media-reader-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("results.json");
        fs::write(&path, b"[{\"private\":\"retained only internally\"}]").unwrap();
        assert!(results(&path).unwrap().is_array());
        let link = root.join("results-link");
        symlink(&path, &link).unwrap();
        let fifo = root.join("results-pipe");
        nix::unistd::mkfifo(&fifo, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        for unsafe_path in [&link, &fifo, &root, &root.join("missing")] {
            assert!(results(unsafe_path).is_none());
        }
        File::create(&path)
            .unwrap()
            .set_len(RESULTS_BYTES + 1)
            .unwrap();
        assert!(results(&path).is_none());
        fs::write(&path, b"\xff").unwrap();
        assert!(results(&path).is_none());
        fs::write(&path, b"{\"private\":1e400}").unwrap();
        assert!(
            results(&path).is_none(),
            "valid JSON overflow is explicitly refused"
        );
        fs::write(&path, b"{\"private\":\"\\ud800\"}").unwrap();
        assert!(
            results(&path).is_none(),
            "unpaired UTF-16 is explicitly refused"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
