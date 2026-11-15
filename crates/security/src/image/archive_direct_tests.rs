//! The bounds at their edges, and what no recorded case reaches: the
//! reference's bounds were constants of its module, so its recorder could
//! reach only those that are cheap to reach.

use super::super::{
    Limits, canonical, extract, image_identity, layer_file, materialize_layers, new_file, number,
    physical_headers, read_layer,
};
use crate::{digest, scratch::Scratch};
use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

const REVIEWED: Limits = Limits::REVIEWED;

enum Member<'a> {
    File(&'a [u8]),
    Directory,
    Symlink(&'a str),
    Hardlink(&'a str),
    /// A header of another kind with its body: a long name, a long
    /// target, the attributes of the next member or of every member.
    Other(u8, &'a [u8]),
}
use Member::{Directory, File, Hardlink, Other, Symlink};

/// A tar archive of `members`, each named exactly as it is given.
fn packed(members: &[(&str, Member<'_>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, member) in members {
        let (kind, body, link) = match member {
            File(body) => (tar::EntryType::Regular, *body, ""),
            Directory => (tar::EntryType::Directory, &[][..], ""),
            Symlink(target) => (tar::EntryType::Symlink, &[][..], *target),
            Hardlink(target) => (tar::EntryType::Link, &[][..], *target),
            Other(kind, body) => (tar::EntryType::new(*kind), *body, ""),
        };
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(kind);
        header.set_mode(0o644);
        header.set_size(body.len() as u64);
        let old = header.as_old_mut();
        old.name[..name.len()].copy_from_slice(name.as_bytes());
        old.linkname[..link.len()].copy_from_slice(link.as_bytes());
        header.set_cksum();
        builder.append(&header, body).expect("a member");
    }
    builder.into_inner().expect("an archive")
}

/// Give a raw header that was changed its check again.
fn sealed(block: &mut [u8]) {
    block[148..156].fill(b' ');
    let sum: u32 = block.iter().map(|byte| u32::from(*byte)).sum();
    block[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

/// The configuration of an image of `layers`.
fn described(layers: &[Vec<u8>]) -> String {
    let ids = layers
        .iter()
        .map(|layer| format!("\"sha256:{}\"", digest(layer)));
    let ids: Vec<String> = ids.collect();
    format!("{{\"rootfs\":{{\"diff_ids\":[{}]}}}}", ids.join(","))
}

/// The list of a saved image whose layers are `0.tar`, `1.tar` and so on.
fn list(layers: usize) -> String {
    let names: Vec<String> = (0..layers)
        .map(|index| format!("\"{index}.tar\""))
        .collect();
    format!(
        "[{{\"Config\":\"config.json\",\"Layers\":[{}]}}]",
        names.join(",")
    )
}

/// A saved image in a directory of its own, and where its evidence goes.
struct Saved {
    _scratch: Scratch,
    archive: PathBuf,
    output: PathBuf,
}

fn saved(members: &[(&str, Member<'_>)]) -> Saved {
    let scratch = Scratch::new("image-direct");
    let (archive, output) = (scratch.0.join("image.tar"), scratch.0.join("evidence"));
    std::fs::write(&archive, packed(members)).expect("an image");
    Saved {
        _scratch: scratch,
        archive,
        output,
    }
}

/// What unpacking the image of `layers` comes to: the pretty report, or
/// the refusal. The image's list and its stored configuration are the
/// usual ones unless they are given.
fn unpacked(
    layers: &[Vec<u8>],
    list_and_stored: Option<(&str, &[u8])>,
    given: Option<&[u8]>,
    limits: Limits,
) -> Result<String, &'static str> {
    let (usual_list, usual) = (list(layers.len()), described(layers));
    let (list, stored) = list_and_stored.unwrap_or((&usual_list, usual.as_bytes()));
    let names: Vec<String> = (0..layers.len())
        .map(|index| format!("{index}.tar"))
        .collect();
    let mut members = vec![
        ("manifest.json", File(list.as_bytes())),
        ("config.json", File(stored)),
    ];
    members.extend(
        names
            .iter()
            .zip(layers)
            .map(|(name, layer)| (name.as_str(), File(layer))),
    );
    let image = saved(&members);
    let given = given.unwrap_or(usual.as_bytes());
    let came = materialize_layers(&image.archive, given, &image.output, &limits);
    let came = came.map(|report| crate::document::Document::Record(report).pretty());
    came.map_err(|fault| fault.0)
}

/// The paths of the final file system that the image of `layers` has.
fn built(layers: &[Vec<u8>]) -> Vec<String> {
    let names: Vec<String> = (0..layers.len())
        .map(|index| format!("{index}.tar"))
        .collect();
    let (list, config) = (list(layers.len()), described(layers));
    let mut members = vec![
        ("manifest.json", File(list.as_bytes())),
        ("config.json", File(config.as_bytes())),
    ];
    members.extend(
        names
            .iter()
            .zip(layers)
            .map(|(name, layer)| (name.as_str(), File(layer))),
    );
    let image = saved(&members);
    let came = materialize_layers(&image.archive, config.as_bytes(), &image.output, &REVIEWED);
    came.expect("an image");
    let tree = super::listing(&image.output.join("rootfs"));
    let fields = tree.fields().expect("a listing");
    fields.iter().map(|(name, _)| name.clone()).collect()
}

fn two() -> Vec<Vec<u8>> {
    vec![
        packed(&[
            ("etc", Directory),
            ("etc/a", File(b"first")),
            ("etc/b", File(b"second")),
        ]),
        packed(&[("etc/c", File(b"third")), ("etc/.wh.a", File(b""))]),
    ]
}

#[test]
fn a_number_of_a_header_is_read_as_the_reference_reads_it() {
    // What the reference's library made of each field.
    let table: [(&[u8], Option<i128>); 43] = [
        (b"0000644\x00", Some(420)),
        (b"644\x00\x00\x00\x00\x00", Some(420)),
        (b"\x00\x00\x00\x00\x00\x00\x00\x00", Some(0)),
        (b"        ", Some(0)),
        (b" 17 \x00\x00\x00\x00", Some(15)),
        (b"17 \x00 9\x00\x00", Some(15)),
        (b"0o17\x00\x00\x00\x00", Some(15)),
        (b"0O17\x00\x00\x00\x00", Some(15)),
        (b"0o_17\x00\x00\x00", Some(15)),
        (b"_17\x00\x00\x00\x00\x00", None),
        (b"1_7\x00\x00\x00\x00\x00", Some(15)),
        (b"1__7\x00\x00\x00\x00", None),
        (b"17_\x00\x00\x00\x00\x00", None),
        (b"+17\x00\x00\x00\x00\x00", Some(15)),
        (b"-17\x00\x00\x00\x00\x00", Some(-15)),
        (b"- 17\x00\x00\x00\x00", None),
        (b"+-17\x00\x00\x00\x00", None),
        (b"0o\x00\x00\x00\x00\x00\x00", None),
        (b"8\x00\x00\x00\x00\x00\x00\x00", None),
        (b"1 7\x00\x00\x00\x00\x00", None),
        (b"\x0917\x0a\x00\x00\x00\x00", Some(15)),
        (b"\x0b17\x0c\x00\x00\x00\x00", Some(15)),
        (b"\x1c17\x1f\x00\x00\x00\x00", Some(15)),
        (b"17\xc2\xa0\x00\x00\x00\x00", None),
        (b"\xc3\xa9\x00\x00\x00\x00\x00\x00", None),
        (b"0x17\x00\x00\x00\x00", None),
        (b"0b1\x00\x00\x00\x00\x00", None),
        (b"-0o17\x00\x00\x00", Some(-15)),
        (b"0o-17\x00\x00\x00", None),
        (b"\x80\x00\x00\x00\x00\x00\x01\x02", Some(258)),
        (b"\xff\xff\xff\xff\xff\xff\xff\xfe", Some(-2)),
        (
            b"\x80\xff\xff\xff\xff\xff\xff\xff",
            Some(72_057_594_037_927_935),
        ),
        (
            b"\xff\x00\x00\x00\x00\x00\x00\x00",
            Some(-72_057_594_037_927_936),
        ),
        (b"\x81\x00\x00\x00\x00\x00\x00\x01", None),
        (b"17", Some(15)),
        (b"-0\x00", Some(0)),
        (b"+\x00", None),
        (b"-\x00", None),
        (b"_\x00", None),
        (b"0o_\x00", None),
        (b"0_7\x00", Some(7)),
        (b"00o7\x00", None),
        (b"1\x857\x00", None),
    ];
    for (field, said) in table {
        assert_eq!(number(field), said, "{field:?}");
    }
}

#[test]
fn raw_headers_are_held_to_the_bounds_of_members_and_of_sizes() {
    let archive = packed(&[("a", File(b"12345")), ("d", Directory), ("b", File(b""))]);
    let walked = |outer, limits: Limits| {
        let came = physical_headers(&mut Cursor::new(&archive), outer, &limits);
        came.map_err(|fault| fault.0)
    };
    let fitted = Limits {
        members: 3,
        file: 5,
        ..REVIEWED
    };
    assert_eq!(walked(false, fitted), Ok(()));
    assert_eq!(
        walked(
            false,
            Limits {
                members: 2,
                ..fitted
            }
        ),
        Err("image_tar_header")
    );
    assert_eq!(
        walked(false, Limits { file: 4, ..fitted }),
        Err("image_tar_body_size")
    );
    // A file of the saved image itself is a layer: the bound is the image's.
    let outer = Limits {
        file: 0,
        archive: 5,
        ..REVIEWED
    };
    assert_eq!(walked(true, outer), Ok(()));
    assert_eq!(
        walked(
            true,
            Limits {
                archive: 4,
                ..outer
            }
        ),
        Err("image_tar_body_size")
    );
    assert_eq!(
        walked(
            false,
            Limits {
                archive: 0,
                ..fitted
            }
        ),
        Ok(())
    );
    // A size below nothing.
    let mut below = archive.clone();
    below[124..136].copy_from_slice(b"-0000000001\0");
    sealed(&mut below[..512]);
    let came = physical_headers(&mut Cursor::new(&below), false, &REVIEWED);
    assert_eq!(came.map_err(|fault| fault.0), Err("image_tar_body_size"));
}

#[test]
fn a_raw_header_may_have_its_check_summed_with_signed_bytes() {
    // The reference's library took either sum.
    let mut archive = packed(&[("caf\u{e9}", File(b""))]);
    let unsigned = archive[148..156].to_vec();
    archive[148..156].fill(b' ');
    let sum: i32 = archive[..512]
        .iter()
        .map(|byte| i32::from(*byte as i8))
        .sum();
    archive[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    assert_ne!(archive[148..156], unsigned[..]);
    let came = physical_headers(&mut Cursor::new(&archive), false, &REVIEWED);
    assert_eq!(came.map_err(|fault| fault.0), Ok(()));
}

#[test]
fn every_field_of_a_raw_header_must_be_read() {
    // Each of these the reference's library refused; what lies between
    // and after the fields it never read.
    let archive = packed(&[("a", File(b""))]);
    let walked = |at: usize, fill: &[u8]| {
        let mut changed = archive.clone();
        changed[at..at + fill.len()].copy_from_slice(fill);
        sealed(&mut changed[..512]);
        let came = physical_headers(&mut Cursor::new(&changed), false, &REVIEWED);
        came.map_err(|fault| fault.0)
    };
    for number in [100, 108, 116, 124, 136, 329, 337] {
        assert_eq!(walked(number, b"zz"), Err("image_tar_header"), "{number}");
    }
    for text in [0, 157, 265, 297, 345] {
        assert_eq!(walked(text, b"\xff"), Err("image_tar_header"), "{text}");
    }
    assert_eq!(walked(257, b"\xff\xff"), Ok(()));
    assert_eq!(walked(500, b"\xff"), Ok(()));
}

/// What reading the layer of `members` comes to: the names it holds.
fn read(members: &[(&str, Member<'_>)], limits: Limits) -> Result<Vec<String>, &'static str> {
    let scratch = Scratch::new("image-direct-read");
    let (path, kept) = (scratch.0.join("layer.tar"), scratch.0.join("kept"));
    std::fs::write(&path, packed(members)).expect("a layer");
    std::fs::create_dir(&kept).expect("a directory");
    let came = read_layer(&path, &kept, &limits);
    let came = came.map(|layer| layer.entries.into_keys().collect());
    came.map_err(|fault| fault.0)
}

#[test]
fn a_path_and_a_target_are_measured_in_characters() {
    let limits = Limits {
        path: 3,
        ..REVIEWED
    };
    let fitted = "\u{e9}".repeat(3);
    let longer = "\u{e9}".repeat(4);
    assert_eq!(canonical(&fitted, false, &limits), Ok(&fitted[..]));
    let came = canonical(&longer, false, &limits);
    assert_eq!(came.map_err(|fault| fault.0), Err("image_member_path"));
    assert_eq!(
        read(&[("l", Symlink(&fitted))], limits),
        Ok(vec!["l".to_owned()])
    );
    assert_eq!(
        read(&[("l", Symlink(&longer))], limits),
        Err("image_link_shape")
    );
}

#[test]
fn a_long_name_or_target_that_is_no_text_is_refused() {
    let long = [
        ("././@LongLink", Other(b'L', b"caf\xff\0")),
        ("short", File(b"")),
    ];
    assert_eq!(read(&long, REVIEWED), Err("image_member_path"));
    let target = [
        ("././@LongLink", Other(b'K', b"caf\xff\0")),
        ("l", Symlink("short")),
    ];
    assert_eq!(read(&target, REVIEWED), Err("image_link_target"));
    // The same headers with text.
    let long = [
        ("././@LongLink", Other(b'L', b"caf\xc3\xa9\0")),
        ("short", File(b"")),
    ];
    assert_eq!(read(&long, REVIEWED), Ok(vec!["caf\u{e9}".to_owned()]));
    let target = [
        ("././@LongLink", Other(b'K', b"caf\xc3\xa9\0")),
        ("l", Symlink("short")),
    ];
    assert_eq!(read(&target, REVIEWED), Ok(vec!["l".to_owned()]));
}

#[test]
fn a_size_from_an_extended_header_is_held_to_the_bound_of_a_file() {
    let limits = Limits {
        file: 5,
        ..REVIEWED
    };
    let fitted = [("x", Other(b'x', b"9 size=5\n")), ("f", File(b"12345"))];
    assert_eq!(read(&fitted, limits), Ok(vec!["f".to_owned()]));
    let larger = [("x", Other(b'x', b"9 size=6\n")), ("f", File(b"12345"))];
    assert_eq!(read(&larger, limits), Err("image_regular_shape"));
}

#[test]
fn the_root_is_no_member_by_either_of_its_names() {
    let layer = [(".", Directory), ("./", Directory), ("a", File(b""))];
    assert_eq!(read(&layer, REVIEWED), Ok(vec!["a".to_owned()]));
}

#[test]
fn a_target_may_hold_a_space_and_no_control_character() {
    assert_eq!(
        read(&[("l", Symlink("a b"))], REVIEWED),
        Ok(vec!["l".to_owned()])
    );
    assert_eq!(
        read(&[("l", Symlink("a\u{1f}b"))], REVIEWED),
        Err("image_link_target")
    );
}

#[test]
fn a_global_extended_header_may_not_change_what_follows_it() {
    for record in [&b"12 path=new\n"[..], b"16 linkpath=new\n", b"9 size=5\n"] {
        let layer = [("g", Other(b'g', record)), ("l", Symlink("old"))];
        assert_eq!(
            read(&layer, REVIEWED),
            Err("image_global_header"),
            "{record:?}"
        );
    }
    let sparse = [
        ("g", Other(b'g', b"22 GNU.sparse.major=1\n")),
        ("f", File(b"")),
    ];
    assert_eq!(read(&sparse, REVIEWED), Err("image_global_header"));
    let comment = [
        ("g", Other(b'g', b"19 comment=abcdefg\n")),
        ("f", File(b"")),
    ];
    assert_eq!(read(&comment, REVIEWED), Ok(vec!["f".to_owned()]));
    let broken = [("g", Other(b'g', b"no record\n")), ("f", File(b""))];
    assert_eq!(read(&broken, REVIEWED), Err("image_tar_header"));
}

#[test]
fn a_layer_of_the_saved_image_is_held_to_the_bound_of_the_image_not_of_a_file() {
    let all = super::cases("images");
    let case = all
        .iter()
        .find(|case| super::text(case, "name") == "one layer");
    let case = case.expect("a case");
    let scratch = Scratch::new("image-direct-outer");
    let (archive, manifest) = (scratch.0.join("image.tar"), scratch.0.join("release.json"));
    let image = super::inflated(case.get("image").expect("an image"));
    std::fs::write(&archive, &image).expect("an image");
    std::fs::write(&manifest, super::text(case, "manifest")).expect("a manifest");
    // No file of the layer has as many bytes as the layer itself.
    let limits = Limits {
        file: 1024,
        ..REVIEWED
    };
    assert!(image.len() > 4096);
    let came = extract(&archive, &manifest, &scratch.0.join("evidence"), &limits);
    assert_eq!(came.map(|_| ()).map_err(|fault| fault.0), Ok(()));
    let limits = Limits {
        file: 0,
        ..REVIEWED
    };
    let came = extract(&archive, &manifest, &scratch.0.join("other"), &limits);
    assert_eq!(
        came.map(|_| ()).map_err(|fault| fault.0),
        Err("image_tar_body_size")
    );
}

#[test]
fn a_layer_file_is_held_to_the_bound_of_what_it_unpacks_to() {
    let scratch = Scratch::new("image-direct-layer");
    let layer = packed(&[("a", File(b"body"))]);
    let mut packer = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    packer.write_all(&layer).expect("a layer");
    let gzip = packer.finish().expect("a packed layer");
    let expected = format!("sha256:{}", digest(&layer));
    let size = layer.len() as u64;
    for (index, (source, most, said)) in [
        (&layer, size, Ok(size)),
        (&layer, size - 1, Err("image_layer_expansion")),
        (&gzip, size, Ok(size)),
        (&gzip, size - 1, Err("image_layer_expansion")),
    ]
    .into_iter()
    .enumerate()
    {
        let limits = Limits {
            layer: most,
            ..REVIEWED
        };
        let target = scratch.0.join(format!("layer-{index}.tar"));
        let came = layer_file(&source[..], &target, &expected, "0.tar", &limits);
        assert_eq!(came.map_err(|fault| fault.0), said, "{index}");
    }
    // Only a whole digest in a blob's name is held to what is stored.
    let target = scratch.0.join("short.tar");
    let came = layer_file(
        &layer[..],
        &target,
        &expected,
        "blobs/sha256/abc",
        &REVIEWED,
    );
    assert_eq!(came.map_err(|fault| fault.0), Ok(size));
}

#[test]
fn a_new_file_is_made_only_where_nothing_is() {
    let scratch = Scratch::new("image-direct-new");
    let path = scratch.0.join("report.json");
    assert_eq!(new_file(&path, b"first"), Ok(()));
    assert_eq!(
        new_file(&path, b"second").map_err(|fault| fault.0),
        Err("image_filesystem_error")
    );
    assert_eq!(std::fs::read(&path).expect("a file"), b"first");
    // A link is not followed, not even to where nothing is yet.
    let link = scratch.0.join("link");
    std::os::unix::fs::symlink("elsewhere", &link).expect("a link");
    assert_eq!(
        new_file(&link, b"third").map_err(|fault| fault.0),
        Err("image_filesystem_error")
    );
    assert!(!scratch.0.join("elsewhere").exists());
}

#[test]
fn layers_are_held_to_their_number_and_to_their_configuration() {
    let layers = two();
    let report = unpacked(&layers, None, None, REVIEWED).expect("a report");
    assert!(report.contains("\"members\": 5,"), "{report}");
    let fitted = Limits {
        layers: 2,
        ..REVIEWED
    };
    assert_eq!(unpacked(&layers, None, None, fitted).as_ref(), Ok(&report));
    let fewer = Limits {
        layers: 1,
        ..REVIEWED
    };
    assert_eq!(
        unpacked(&layers, None, None, fewer),
        Err("image_layer_count")
    );
    assert_eq!(
        unpacked(&[], None, None, REVIEWED),
        Err("image_layer_count")
    );
    // The configuration is held byte for byte.
    let usual = described(&layers);
    let spaced = format!("{usual} ");
    assert_eq!(
        unpacked(&layers, None, Some(spaced.as_bytes()), REVIEWED),
        Err("image_config_changed")
    );
    let one = list(1);
    assert_eq!(
        unpacked(&layers, Some((&one, usual.as_bytes())), None, REVIEWED),
        Err("image_layer_count_mismatch")
    );
    let other = "[{\"Config\":\"config.json\",\"Layers\":[\"0.tar\",\"9.tar\"]}]";
    assert_eq!(
        unpacked(&layers, Some((other, usual.as_bytes())), None, REVIEWED),
        Err("image_layer_missing")
    );
    let directory = "[{\"Config\":\"config.json\",\"Layers\":[\"0.tar\",\"etc\"]}]";
    let image = saved(&[
        ("manifest.json", File(directory.as_bytes())),
        ("config.json", File(usual.as_bytes())),
        ("0.tar", File(&layers[0])),
        ("etc/", Directory),
    ]);
    let came = materialize_layers(&image.archive, usual.as_bytes(), &image.output, &REVIEWED);
    assert_eq!(came.map_err(|fault| fault.0), Err("image_layer_missing"));
}

#[test]
fn a_digest_of_a_layer_must_be_spelled_in_small_letters_whole() {
    let layers = two();
    let usual = described(&layers);
    let name = digest(&layers[1]);
    for other in [
        name.to_uppercase(),
        format!("{name}0"),
        name[1..].to_owned(),
        format!("{}g", &name[1..]),
    ] {
        let changed = usual.replace(&name, &other);
        let stored = Some((&list(2)[..], changed.as_bytes()));
        let came = unpacked(&layers, stored, Some(changed.as_bytes()), REVIEWED);
        assert_eq!(came, Err("image_diff_id_shape"), "{other}");
    }
    let unmarked = usual.replace(&format!("sha256:{name}"), &format!("sha512:{name}"));
    let stored = Some((&list(2)[..], unmarked.as_bytes()));
    let came = unpacked(&layers, stored, Some(unmarked.as_bytes()), REVIEWED);
    assert_eq!(came, Err("image_diff_id_shape"));
}

#[test]
fn every_layer_together_is_held_to_the_bounds_of_bytes_and_of_members() {
    let layers = two();
    let bytes = (layers[0].len() + layers[1].len()) as u64;
    let fitted = Limits {
        expanded: bytes,
        members: 5,
        ..REVIEWED
    };
    assert!(unpacked(&layers, None, None, fitted).is_ok());
    let came = unpacked(
        &layers,
        None,
        None,
        Limits {
            expanded: bytes - 1,
            ..fitted
        },
    );
    assert_eq!(came, Err("image_total_expansion"));
    let came = unpacked(
        &layers,
        None,
        None,
        Limits {
            members: 4,
            ..fitted
        },
    );
    assert_eq!(came, Err("image_total_members"));
}

#[test]
fn a_removed_path_takes_what_is_below_it_and_nothing_beside_it() {
    let lower = packed(&[
        ("d", Directory),
        ("d/a", File(b"a")),
        ("d!x", File(b"x")),
        ("d-x", File(b"x")),
        ("d.x", File(b"x")),
        ("d0", File(b"x")),
    ]);
    let upper = packed(&[(".wh.d", File(b""))]);
    assert_eq!(built(&[lower, upper]), ["d!x", "d-x", "d.x", "d0"]);
}

#[test]
fn an_opaque_directory_stays_itself_and_a_whiteout_is_no_path_of_the_image() {
    let lower = packed(&[("d", Directory), ("d/a", File(b"a")), ("e/b", File(b"b"))]);
    let opaque = packed(&[("d/.wh..wh..opq", File(b""))]);
    assert_eq!(built(&[lower.clone(), opaque]), ["d", "e", "e/b"]);
    // A directory that was never listed is there only for what is below it.
    let removed = packed(&[("e/.wh.b", File(b""))]);
    assert_eq!(built(&[lower, removed]), ["d", "d/a"]);
    // A listed directory is made although nothing is below it.
    assert_eq!(built(&[packed(&[("empty", Directory)])]), ["empty"]);
}

#[test]
fn every_directory_above_a_path_must_be_a_directory() {
    let lower = packed(&[("d", File(b"a file"))]);
    let upper = packed(&[("d/x/y", File(b"y"))]);
    let came = unpacked(&[lower, upper], None, None, REVIEWED);
    assert_eq!(came, Err("image_parent_not_directory"));
}

#[test]
fn the_first_image_of_the_list_is_the_one_that_is_unpacked() {
    let layers = two();
    let usual = described(&layers);
    let both = format!(
        "[{},{{\"Config\":\"other.json\",\"Layers\":[]}}]",
        list(2).trim_start_matches('[').trim_end_matches(']')
    );
    let came = unpacked(&layers, Some((&both, usual.as_bytes())), None, REVIEWED);
    assert_eq!(came, unpacked(&layers, None, None, REVIEWED));
    assert!(came.is_ok());
}

#[test]
fn a_chain_of_hard_links_is_held_to_its_bound() {
    // The first name is resolved first, through every link after it.
    let chain = |links: usize| {
        let names: Vec<String> = (0..=links).map(|index| format!("l{index:02}")).collect();
        let mut members = Vec::new();
        for pair in names.windows(2) {
            members.push((pair[0].as_str(), Hardlink(&pair[1])));
        }
        members.push((names[links].as_str(), File(b"kept")));
        vec![packed(&members)]
    };
    let limits = Limits {
        links: 3,
        ..REVIEWED
    };
    assert!(unpacked(&chain(3), None, None, limits).is_ok());
    assert_eq!(
        unpacked(&chain(4), None, None, limits),
        Err("image_hardlink_cycle")
    );
}

#[test]
fn what_the_saved_image_says_of_itself_must_be_there_and_be_read() {
    let layers = two();
    let usual = described(&layers);
    let stored =
        |list: &'static str| unpacked(&two(), Some((list, usual.as_bytes())), None, REVIEWED);
    for list in [
        "{",
        "{}",
        "[]",
        "[7]",
        "[{\"Layers\":[]}]",
        "[{\"Config\":7,\"Layers\":[]}]",
    ] {
        assert_eq!(stored(list), Err("image_metadata_invalid"), "{list}");
    }
    for list in [
        "[{\"Config\":\"config.json\"}]",
        "[{\"Config\":\"config.json\",\"Layers\":[7, 8]}]",
    ] {
        assert_eq!(stored(list), Err("image_metadata_invalid"), "{list}");
    }
    assert_eq!(stored(""), Err("image_metadata_missing"));
    assert_eq!(
        stored("[{\"Config\":\"other.json\",\"Layers\":[]}]"),
        Err("image_metadata_missing")
    );
    // The configuration that is handed in.
    for given in [
        "{",
        "[]",
        "{}",
        "{\"rootfs\":{}}",
        "{\"rootfs\":{\"diff_ids\":{}}}",
    ] {
        let came = unpacked(&layers, None, Some(given.as_bytes()), REVIEWED);
        assert_eq!(came, Err("image_metadata_invalid"), "{given}");
    }
    let numbered = "{\"rootfs\":{\"diff_ids\":[7,8]}}";
    let both = Some((&list(2)[..], numbered.as_bytes()));
    let came = unpacked(&layers, both, Some(numbered.as_bytes()), REVIEWED);
    assert_eq!(came, Err("image_metadata_invalid"));
    // A configuration of the largest size, and one byte more.
    let most = 1024 * 1024;
    for (size, said) in [(most, None), (most + 1, Some("image_metadata_missing"))] {
        let padded = format!("{usual}{}", " ".repeat(size - usual.len()));
        let both = Some((&list(2)[..], padded.as_bytes()));
        let came = unpacked(&layers, both, Some(padded.as_bytes()), REVIEWED);
        assert_eq!(came.err(), said, "{size}");
    }
}

#[test]
fn the_saved_image_has_only_files_and_directories_each_named_once_and_plainly() {
    let layers = two();
    let usual = described(&layers);
    let one = list(1);
    let image = |more: &[(&str, Member<'_>)], limits: Limits| {
        let mut members = vec![
            ("manifest.json", File(one.as_bytes())),
            ("config.json", File(usual.as_bytes())),
            ("0.tar", File(&layers[0])),
        ];
        for (name, member) in more {
            members.push((
                name,
                match member {
                    File(body) => File(body),
                    Directory => Directory,
                    Symlink(target) => Symlink(target),
                    Hardlink(target) => Hardlink(target),
                    Other(kind, body) => Other(*kind, body),
                },
            ));
        }
        let image = saved(&members);
        image_identity(&image.archive, &limits).map_err(|fault| fault.0)
    };
    let identity = format!("sha256:{}", digest(usual.as_bytes()));
    assert_eq!(
        image(&[("blobs/", Directory)], REVIEWED),
        Ok(identity.clone())
    );
    let fitted = Limits {
        outer: 4,
        ..REVIEWED
    };
    assert_eq!(image(&[("blobs", Directory)], fitted), Ok(identity));
    let fewer = Limits {
        outer: 3,
        ..REVIEWED
    };
    assert_eq!(
        image(&[("blobs", Directory)], fewer),
        Err("image_outer_member")
    );
    for more in [
        ("0.tar", File(b"again")),
        ("config.json/", Directory),
        ("link", Symlink("0.tar")),
        ("link", Hardlink("0.tar")),
        ("./1.tar", File(b"")),
        ("a//b", File(b"")),
        ("a/../b", File(b"")),
        ("/a", File(b"")),
        ("a\\b", File(b"")),
        ("a\tb", File(b"")),
    ] {
        let name = more.0.to_owned();
        assert_eq!(
            image(&[more], REVIEWED),
            Err("image_outer_member"),
            "{name:?}"
        );
    }
}

#[test]
fn a_member_of_the_saved_image_whose_name_is_no_text_is_refused() {
    let scratch = Scratch::new("image-direct-name");
    let archive = scratch.0.join("image.tar");
    let mut data = packed(&[("manifest.json", File(b"[]")), ("zz", File(b""))]);
    // The last header is before the two blocks of the end.
    let at = data.len() - 3 * 512;
    data[at] = 0xff;
    sealed(&mut data[at..at + 512]);
    std::fs::write(&archive, &data).expect("an image");
    let came = image_identity(&archive, &REVIEWED);
    assert_eq!(came.map_err(|fault| fault.0), Err("image_outer_member"));
}

#[test]
fn an_identity_is_that_of_the_one_image_of_the_archive() {
    let config = b"{\"rootfs\":{}}";
    let identity = |list: &str| {
        let image = saved(&[
            ("manifest.json", File(list.as_bytes())),
            ("c.json", File(config)),
        ]);
        image_identity(&image.archive, &REVIEWED).map_err(|fault| fault.0)
    };
    let one = "{\"Config\":\"c.json\",\"Layers\":[]}";
    assert_eq!(
        identity(&format!("[{one}]")),
        Ok(format!("sha256:{}", digest(config)))
    );
    assert_eq!(
        identity(&format!("[{one},{one}]")),
        Err("image_config_selection")
    );
    assert_eq!(identity("[]"), Err("image_metadata_invalid"));
}

#[test]
fn a_saved_image_is_held_to_the_bound_of_its_size() {
    let scratch = Scratch::new("image-direct-size");
    let archive = scratch.0.join("image.tar");
    let data = packed(&[("manifest.json", File(b"[]"))]);
    std::fs::write(&archive, &data).expect("an image");
    let (manifest, output) = (scratch.0.join("release.json"), scratch.0.join("evidence"));
    let came = |most: u64| {
        let limits = Limits {
            archive: most,
            ..REVIEWED
        };
        let came = extract(&archive, &manifest, &output, &limits);
        came.map(|_| ()).map_err(|fault| fault.0)
    };
    // Within its bound the image is walked, and then its manifest is wanted.
    assert_eq!(came(data.len() as u64), Err("release_manifest_invalid"));
    assert_eq!(came(data.len() as u64 - 1), Err("image_archive_file"));
    assert!(!Path::new(&output).exists());
}
