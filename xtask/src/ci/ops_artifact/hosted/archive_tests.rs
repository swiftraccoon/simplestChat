//! Synthetic complete installer/deb graphs. No real package body is acquired.

use super::super::namespace::Budget;
use super::{
    archive::Graph,
    catalog::{Artifact, Catalog, Format},
    files::Tree,
    fixture,
};
use flate2::{Compression, write::GzEncoder};
use std::{fs, io::Write, os::unix::fs::MetadataExt};

struct Entry<'a> {
    name: &'a str,
    data: &'a [u8],
    kind: u8,
    target: Option<&'a str>,
}
fn file<'a>(name: &'a str, data: &'a [u8]) -> Entry<'a> {
    Entry {
        name,
        data,
        kind: b'0',
        target: None,
    }
}
fn directory(name: &str) -> Entry<'_> {
    Entry {
        name,
        data: b"",
        kind: b'5',
        target: None,
    }
}
fn tar(entries: &[Entry<'_>]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::new(entry.kind));
        header.set_size(entry.data.len() as u64);
        if let Some(target) = entry.target {
            header.set_link_name(target).unwrap();
        }
        header.set_cksum();
        builder
            .append_data(&mut header, entry.name, entry.data)
            .unwrap();
    }
    builder.finish().unwrap();
    builder.into_inner().unwrap()
}

pub(super) fn tar_files(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    tar(&entries
        .iter()
        .map(|(name, data)| file(name, data))
        .collect::<Vec<_>>())
}
pub(super) fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut writer = GzEncoder::new(Vec::new(), Compression::fast());
    writer.write_all(bytes).unwrap();
    writer.finish().unwrap()
}
fn rust(manifest: &[u8], version: &[u8], extra: &[Entry<'_>]) -> (Artifact, Vec<u8>) {
    rust_component("rustc", manifest, version, extra)
}
fn rust_component(
    component: &str,
    manifest: &[u8],
    version: &[u8],
    extra: &[Entry<'_>],
) -> (Artifact, Vec<u8>) {
    let artifact = Catalog::load()
        .unwrap()
        .artifacts
        .into_iter()
        .find(|p| p.component == component)
        .unwrap();
    let root = artifact.archive_root.clone();
    let components = format!("{root}/components");
    let installer = format!("{root}/rust-installer-version");
    let declarations = format!("{root}/{component}/manifest.in");
    let compiler = format!("{root}/{component}/bin/rustc");
    let components_body = format!("{component}\n");
    let mut entries = vec![
        file(&components, components_body.as_bytes()),
        file(&installer, version),
        file(&declarations, manifest),
        file(&compiler, b"inert compiler bytes"),
    ];
    entries.extend(extra.iter().map(|entry| Entry {
        name: entry.name,
        data: entry.data,
        kind: entry.kind,
        target: entry.target,
    }));
    let bytes = gzip(&tar(&entries));
    (artifact, bytes)
}
fn deb() -> Artifact {
    Catalog::load()
        .unwrap()
        .artifacts
        .into_iter()
        .find(|p| p.format == Format::UbuntuDeb)
        .unwrap()
}
fn admitted(artifact: &Artifact, bytes: &[u8], expected: bool, resolve: bool) -> Option<Vec<u8>> {
    let fixture = fixture::Fixture::new();
    let budget = Budget::default();
    let source = fixture.root.join("archive");
    fs::write(&source, bytes).unwrap();
    let input = fs::File::open(source).unwrap();
    let mut tree = Tree::create(&fixture.root.join("kit"), &budget).unwrap();
    let mut graph = Graph::default();
    let result = graph
        .import(&input, artifact, &mut tree, &mut || Ok(()))
        .and_then(|()| {
            if resolve {
                graph.resolve(&mut tree, &mut || Ok(()))
            } else {
                Ok(())
            }
        });
    assert_eq!(
        result.is_ok(),
        expected,
        "unexpected synthetic archive verdict"
    );
    let selected = tree
        .records
        .keys()
        .next()
        .cloned()
        .and_then(|name| tree.read(&name, 1024, &mut || Ok(())).ok());
    nix::unistd::close(input).unwrap();
    let _ = tree.finish();
    assert_eq!(budget.count(), 0);
    selected
}

#[test]
fn complete_rust_installer_component_requires_exact_declarations_and_integrity() {
    let (artifact, bytes) = rust(b"file:bin/rustc\n", b"3\n", &[]);
    assert_eq!(
        admitted(&artifact, &bytes, true, true).unwrap(),
        b"inert compiler bytes"
    );
    for (manifest, version) in [
        (b"file:bin/missing\n".as_slice(), b"3\n".as_slice()),
        (b"file:bin/rustc\n", b"2\n"),
        (b"file:../escape\n", b"3\n"),
        (b"file:bin/rustc\nfile:bin/rustc\n", b"3\n"),
    ] {
        let (artifact, bytes) = rust(manifest, version, &[]);
        admitted(&artifact, &bytes, false, false);
    }
    let mut corrupt = bytes.clone();
    let trailer = corrupt.len() - 8;
    corrupt[trailer] ^= 1;
    admitted(&artifact, &corrupt, false, false);
    let mut appended = bytes.clone();
    appended.extend_from_slice(&gzip(&[]));
    admitted(&artifact, &appended, false, false);
    admitted(&artifact, &bytes[..bytes.len() - 1], false, false);
}

#[test]
fn file_only_rust_manifest_admits_exact_structural_parent_directories() {
    let artifact = Catalog::load()
        .unwrap()
        .artifacts
        .into_iter()
        .find(|p| p.component == "rustc")
        .unwrap();
    let bin = format!("{}/rustc/bin", artifact.archive_root);
    let lib = format!("{}/rustc/lib", artifact.archive_root);
    let nested = format!("{lib}/nested");
    let payload = format!("{nested}/library");
    for manifest in [
        b"file:bin/rustc\nfile:lib/nested/library\n".as_slice(),
        b"file:bin/rustc\ndir:lib/nested\n",
    ] {
        let (artifact, bytes) = rust(
            manifest,
            b"3\n",
            &[
                directory(&bin),
                directory(&lib),
                directory(&nested),
                file(&payload, b"inert library bytes"),
            ],
        );
        admitted(&artifact, &bytes, true, true);
    }
}

#[test]
fn structural_parents_authorize_no_undeclared_payload_or_empty_directory() {
    let artifact = Catalog::load()
        .unwrap()
        .artifacts
        .into_iter()
        .find(|p| p.component == "rustc")
        .unwrap();
    let bin = format!("{}/rustc/bin", artifact.archive_root);
    let extra = format!("{bin}/extra");
    for entry in [
        directory(&extra),
        file(&extra, b"undeclared payload"),
        Entry {
            name: &extra,
            data: b"",
            kind: b'2',
            target: Some("rustc"),
        },
    ] {
        let (artifact, bytes) = rust(b"file:bin/rustc\n", b"3\n", &[directory(&bin), entry]);
        admitted(&artifact, &bytes, false, true);
    }
    for manifest in [
        b"file:bin\nfile:bin/rustc\n".as_slice(),
        b"dir:bin/rustc\n",
        b"file:bin/rustc\nfile:bin/missing\n",
        b"file:bin/rustc\ndir:bin/missing\n",
    ] {
        let (artifact, bytes) = rust(manifest, b"3\n", &[directory(&bin)]);
        admitted(&artifact, &bytes, false, false);
    }
}

#[test]
fn only_the_pinned_gnu_std_empty_packaging_directory_is_admitted() {
    let catalog = Catalog::load().unwrap();
    for component in [
        "rust-std-x86_64-unknown-linux-gnu",
        "rust-std-x86_64-unknown-linux-musl",
        "rustc",
    ] {
        let artifact = catalog
            .artifacts
            .iter()
            .find(|p| p.component == component)
            .unwrap();
        let parent = format!("{}/{component}", artifact.archive_root);
        let name = format!("{parent}/lib/rustlib/x86_64-unknown-linux-gnu/lib/self-contained");
        let (artifact, bytes) =
            rust_component(component, b"file:bin/rustc\n", b"3\n", &[directory(&name)]);
        admitted(
            &artifact,
            &bytes,
            component == "rust-std-x86_64-unknown-linux-gnu",
            true,
        );
        for entry in [
            file(&name, b"undeclared payload"),
            Entry {
                name: &name,
                data: b"nonempty directory body",
                kind: b'5',
                target: None,
            },
        ] {
            let (artifact, bytes) =
                rust_component(component, b"file:bin/rustc\n", b"3\n", &[entry]);
            admitted(&artifact, &bytes, false, false);
        }
        let sibling = format!("{name}-other");
        let child = format!("{name}/extra");
        for entry in [directory(&sibling), file(&child, b"undeclared child")] {
            let (artifact, bytes) =
                rust_component(component, b"file:bin/rustc\n", b"3\n", &[entry]);
            admitted(&artifact, &bytes, false, false);
        }
    }
}

#[test]
fn deb_projection_never_runs_scripts_and_materializes_links_as_independent_bytes() {
    let fixture = fixture::Fixture::new();
    let budget = Budget::default();
    let artifact = deb();
    let bytes = tar(&[
        file("usr/lib/x86_64-linux-gnu/original", b"owned payload"),
        Entry {
            name: "usr/lib/x86_64-linux-gnu/relative",
            data: b"",
            kind: b'2',
            target: Some("original"),
        },
        Entry {
            name: "usr/lib/x86_64-linux-gnu/absolute",
            data: b"",
            kind: b'2',
            target: Some("/usr/lib/x86_64-linux-gnu/original"),
        },
        Entry {
            name: "usr/lib/x86_64-linux-gnu/hard",
            data: b"",
            kind: b'1',
            target: Some("usr/lib/x86_64-linux-gnu/original"),
        },
        Entry {
            name: "usr/lib/alias",
            data: b"",
            kind: b'2',
            target: Some("x86_64-linux-gnu"),
        },
        file("DEBIAN/postinst", b"must never execute or appear"),
        file("etc/PRIVATE-config", b"must not enter sysroot"),
    ]);
    let source = fixture.root.join("tar");
    fs::write(&source, bytes).unwrap();
    let input = fs::File::open(source).unwrap();
    let mut tree = Tree::create(&fixture.root.join("kit"), &budget).unwrap();
    let mut graph = Graph::default();
    graph
        .import(&input, &artifact, &mut tree, &mut || Ok(()))
        .unwrap();
    graph.resolve(&mut tree, &mut || Ok(())).unwrap();
    for name in ["original", "relative", "absolute", "hard"] {
        let name = format!("host-tools/sysroot/usr/lib/x86_64-linux-gnu/{name}");
        assert_eq!(
            tree.read(&name, 1024, &mut || Ok(())).unwrap(),
            b"owned payload"
        );
        assert_eq!(tree.records[&name].stamp.nlink(), 1);
    }
    assert_eq!(
        tree.read(
            "host-tools/sysroot/usr/lib/alias/original",
            1024,
            &mut || Ok(())
        )
        .unwrap(),
        b"owned payload"
    );
    assert!(
        !tree
            .records
            .keys()
            .any(|name| name.contains("postinst") || name.contains("PRIVATE"))
    );
    let inodes = tree
        .records
        .values()
        .map(|record| record.stamp.ino())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(inodes.len(), tree.records.len());
    nix::unistd::close(input).unwrap();
    tree.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

#[test]
fn closed_tar_profile_refuses_cycles_missing_links_specials_extensions_and_tail_damage() {
    let artifact = deb();
    for entries in [
        vec![
            Entry {
                name: "usr/bin/a",
                data: b"",
                kind: b'2',
                target: Some("b"),
            },
            Entry {
                name: "usr/bin/b",
                data: b"",
                kind: b'2',
                target: Some("a"),
            },
        ],
        vec![Entry {
            name: "usr/bin/a",
            data: b"",
            kind: b'2',
            target: Some("missing"),
        }],
        vec![Entry {
            name: "usr/bin/a",
            data: b"",
            kind: b'2',
            target: Some("../../../../escape"),
        }],
        vec![Entry {
            name: "usr/bin/a",
            data: b"",
            kind: b'6',
            target: None,
        }],
        vec![Entry {
            name: "usr/bin/a",
            data: b"",
            kind: b'x',
            target: None,
        }],
        vec![file("usr/bin/a", b"first"), file("usr/bin/a", b"second")],
    ] {
        admitted(&artifact, &tar(&entries), false, true);
    }
    let valid = tar(&[file("usr/bin/a", b"one")]);
    let mut trailing = valid.clone();
    trailing.push(1);
    admitted(&artifact, &trailing, false, false);
    admitted(&artifact, &valid[..1024], false, false);
    let mut bad = valid;
    let mut header = tar::Header::new_gnu();
    header.as_mut_bytes().copy_from_slice(&bad[..512]);
    header.as_mut_bytes()[0] = b'/';
    header.set_cksum();
    bad[..512].copy_from_slice(header.as_bytes());
    admitted(&artifact, &bad, false, false);
}

#[test]
fn identical_cross_package_files_merge_but_conflicting_payload_or_mode_refuses() {
    let fixture = fixture::Fixture::new();
    let budget = Budget::default();
    let artifact = deb();
    let mut tree = Tree::create(&fixture.root.join("kit"), &budget).unwrap();
    let mut graph = Graph::default();
    for (index, data, expected) in [
        (0, b"same".as_slice(), true),
        (1, b"same", true),
        (2, b"wrong", false),
        (3, b"same", false),
    ] {
        let source = fixture.root.join(format!("tar-{index}"));
        let mut bytes = tar(&[file("usr/bin/shared", data)]);
        if index == 3 {
            let mut header = tar::Header::new_gnu();
            header.as_mut_bytes().copy_from_slice(&bytes[..512]);
            header.set_mode(0o644);
            header.set_cksum();
            bytes[..512].copy_from_slice(header.as_bytes());
        }
        fs::write(&source, bytes).unwrap();
        let input = fs::File::open(source).unwrap();
        assert_eq!(
            graph
                .import(&input, &artifact, &mut tree, &mut || Ok(()))
                .is_ok(),
            expected
        );
        nix::unistd::close(input).unwrap();
    }
    assert_eq!(
        tree.read("host-tools/sysroot/usr/bin/shared", 8, &mut || Ok(()))
            .unwrap(),
        b"same"
    );
    tree.finish().unwrap();
    assert_eq!(budget.count(), 0);
}
