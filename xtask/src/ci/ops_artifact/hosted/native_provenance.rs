//! Closed joins for actual native build, selected guest recipe and measured tools.

use super::{
    catalog::{Catalog, Format, MUSL_COMMIT},
    context, failure, native_build, shim,
};
use crate::tooling::Result;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const MUSL_LIBS: [&str; 14] = [
    "crt1.o",
    "Scrt1.o",
    "rcrt1.o",
    "crti.o",
    "crtn.o",
    "libc.a",
    "libm.a",
    "librt.a",
    "libpthread.a",
    "libcrypt.a",
    "libutil.a",
    "libxnet.a",
    "libresolv.a",
    "libdl.a",
];
const ENVIRONMENT: [&str; 21] = [
    "CARGO_HOME",
    "HOME",
    "TMPDIR",
    "PATH",
    "RUSTC",
    "CC",
    "CC_x86_64_unknown_linux_musl",
    "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER",
    "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
    "OPS_MUSL_ROOT",
    "X86_64_UNKNOWN_LINUX_MUSL_OPENSSL_DIR",
    "X86_64_UNKNOWN_LINUX_MUSL_OPENSSL_STATIC",
    "X86_64_UNKNOWN_LINUX_MUSL_OPENSSL_NO_VENDOR",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_INCREMENTAL",
    "CARGO_TERM_COLOR",
    "LC_ALL",
    "TZ",
    "GCC_EXEC_PREFIX",
    "COMPILER_PATH",
    "LIBRARY_PATH",
];

fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(failure(1)) }
}
fn keys(value: &Value, expected: &[&str]) -> Result<()> {
    let object = value.as_object().ok_or_else(|| failure(1))?;
    require(
        object.len() == expected.len() && expected.iter().all(|name| object.contains_key(*name)),
    )
}
fn number(value: &Value, minimum: u64, maximum: u64) -> Result<u64> {
    value
        .as_u64()
        .filter(|n| (minimum..=maximum).contains(n))
        .ok_or_else(|| failure(1))
}
fn digest(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|s| super::super::digest(s))
        .ok_or_else(|| failure(1))
}
fn pin(value: &Value, maximum: u64) -> Result<u64> {
    keys(value, &["sha256", "bytes"])?;
    digest(&value["sha256"])?;
    number(&value["bytes"], 1, maximum)
}

fn recipe(value: &Value) -> Result<()> {
    keys(
        value,
        &[
            "schemaVersion",
            "profile",
            "workingDirectory",
            "manifest",
            "configuration",
            "environmentPolicy",
            "environmentNames",
            "rustflags",
            "target",
            "host",
            "toolchainVersion",
            "defaultFeatures",
            "features",
            "staticPieExpectation",
            "candidateExecuted",
        ],
    )?;
    require(
        value["schemaVersion"] == 1
            && value["profile"] == "ops-producer-recipe-v2"
            && value["workingDirectory"] == "freshRunRoot"
            && value["manifest"] == "stagedSource/Cargo.toml"
            && value["configuration"] == "generatedDirectorySourceOnly"
            && value["environmentPolicy"] == "clearThenSet"
            && value["rustflags"] == simplestchat_ops::distribution::build_plan::MUSL_RUSTFLAGS
            && value["target"] == context::TARGET
            && value["host"] == context::HOST
            && value["toolchainVersion"] == context::VERSION
            && value["defaultFeatures"] == false
            && value["features"] == json!(["maintain-guest"])
            && value["staticPieExpectation"] == "authenticatedMuslStaticPieCrt"
            && value["candidateExecuted"] == false,
    )?;
    let names = value["environmentNames"]
        .as_array()
        .ok_or_else(|| failure(1))?;
    let observed = names
        .iter()
        .map(Value::as_str)
        .collect::<Option<BTreeSet<_>>>()
        .ok_or_else(|| failure(1))?;
    require(names.len() == ENVIRONMENT.len() && observed == ENVIRONMENT.into_iter().collect())
}

fn resolution(value: &Value) -> Result<()> {
    keys(
        value,
        &[
            "schemaVersion",
            "profile",
            "originalSha256",
            "originalBytes",
            "packages",
            "workspaceMembers",
            "scope",
            "opsFeatures",
        ],
    )?;
    require(
        value["schemaVersion"] == 1
            && value["profile"] == "ops-producer-resolution-v2"
            && value["scope"] == "observedOpsFeaturesAndManifestLocations"
            && value["opsFeatures"] == json!(["maintain-guest"]),
    )?;
    digest(&value["originalSha256"])?;
    number(&value["originalBytes"], 1, 16 * 1024 * 1024)?;
    let packages = number(&value["packages"], 1, 4096)?;
    let members = number(&value["workspaceMembers"], 1, 64)?;
    require(members <= packages)
}

fn libraries(value: &Value) -> Result<u64> {
    let names = MUSL_LIBS
        .iter()
        .map(|name| format!("native/musl/lib/{name}"))
        .chain(["libssl.a", "libcrypto.a"].map(|name| format!("native/openssl/lib/{name}")))
        .collect::<Vec<_>>();
    keys(value, &names.iter().map(String::as_str).collect::<Vec<_>>())?;
    let mut total = 0;
    for name in names {
        let row = &value[&name];
        keys(row, &["sha256", "bytes", "format", "objects"])?;
        digest(&row["sha256"])?;
        let bytes = number(&row["bytes"], 1, 128 * 1024 * 1024)?;
        let object = name.ends_with(".o");
        require(
            row["format"]
                == if object {
                    "ELF64LE-ET_REL-EM_X86_64"
                } else {
                    "ar-ELF64LE-ET_REL-EM_X86_64"
                },
        )?;
        if object {
            number(&row["objects"], 1, 1)?;
            require(bytes >= 64)?;
        } else {
            let empty = name.starts_with("native/musl/") && !name.ends_with("/libc.a");
            number(
                &row["objects"],
                if empty { 0 } else { 1 },
                if empty { 0 } else { 16384 },
            )?;
            require(bytes >= 8)?;
        }
        total += bytes;
    }
    Ok(total)
}

fn native(value: &Value, catalog: &Catalog) -> Result<u64> {
    keys(
        value,
        &[
            "schemaVersion",
            "scope",
            "target",
            "muslSourceCommit",
            "opensslVersion",
            "opensslBuildRecipe",
            "sourceArchivePins",
            "configdataSha256",
            "disabled",
            "stockMuslSpecsSha256",
            "outputInventory",
            "libraries",
            "licenseNotices",
        ],
    )?;
    require(
        value["schemaVersion"] == 1
            && value["scope"] == "OpsMuslStaticNativeBuild"
            && value["target"] == context::TARGET
            && value["muslSourceCommit"] == MUSL_COMMIT
            && value["opensslVersion"] == "4.1.0"
            && value["opensslBuildRecipe"] == native_build::openssl_recipe()
            && value["disabled"] == json!({"shared":true,"dso":true,"module":true,"engine":true}),
    )?;
    digest(&value["configdataSha256"])?;
    digest(&value["stockMuslSpecsSha256"])?;
    let expected = catalog
        .artifacts
        .iter()
        .filter(|a| a.format == Format::NativeGzip)
        .map(|a| {
            (
                a.name.clone(),
                json!({"sha256":a.sha256,"bytes":a.bytes,"version":a.version}),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    require(
        expected.len() == 2
            && value["sourceArchivePins"]
                == serde_json::to_value(expected).map_err(|_| failure(1))?,
    )?;
    let inventory = &value["outputInventory"];
    keys(
        inventory,
        &["algorithm", "sha256", "files", "bytes", "mode"],
    )?;
    require(inventory["algorithm"] == "sha256-length-framed-v1" && inventory["mode"] == "0600")?;
    digest(&inventory["sha256"])?;
    number(&inventory["files"], 20, 2 * 4096 + 18)?;
    let bytes = number(&inventory["bytes"], 1, 8 * 1024 * 1024 * 1024)?;
    let notices = &value["licenseNotices"];
    keys(notices, &["musl", "openssl"])?;
    let notices_bytes = pin(&notices["musl"], 32768)? + pin(&notices["openssl"], 32768)?;
    require(libraries(&value["libraries"])? + notices_bytes <= bytes)?;
    Ok(bytes)
}

fn tools(value: &Value, native: &Value) -> Result<()> {
    keys(
        value,
        &[
            "schemaVersion",
            "kind",
            "selectedTools",
            "versionProbes",
            "sysroot",
            "authentication",
            "runtimeClosure",
        ],
    )?;
    require(
        value["schemaVersion"] == 1
            && value["kind"] == "ops-producer-tool-observations"
            && value["authentication"] == "callerPrerequisite"
            && value["runtimeClosure"] == "unproven",
    )?;
    let selected = &value["selectedTools"];
    keys(
        selected,
        &[
            "cargo",
            "rustc",
            "rustLld",
            "hostCc",
            "muslCc",
            "nativeLicenseNotices",
        ],
    )?;
    for name in ["cargo", "rustc", "rustLld", "hostCc", "muslCc"] {
        pin(&selected[name], 512 * 1024 * 1024)?;
    }
    for (name, source) in [("hostCc", shim::CC), ("muslCc", shim::MUSL_CC)] {
        require(
            selected[name]["sha256"] == super::super::sha(source)
                && selected[name]["bytes"].as_u64() == Some(source.len() as u64),
        )?;
    }
    let notices = selected["nativeLicenseNotices"]
        .as_array()
        .ok_or_else(|| failure(1))?;
    require(notices.len() == 2)?;
    for (notice, component) in notices.iter().zip(["musl", "openssl"]) {
        keys(notice, &["component", "sha256", "text"])?;
        let text = notice["text"].as_str().ok_or_else(|| failure(1))?;
        let expected = &native["licenseNotices"][component];
        require(
            notice["component"] == component
                && !text.is_empty()
                && text.len() <= 32768
                && digest(&notice["sha256"])? == super::super::sha(text.as_bytes())
                && digest(&notice["sha256"])? == digest(&expected["sha256"])?
                && expected["bytes"].as_u64() == Some(text.len() as u64),
        )?;
    }
    let probes = value["versionProbes"]
        .as_array()
        .ok_or_else(|| failure(1))?;
    require(probes.len() == 2)?;
    for probe in probes {
        keys(probe, &["version", "host", "sha256", "bytes"])?;
        require(probe["version"] == context::VERSION && probe["host"] == context::HOST)?;
        digest(&probe["sha256"])?;
        number(&probe["bytes"], 1, 16384)?;
    }
    let sysroot = &value["sysroot"];
    keys(sysroot, &["sha256", "bytes", "selectedStagedSysroot"])?;
    digest(&sysroot["sha256"])?;
    number(&sysroot["bytes"], 1, 4096)?;
    require(sysroot["selectedStagedSysroot"] == true)
}

pub(super) fn check(
    kit: &Value,
    selected_recipe: &Value,
    observed_resolution: &Value,
    toolchain: &Value,
    catalog: &Catalog,
) -> Result<()> {
    native(&kit["native"], catalog)?;
    recipe(selected_recipe)?;
    resolution(observed_resolution)?;
    tools(toolchain, &kit["native"])
}
