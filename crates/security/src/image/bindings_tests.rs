//! Replays of `recorded_bindings.json`: what the reference in Python made
//! of how a path is spelled, of the binding of the external AWS-LC and of
//! OpenSSL to the reviewed native record, and of the binding of the
//! binary's shared libraries to the image's rpms.

use super::{aws_lc_build_binding, last_step, openssl_build_binding, runtime_rpm_bindings};
use crate::{
    document::Document,
    image::{
        canonical,
        notices::tests::built,
        policy::tests::{There, tallied},
        secrets::tests::{here, text, there},
    },
    scratch::Scratch,
};

const RECORDING: &[u8] = include_bytes!("recorded_bindings.json");

fn cases(name: &str) -> Vec<Document> {
    let recording = Document::read(RECORDING).expect("a recording");
    let held = recording
        .get(name)
        .and_then(Document::items)
        .map(<[Document]>::to_vec);
    held.unwrap_or_else(|| panic!("no cases {name}"))
}

fn awslcs() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("awslcs") {
        let native = case.get("native").expect("a native record");
        let expected = case.get("expected").expect("a review");
        let came = aws_lc_build_binding(native, expected).map(|()| Document::Null);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn openssls() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("openssls") {
        let native = case.get("native").expect("a native record");
        let expected = case.get("expected").expect("a review");
        let came = openssl_build_binding(native, expected).map(|()| Document::Null);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn runtimes() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("runtimes") {
        let scratch = Scratch::new("bindings");
        let rootfs = built(&case, &scratch.0);
        let packages = case
            .get("packages")
            .and_then(Document::items)
            .expect("packages");
        let elf = case.get("elf").expect("binary evidence");
        let came = runtime_rpm_bindings(packages, elf, &rootfs).map(Document::List);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

#[test]
fn every_recorded_path_is_read_alike() {
    let cases = cases("paths");
    for case in &cases {
        let path = text(case, "path");
        let reading = case.get("canonical").expect("a reading");
        assert_eq!(&Document::Bool(canonical(path)), reading, "{path:?}");
        assert_eq!(last_step(path), text(case, "last"), "{path:?}");
    }
}

/// The recorded OpenSSL build with each of its two static libraries linked
/// `count` times, by both providers in turn, each time from a directory of
/// its own.
fn linked(count: usize) -> crate::Result<()> {
    let case = cases("openssls").into_iter();
    let mut case = case.filter(|case| text(case, "name") == "the OpenSSL build");
    let case = case.next().expect("the recorded build");
    let mut native = case.get("native").expect("a native record").clone();
    let build = native
        .get("openssl")
        .and_then(|openssl| openssl.get("build"));
    let libraries = build.and_then(|build| build.get("static_libraries"));
    let libraries = libraries
        .and_then(Document::items)
        .expect("libraries")
        .to_vec();
    let mut archives = Vec::new();
    for library in &libraries {
        let field = |name: &str| library.get(name).expect("a field").clone();
        for index in 0..count {
            archives.push(Document::record([
                ("library", field("library")),
                ("path", field("path")),
                ("sha256", field("sha256")),
                ("size", field("size")),
                (
                    "provider",
                    ["mediasoup-sys", "openssl-sys"][index % 2].into(),
                ),
                ("out_dir", format!("/app/out/{}", index / 2).into()),
            ]));
        }
    }
    let Document::Record(fields) = &mut native else {
        panic!("a native record");
    };
    let slot = fields
        .iter_mut()
        .find(|(name, _)| name == "static_archives");
    slot.expect("the linked archives").1 = Document::List(archives);
    openssl_build_binding(&native, case.get("expected").expect("a review"))
}

#[test]
fn each_library_of_openssl_is_linked_at_most_twenty_thousand_times() {
    assert_eq!(linked(2), Ok(()));
    assert_eq!(linked(20_000), Ok(()));
    let refused = Err(crate::Fault("image_openssl_archive_binding"));
    assert_eq!(linked(20_001), refused);
}

/// Builds of AWS-LC and reviews of it that the reference failed on in its
/// interpreter's words: a field that is missing or of another kind.
const UNREAD_AWSLCS: [(&str, &str); 8] = [
    ("an AWS-LC archive without its path", "image_binding_shape"),
    ("a review that names no version", "image_binding_shape"),
    ("a review without its system", "image_binding_shape"),
    ("a review whose source is text", "image_binding_shape"),
    (
        "a review without the configure options of its system",
        "image_binding_shape",
    ),
    (
        "a build of a file digest that is a number",
        "image_binding_shape",
    ),
    (
        "a build of a bindgen digest that is a number",
        "image_binding_shape",
    ),
    (
        "a component without its bindings digest",
        "image_binding_shape",
    ),
];

/// A component that the reference took because its interpreter holds one
/// equal to true; a value is held here to the reviewed value of the same
/// kind.
const SAME_KIND: [(&str, &str); 1] = [(
    "a component named true where one is expected",
    "image_awslc_source_binding",
)];

/// Builds of OpenSSL that the reference failed on in its interpreter's
/// words.
const UNREAD_OPENSSLS: [(&str, &str); 7] = [
    (
        "a linked archive without its directory",
        "image_binding_shape",
    ),
    (
        "a linked archive without its library",
        "image_binding_shape",
    ),
    ("a version header that is no record", "image_binding_shape"),
    ("a build whose libraries are no list", "image_binding_shape"),
    ("a linked archive without its size", "image_binding_shape"),
    ("a linked archive without its digest", "image_binding_shape"),
    (
        "a component and a review without configure options",
        "image_binding_shape",
    ),
];

/// A header that is the same record as a library: the reference took it
/// for that library and refused the headers' prefix; here a header is a
/// header, and one with a library's fields is refused as an artifact.
const OTHERWISE_OPENSSLS: [(&str, &str); 1] = [(
    "a version header that is the record of the ssl archive",
    "image_openssl_artifact_identity",
)];

/// Libraries and rpms that the reference failed on in its interpreter's
/// words: a missing field, and a file mode that its interpreter could not
/// hold, which is refused here as no mode of a regular file.
const UNREAD_RUNTIMES: [(&str, &str); 6] = [
    (
        "a library recorded with mode 98797",
        "image_runtime_rpm_digest",
    ),
    (
        "a library recorded with mode -1",
        "image_runtime_rpm_digest",
    ),
    (
        "a library recorded with mode -1015315",
        "image_runtime_rpm_digest",
    ),
    ("an rpm without its type", "image_binding_shape"),
    (
        "a library recorded without its digest",
        "image_binding_shape",
    ),
    ("an rpm without its source package", "image_binding_shape"),
];

#[test]
fn every_recorded_build_of_aws_lc_is_bound_alike() {
    let tables = [
        (&UNREAD_AWSLCS[..], There::Raised),
        (&SAME_KIND[..], There::Held),
    ];
    assert_eq!(tallied(awslcs(), &tables), [6, 35, 8, 1, 0]);
}

#[test]
fn every_recorded_build_of_openssl_is_bound_alike() {
    let tables = [
        (&UNREAD_OPENSSLS[..], There::Raised),
        (&OTHERWISE_OPENSSLS[..], There::Refused),
    ];
    assert_eq!(tallied(openssls(), &tables), [2, 62, 7, 0, 1]);
}

#[test]
fn every_recorded_runtime_library_is_bound_alike() {
    let tables = [(&UNREAD_RUNTIMES[..], There::Raised)];
    assert_eq!(tallied(runtimes(), &tables), [4, 35, 6, 0, 0]);
}

/// What the reference's binding made of each receipt that the native
/// producer wrote in its own tests: three instances of OpenSSL, the
/// external AWS-LC, and each with what the tests changed in it.
const RECEIPTS: &[u8] = include_bytes!("recorded_receipts.json");

/// The recorded `name` receipts, each as bound here and as the reference
/// bound it.
fn receipts(
    name: &str,
    bound: fn(&Document, &Document) -> crate::Result<()>,
) -> Vec<(String, String, String)> {
    let recording = Document::read(RECEIPTS).expect("a recording");
    let held = recording.get(name).and_then(Document::items);
    let held = held.unwrap_or_else(|| panic!("no receipts {name}"));
    let receipt = |case: &Document| {
        let native = case.get("native").expect("a native record");
        let expected = case.get("expected").expect("a review");
        let came = bound(native, expected).map(|()| Document::Null);
        (text(case, "name").to_owned(), here(came), there(case))
    };
    held.iter().map(receipt).collect()
}

#[test]
fn every_receipt_of_the_native_producer_is_bound_alike() {
    let awslcs = receipts("awslcs", aws_lc_build_binding);
    assert_eq!(tallied(awslcs, &[]), [1, 5, 0, 0, 0]);
    let openssls = receipts("openssls", openssl_build_binding);
    assert_eq!(tallied(openssls, &[]), [1, 8, 0, 0, 0]);
}
