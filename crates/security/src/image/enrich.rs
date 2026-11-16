//! The inventory of the image, completed with what the scanner cannot see:
//! the licences of the binary's Rust crates, joined from the authenticated
//! build's own records of them, and the native inputs that were linked
//! into it statically. Nothing that the scanner observed is dropped; the
//! image's configuration and manifest, whose raw bytes may hold secrets of
//! its history, are, and their digests stay.

use super::policy::{self, Limits};
use crate::{Fault, Result, digest, document::Document, escaped, require};
use std::collections::BTreeSet;

/// An inventory, a native record or binary metadata that lacks a field or
/// has one of another kind.
const SHAPE: Fault = Fault("image_evidence_shape");
/// The declarations of Cargo that registry archives of the lock hold, and
/// the expressions they mean.
const CARGO_ALTERNATIVES: [(&str, &str); 4] = [
    ("MIT/Apache-2.0", "MIT OR Apache-2.0"),
    ("Apache-2.0/MIT", "Apache-2.0 OR MIT"),
    ("Apache-2.0 / MIT", "Apache-2.0 OR MIT"),
    ("Unlicense/MIT", "Unlicense OR MIT"),
];
/// The fields of the build's record of the binary's dependency metadata.
const METADATA: [&str; 4] = ["format", "sha256", "compressedSha256", "packageCount"];
/// Who adds the native inputs to the inventory.
const NATIVE: &str = "simplestchat-authenticated-native-inputs";

fn record(value: Option<&Document>) -> Result<&Document> {
    value.filter(|value| value.fields().is_some()).ok_or(SHAPE)
}

fn records(value: Option<&Document>) -> Result<&[Document]> {
    let items = value.and_then(Document::items).ok_or(SHAPE)?;
    require(items.iter().all(|item| item.fields().is_some()), SHAPE.0)?;
    Ok(items)
}

fn text<'a>(value: &'a Document, name: &str) -> Result<&'a str> {
    value.get(name).and_then(Document::text).ok_or(SHAPE)
}

/// The text field `name` of `item`, if it is there and text.
fn word<'a>(item: &'a Document, name: &str) -> Option<&'a str> {
    item.get(name).and_then(Document::text)
}

/// Whether `item` is the package `name` of `version`; an item that lacks
/// either field is no record of a package.
fn named(item: &Document, name: &str, version: &str) -> Result<bool> {
    let held = item.get("name").ok_or(SHAPE)?;
    let release = item.get("version").ok_or(SHAPE)?;
    Ok(held.text() == Some(name) && release.text() == Some(version))
}

/// Give every Rust crate of `packages` the licence that the build's record
/// of it declares: its name and version must be those of exactly one crate
/// of the binary's own metadata, which the build's record of that metadata
/// describes, and of exactly one licence record from that crate's source.
/// Every crate that the binary runs must be among them.
fn rust_license_join(packages: &mut [Document], native: &Document, elf: &Document) -> Result<()> {
    let metadata = record(elf.get("auditable"))?;
    let declared = record(native.get("rust_dependency_metadata"))?;
    let mut bound = declared.names() == BTreeSet::from(METADATA);
    for (name, value) in declared.fields().unwrap_or_default() {
        bound = bound && metadata.get(name).ok_or(SHAPE)? == value;
    }
    require(bound, "image_rust_metadata_binding")?;
    let graph = records(metadata.get("packages"))?;
    let built = records(native.get("rust_licenses"))?;
    let mut runtime = BTreeSet::new();
    for item in graph {
        let kind = item.get("kind").map_or(Some("runtime"), Document::text);
        if kind == Some("runtime") {
            runtime.insert((
                text(item, "name")?.to_owned(),
                text(item, "version")?.to_owned(),
            ));
        }
    }
    let mut seen = BTreeSet::new();
    for package in packages.iter_mut() {
        if package.get("type") != Some(&"rust-crate".into()) {
            continue;
        }
        let identity = (
            text(package, "name")?.to_owned(),
            text(package, "version")?.to_owned(),
        );
        let (name, version) = (identity.0.as_str(), identity.1.as_str());
        let mut observed = Vec::new();
        for item in graph {
            if named(item, name, version)? {
                observed.push(item);
            }
        }
        let [observed] = observed[..] else {
            return Err(Fault("image_rust_binary_identity"));
        };
        let source = text(observed, "source")?;
        let reviewed = matches!(source, "local" | "crates.io");
        let mut licenses = Vec::new();
        for item in built {
            if named(item, name, version)?
                && reviewed
                && item.get("source").ok_or(SHAPE)?.text() == Some(source)
            {
                licenses.push(item);
            }
        }
        let [license] = licenses[..] else {
            return Err(Fault("image_rust_license_source"));
        };
        let coverage = record(license.get("evidence"))?;
        let covered = coverage.names() == BTreeSet::from(["compilerArtifact", "embeddedMetadata"])
            && matches!(coverage.get("compilerArtifact"), Some(Document::Bool(_)))
            && coverage.get("embeddedMetadata") == Some(&Document::Bool(true));
        require(covered, "image_rust_license_coverage")?;
        let declared = record(license.get("license"))?;
        let declared = match declared.get("expression").ok_or(SHAPE)? {
            Document::Null => None,
            other => Some(other.text().ok_or(SHAPE)?),
        };
        let meant = |declared: &str| {
            let found = CARGO_ALTERNATIVES.iter().find(|(raw, _)| *raw == declared);
            found
                .filter(|_| source == "crates.io")
                .map(|(_, meant)| *meant)
        };
        let mut expression = declared.map(|declared| meant(declared).unwrap_or(declared));
        let mut shown = declared;
        if license.get("first_party").ok_or(SHAPE)? == &Document::Bool(true) {
            let first = name == "simplestchat"
                && source == "local"
                && expression.is_none()
                && license.get("cargo_publish").ok_or(SHAPE)? == &Document::Bool(false);
            require(first, "image_first_party_license_identity")?;
            // What publication says, which grants no licence.
            (expression, shown) = (Some("NOASSERTION"), Some("NOASSERTION"));
        }
        let joined = expression.zip(shown).map(|(expression, shown)| {
            Document::record([
                ("value", shown.into()),
                ("spdxExpression", expression.into()),
                ("type", "declared".into()),
                ("urls", Document::List(Vec::new())),
                ("locations", Document::List(Vec::new())),
            ])
        });
        package.put("licenses", Document::List(joined.into_iter().collect()));
        seen.insert(identity);
    }
    require(runtime.is_subset(&seen), "image_rust_runtime_coverage")
}

/// Give the native `package` of the static C++ runtime the identity of the
/// Fedora rpm that `owner` names, by its name, its epoch, version, release
/// and machine, and its source package, so that advisories of rpms reach
/// it.
fn static_rpm(package: &mut Document, owner: [&str; 3]) -> Result<()> {
    const NEVRA: Fault = Fault("image_static_rpm_nevra");
    let [name, identity, source] = owner;
    let (epoch, nevra) = identity.split_once(':').ok_or(NEVRA)?;
    let (version_release, architecture) = nevra.rsplit_once('.').unwrap_or(("", nevra));
    let (version, release) = version_release.rsplit_once('-').ok_or(NEVRA)?;
    // The digits of ASCII, of a number that a machine word holds.
    let digits = epoch.bytes().all(|digit| digit.is_ascii_digit());
    let number: u64 = epoch.parse().ok().filter(|_| digits).ok_or(NEVRA)?;
    let shaped = !version.is_empty()
        && !release.is_empty()
        && matches!(architecture, "x86_64" | "aarch64")
        && source.ends_with(".src.rpm");
    require(shaped, NEVRA.0)?;
    let mut qualifiers = vec![
        ("arch", architecture),
        ("distro", "fedora-44"),
        ("upstream", source),
    ];
    if number != 0 {
        qualifiers.push(("epoch", epoch));
    }
    let query: Vec<String> = qualifiers
        .iter()
        .map(|(field, value)| format!("{field}={}", escaped(value, true)))
        .collect();
    let purl = format!(
        "pkg:rpm/fedora/{}@{}?{}",
        escaped(name, false),
        escaped(version_release, false),
        query.join("&")
    );
    package.put("type", "rpm".into());
    package.put("version", version_release.into());
    package.put("purl", purl.into());
    package.put("metadataType", "rpm-db-entry".into());
    let metadata = Document::record([
        ("name", name.into()),
        ("version", version.into()),
        ("epoch", number.into()),
        ("release", release.into()),
        ("architecture", architecture.into()),
        ("sourceRpm", source.into()),
        ("files", Document::List(Vec::new())),
    ]);
    package.put("metadata", metadata);
    Ok(())
}

/// The native inputs of the build's native record as packages of the
/// inventory: the wrapped components of production, the adapted one,
/// OpenSSL, the external AWS-LC and the static C++ runtime, which keeps the
/// identity of its rpm. Tests, inputs of other systems and the builder's
/// keys are no part of what runs.
fn native_packages(native: &Document) -> Result<Vec<Document>> {
    // As the reference read them: the records of the components first, the
    // owner of the static runtime next, and the fields of each one last.
    let mut components = Vec::new();
    for item in records(native.get("wrap_components"))? {
        if item.get("usage").ok_or(SHAPE)? == &Document::from("production") {
            components.push([
                word(item, "name"),
                word(item, "version"),
                word(item, "license"),
            ]);
        }
    }
    let adapted = record(native.get("adapted_component"))?;
    let openssl = record(native.get("openssl"))?;
    let registry = record(native.get("registry_component"))?;
    components.push([
        word(adapted, "name"),
        word(adapted, "version"),
        word(adapted, "license"),
    ]);
    // OpenSSL's record names no component, unless it names one itself.
    let named = openssl.get("name").map_or(Some("OpenSSL"), Document::text);
    components.push([named, word(openssl, "version"), word(openssl, "license")]);
    components.push([
        word(registry, "name"),
        word(registry, "version"),
        word(registry, "license"),
    ]);
    let toolchain = record(native.get("toolchain"))?;
    let owner: Vec<&str> = text(toolchain, "static_cxx_owner")?.split('\t').collect();
    let [package, release, source] = owner[..] else {
        return Err(Fault("image_static_cxx_identity"));
    };
    let license = toolchain.get("static_cxx_license").ok_or(SHAPE)?;
    components.push([Some(package), Some(release), license.text()]);
    let mut inputs = Vec::new();
    for [name, version, license] in components {
        let (name, version) = (name.ok_or(SHAPE)?, version.ok_or(SHAPE)?);
        let expression = license.ok_or(SHAPE)?;
        let identifier = digest(format!("{name}\0{version}").as_bytes());
        let location = Document::record([
            ("path", "/app/simplestChat".into()),
            ("accessPath", "/app/simplestChat".into()),
            (
                "annotations",
                Document::record([("evidence", "static-build-input".into())]),
            ),
        ]);
        let declared = Document::record([
            ("value", expression.into()),
            ("spdxExpression", expression.into()),
            ("type", "declared".into()),
            ("urls", Document::List(Vec::new())),
            ("locations", Document::List(Vec::new())),
        ]);
        inputs.push(Document::record([
            ("id", format!("native-{}", &identifier[..16]).into()),
            ("name", name.into()),
            ("version", version.into()),
            ("type", "binary".into()),
            ("foundBy", NATIVE.into()),
            ("locations", Document::List(vec![location])),
            ("licenses", Document::List(vec![declared])),
            ("language", "c++".into()),
            ("cpes", Document::List(Vec::new())),
            ("purl", "".into()),
            ("metadataType", "".into()),
            ("metadata", Document::Null),
        ]));
    }
    if let Some(runtime) = inputs.last_mut() {
        static_rpm(runtime, [package, release, source])?;
    }
    Ok(inputs)
}

/// The field `name` of a record, to change.
fn field<'a>(value: &'a mut Document, name: &str) -> Option<&'a mut Document> {
    match value {
        Document::Record(fields) => fields
            .iter_mut()
            .find_map(|(held, value)| (held == name).then_some(value)),
        _ => None,
    }
}

/// The scanner's inventory `value` of the image, held to the image's
/// `image_policy` and to the scanner release `syft` as
/// [`policy::inventory`] holds it, its Rust crates given their licences and
/// the build's native inputs added; the image's configuration and manifest
/// are left out of its source's metadata.
pub fn enrich_sbom(
    mut value: Document,
    native: &Document,
    elf: &Document,
    image_policy: &Document,
    syft: &str,
    limits: &Limits,
) -> Result<Document> {
    let mut packages = policy::inventory(&value, image_policy, syft, limits)?;
    rust_license_join(&mut packages, native, elf)?;
    let supplemental = native_packages(native)?;
    let observed: BTreeSet<&str> = packages
        .iter()
        .filter_map(|item| word(item, "id"))
        .collect();
    let collides = supplemental
        .iter()
        .any(|item| word(item, "id").is_some_and(|id| observed.contains(id)));
    require(!collides, "image_native_package_collision")?;
    packages.extend(supplemental);
    value.put("artifacts", Document::List(packages));
    let metadata = field(&mut value, "source").and_then(|source| field(source, "metadata"));
    let Some(Document::Record(fields)) = metadata else {
        return Err(SHAPE);
    };
    fields.retain(|(name, _)| name != "config" && name != "manifest");
    Ok(value)
}

#[cfg(test)]
#[path = "enrich_tests.rs"]
mod tests;
