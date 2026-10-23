//! Fixed compiler roles and resolver evidence. No executable is considered
//! relocatable merely because its archive was copied to a different prefix.

use super::{archive::Graph, failure, files::Tree, shim};
use crate::tooling::Result;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub(super) const PROGRAMS: [&str; 4] = ["cc1", "collect2", "as", "ld"];
pub(super) const CRT: [&str; 7] = [
    "Scrt1.o",
    "crti.o",
    "crtn.o",
    "crtbeginS.o",
    "crtendS.o",
    "libgcc.a",
    "libc.so",
];

pub(super) fn complete(
    tree: &mut Tree,
    graph: &mut Graph,
    checkpoint: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    graph.resolve(tree, checkpoint)?;
    for name in ["as", "ld", "ar", "nm", "ranlib", "strip"] {
        let source = format!("host-tools/sysroot/usr/bin/x86_64-linux-gnu-{name}");
        let destination = format!("host-tools/sysroot/usr/bin/{name}");
        graph.copy(tree, &source, &destination, checkpoint)?;
    }
    // Modern Ubuntu uses /usr merging. Materialize those controlled aliases
    // with independent files so linker scripts cannot reach the host's /lib.
    for component in ["lib", "lib64"] {
        let prefix = format!("host-tools/sysroot/usr/{component}");
        let names = tree
            .records
            .keys()
            .filter(|p| p.starts_with(&format!("{prefix}/")))
            .cloned()
            .collect::<Vec<_>>();
        for source in names {
            graph.copy(
                tree,
                &source,
                &format!("host-tools/sysroot/{component}{}", &source[prefix.len()..]),
                checkpoint,
            )?;
        }
    }
    for role in [
        shim::GCC,
        "toolchain/bin/cargo",
        "toolchain/bin/rustc",
        "toolchain/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld",
    ] {
        let record = tree.records.get(role).ok_or_else(|| failure(1))?;
        if record.bytes == 0 || !executable(record) {
            return Err(failure(1));
        }
    }
    if !tree
        .directory_names()
        .any(|p| p == "toolchain/lib/rustlib/x86_64-unknown-linux-musl/lib")
    {
        return Err(failure(1));
    }
    tree.write("host-tools/bin/cc", shim::CC, true)?;
    tree.check()
}

fn executable(record: &super::files::Record) -> bool {
    use std::os::unix::fs::MetadataExt;
    record.stamp.mode() & 0o111 != 0
}
pub(super) fn arguments() -> Vec<Vec<OsString>> {
    let mut args = vec![
        vec!["--version".into()],
        vec!["-print-sysroot".into()],
        vec!["-print-search-dirs".into()],
    ];
    args.extend(
        PROGRAMS
            .iter()
            .map(|p| vec![format!("-print-prog-name={p}").into()]),
    );
    args.extend(
        CRT.iter()
            .map(|p| vec![format!("-print-file-name={p}").into()]),
    );
    args
}
fn path(bytes: &[u8]) -> Result<PathBuf> {
    let text = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() || text.len() > 4096 || text.chars().any(char::is_control) {
        return Err(failure(1));
    }
    Ok(PathBuf::from(text))
}
pub(super) fn validate(index: usize, bytes: &[u8], tree: &Tree) -> Result<()> {
    if bytes.len() > 65536 {
        return Err(failure(1));
    }
    if index == 0 {
        let text = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
        if !text
            .lines()
            .next()
            .is_some_and(|line| line.contains("13.3.0"))
        {
            return Err(failure(1));
        }
        return Ok(());
    }
    if index == 1 {
        return if path(bytes)? == tree.run.path.join("host-tools/sysroot") {
            Ok(())
        } else {
            Err(failure(1))
        };
    }
    if index == 2 {
        // Search directories are bounded diagnostics, not authority: GCC may
        // still list compiled-in fallbacks. Actual fixed role probes below
        // must resolve within the admitted kit; complete I/O isolation is not
        // inferred from a search-directory listing.
        let text = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
        return if text.lines().count() == 3
            && ["install:", "programs:", "libraries:"]
                .iter()
                .all(|prefix| text.lines().any(|line| line.starts_with(prefix)))
        {
            Ok(())
        } else {
            Err(failure(1))
        };
    }
    let selected = path(bytes)?;
    let normalized = selected.components().fold(PathBuf::new(), |mut out, c| {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            _ => out.push(c.as_os_str()),
        }
        out
    });
    let relative = normalized
        .strip_prefix(&tree.run.path)
        .map_err(|_| failure(1))?
        .to_str()
        .ok_or_else(|| failure(1))?;
    let expected = if index < 3 + PROGRAMS.len() {
        let name = PROGRAMS[index - 3];
        match name {
            "cc1" | "collect2" => vec![
                format!("host-tools/sysroot/usr/lib/gcc/x86_64-linux-gnu/13/{name}"),
                format!("host-tools/sysroot/usr/libexec/gcc/x86_64-linux-gnu/13/{name}"),
            ],
            _ => vec![
                format!("host-tools/sysroot/usr/bin/{name}"),
                format!("host-tools/sysroot/usr/bin/x86_64-linux-gnu-{name}"),
            ],
        }
    } else {
        let name = *CRT
            .get(index - 3 - PROGRAMS.len())
            .ok_or_else(|| failure(1))?;
        vec![
            format!("host-tools/sysroot/usr/lib/gcc/x86_64-linux-gnu/13/{name}"),
            format!("host-tools/sysroot/usr/lib/x86_64-linux-gnu/{name}"),
            format!("host-tools/sysroot/lib/x86_64-linux-gnu/{name}"),
        ]
    };
    if !expected.iter().any(|p| p == relative) || !tree.records.contains_key(relative) {
        return Err(failure(1));
    }
    tree.open(relative)?.finish()
}

pub(super) fn environment(root: &Path) -> Result<crate::tooling::Environment> {
    let mut environment = crate::tooling::Environment::new();
    environment.extend(
        shim::environment(root)?
            .into_iter()
            .map(|(k, v)| (k.into(), v)),
    );
    environment.insert("LC_ALL".into(), "C".into());
    environment.insert("LANG".into(), "C".into());
    environment.insert(
        "PATH".into(),
        root.join("host-tools/sysroot/usr/bin").into_os_string(),
    );
    Ok(environment)
}
