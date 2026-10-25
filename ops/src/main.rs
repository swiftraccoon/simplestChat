//! Allowlisted inspection, explicit local release builds and host capacity.
//! Inspection starts no child, engine or network. Release builds own their local
//! Git/Docker effects; cached verification owns fresh maintained verifier calls.
//! Capacity retains its separate authenticated host contract.

mod assets_cli;
mod preflight_cli;
mod receipt_cli;

use simplestchat_ops::{
    distribution,
    distribution::manifest::assembly::{EvidenceSources, prepare_artifact},
    distribution::manifest::{ArtifactManifest, Selection, verify_pair},
    release::{archive::verify_archive, manifest::Manifest},
};
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = concat!(
    "Usage: simplestchat-ops release build --output PATH [--timeout-seconds N] [--image-id ID]\n",
    "       simplestchat-ops release verify [OPTIONS]\n",
    "       simplestchat-ops release fetch [OPTIONS]\n",
    "       simplestchat-ops release receive [OPTIONS]\n",
    "       simplestchat-ops release stage [OPTIONS]\n",
    "       simplestchat-ops release deploy [OPTIONS]\n",
    "       simplestchat-ops release backup verify [OPTIONS]\n",
    "       simplestchat-ops release maintain-prerequisite [OPTIONS]\n",
    "       simplestchat-ops release attestation prepare [OPTIONS]\n",
    "       simplestchat-ops release attestation verify [OPTIONS]\n",
    "       simplestchat-ops release manifest --path PATH\n",
    "       simplestchat-ops release archive --manifest PATH --path PATH\n",
    "       simplestchat-ops release receipt --envelope PATH --receipt PATH\n",
    "       simplestchat-ops release preflight [--helpers-json JSON]\n",
    "       simplestchat-ops release assets initialize [--revision REV]\n",
    "       simplestchat-ops release assets prune|inspect\n",
    "       simplestchat-ops release assets select --revision REV\n",
    "       simplestchat-ops release assets capture --path TAR\n",
    "       simplestchat-ops release assets publish --capture REV TAR [--capture REV TAR]\n",
    "       simplestchat-ops distribution inspect-executable --path PATH\n",
    "       simplestchat-ops distribution install --request ABS --request-sha256 SHA\n",
    "       simplestchat-ops distribution verify-pair --manifest PATH ",
    "--path PATH --selection PATH\n",
    "       simplestchat-ops distribution build-plan --selection PATH --source-root PATH\n",
    "       simplestchat-ops distribution prepare-artifact --manifest PATH --path PATH ",
    "--selection PATH --evidence PATH --output PATH\n",
    "       simplestchat-ops capacity run --server-image IMAGE --generator-image IMAGE [OPTIONS]\n",
    "       simplestchat-ops capacity-vps serve --transport-json JSON --origin-monotonic-ns N\n",
    "       simplestchat-ops capacity-vps worker --run RUN --request-sha256 SHA",
);

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.len() >= 2 && args[0] == "release" && args[1] == "maintain-prerequisite" {
        return simplestchat_ops::release::maintain_prerequisite::run(&args[2..]);
    }
    if args.len() >= 3 && args[0] == "release" && args[1] == "backup" && args[2] == "verify" {
        return simplestchat_ops::release::backup::verify::run(&args[3..]);
    }
    if args.len() >= 2 && args[0] == "release" && args[1] == "deploy" {
        return simplestchat_ops::release::deploy::run(&args[2..]);
    }
    if args.len() >= 2 && args[0] == "release" && args[1] == "stage" {
        return simplestchat_ops::release::stage::run(&args[2..]);
    }
    if args.len() >= 2 && args[0] == "release" && args[1] == "receive" {
        return simplestchat_ops::release::receive::run(&args[2..]);
    }
    if args.len() >= 2 && args[0] == "release" && args[1] == "fetch" {
        return simplestchat_ops::release::fetch::run(&args[2..]);
    }
    if args.len() >= 2 && args[0] == "release" && args[1] == "verify" {
        return simplestchat_ops::release::verify::run(&args[2..]);
    }
    if args.len() >= 3 && args[0] == "release" && args[1] == "attestation" && args[2] == "prepare" {
        return simplestchat_ops::release::attestation::prepare::run(&args[3..]);
    }
    if args.len() >= 3 && args[0] == "release" && args[1] == "attestation" && args[2] == "verify" {
        return simplestchat_ops::release::attestation::verify::run(&args[3..]);
    }
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.first().is_some_and(|arg| arg == "capacity") {
        if args.get(1).is_some_and(|arg| arg == "run") {
            return simplestchat_load_test::capacity::run_with_program(
                &args[1..],
                "simplestchat-ops",
            );
        }
        if args.len() == 2 && matches!(args[1].to_str(), Some("--help" | "-h")) {
            return simplestchat_load_test::capacity::run_with_program(
                &[OsString::from("run"), OsString::from("--help")],
                "simplestchat-ops",
            );
        }
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    if args.first().is_some_and(|arg| arg == "capacity-vps") {
        if args.len() == 6 {
            if args[1] == "serve"
                && args[2] == "--transport-json"
                && args[4] == "--origin-monotonic-ns"
            {
                return simplestchat_ops::capacity_vps::serve(&args[3], &args[5]);
            }
            if args[1] == "worker" && args[2] == "--run" && args[4] == "--request-sha256" {
                return simplestchat_ops::capacity_vps::worker(&args[3], &args[5]);
            }
        }
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    if args.len() >= 2 && args[0] == "distribution" && args[1] == "install" {
        if args.len() != 6 || args[2] != "--request" || args[4] != "--request-sha256" {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
        let result = args[5]
            .to_str()
            .ok_or(distribution::install::Failure::Unproven)
            .and_then(|sha| distribution::install::install(Path::new(&args[3]), sha));
        let (value, code) = match result {
            Ok(receipt) => (serde_json::to_value(receipt), ExitCode::SUCCESS),
            Err(error) => (
                Ok(serde_json::json!({
                    "schemaVersion":1,"passed":false,"failure":error.code(),
                })),
                ExitCode::from(error.exit_code()),
            ),
        };
        let Ok(mut bytes) = value.and_then(|value| serde_json::to_vec(&value)) else {
            return ExitCode::FAILURE;
        };
        bytes.push(b'\n');
        return if std::io::stdout().lock().write_all(&bytes).is_ok() || code != ExitCode::SUCCESS {
            code
        } else {
            ExitCode::FAILURE
        };
    }
    if args.len() >= 2 && args[0] == "release" && args[1] == "build" {
        return simplestchat_ops::release::build::run(&args[2..]);
    }
    if args.len() >= 2 && args[0] == "release" && args[1] == "assets" {
        let code = assets_cli::run(&args[2..]);
        if code == ExitCode::from(2) {
            eprintln!("{USAGE}");
        }
        return code;
    }
    if args.len() >= 2 && args[0] == "release" && args[1] == "preflight" {
        if args.len() == 2 {
            return preflight_cli::run(None);
        }
        if args.len() == 4 && args[2] == "--helpers-json" {
            return preflight_cli::run(Some(&args[3]));
        }
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let (result, failure) = if args.len() == 12
        && args[0] == "distribution"
        && args[1] == "prepare-artifact"
        && args[2] == "--manifest"
        && args[4] == "--path"
        && args[6] == "--selection"
        && args[8] == "--evidence"
        && args[10] == "--output"
    {
        let result = ArtifactManifest::read(Path::new(&args[3]))
            .map_err(|_| ())
            .and_then(|manifest| {
                let selected = Selection::read(Path::new(&args[7])).map_err(|_| ())?;
                let evidence = EvidenceSources::read(Path::new(&args[9])).map_err(|_| ())?;
                prepare_artifact(
                    &manifest,
                    Path::new(&args[5]),
                    &selected,
                    &evidence,
                    Path::new(&args[11]),
                )
                .map_err(|_| ())
            })
            .and_then(|value| serde_json::to_value(value).map_err(|_| ()));
        (result, "invalid_ops_preparation")
    } else if args.len() == 6
        && args[0] == "distribution"
        && args[1] == "build-plan"
        && args[2] == "--selection"
        && args[4] == "--source-root"
    {
        let result = Selection::read(Path::new(&args[3]))
            .map_err(|_| ())
            .and_then(|selection| {
                distribution::build_plan::inspect(&selection, Path::new(&args[5])).map_err(|_| ())
            })
            .and_then(|plan| serde_json::to_value(plan).map_err(|_| ()));
        (result, "invalid_ops_build_plan")
    } else if args.len() == 8
        && args[0] == "distribution"
        && args[1] == "verify-pair"
        && args[2] == "--manifest"
        && args[4] == "--path"
        && args[6] == "--selection"
    {
        let result = ArtifactManifest::read(Path::new(&args[3]))
            .and_then(|manifest| {
                let selection = Selection::read(Path::new(&args[7]))?;
                verify_pair(&manifest, Path::new(&args[5]), &selection)
            })
            .map_err(|_| ())
            .and_then(|pair| serde_json::to_value(pair).map_err(|_| ()));
        (result, "invalid_ops_artifact")
    } else if args.len() == 4
        && args[0] == "distribution"
        && args[1] == "inspect-executable"
        && args[2] == "--path"
    {
        let result = distribution::inspect(Path::new(&args[3]))
            .map_err(|_| ())
            .and_then(|identity| serde_json::to_value(identity).map_err(|_| ()));
        (result, "invalid_executable")
    } else if args.len() == 4
        && args[0] == "release"
        && args[1] == "manifest"
        && args[2] == "--path"
    {
        let result = Manifest::read(Path::new(&args[3]))
            .map(|manifest| {
                serde_json::json!({
                    "manifest": manifest.fields(), "manifestSha256": manifest.sha256(),
                    "manifestBytes": manifest.as_json().len(),
                })
            })
            .map_err(|_| ());
        (result, "invalid_release_manifest")
    } else if args.len() == 6
        && args[0] == "release"
        && args[1] == "receipt"
        && args[2] == "--envelope"
        && args[4] == "--receipt"
    {
        (
            receipt_cli::inspect(Path::new(&args[3]), Path::new(&args[5])),
            "invalid_release_receipt",
        )
    } else if args.len() == 6
        && args[0] == "release"
        && args[1] == "archive"
        && args[2] == "--manifest"
        && args[4] == "--path"
    {
        let result = Manifest::read(Path::new(&args[3]))
            .map_err(|_| ())
            .and_then(|manifest| verify_archive(Path::new(&args[5]), &manifest).map_err(|_| ()))
            .and_then(|archive| serde_json::to_value(archive.identity()).map_err(|_| ()));
        (result, "invalid_release_archive")
    } else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let value = match result {
        Ok(value) => value,
        Err(()) => {
            let bytes = match failure {
                "invalid_ops_preparation" => {
                    b"{\"passed\":false,\"failure\":\"invalid_ops_preparation\"}\n".as_slice()
                }
                "invalid_ops_build_plan" => {
                    b"{\"passed\":false,\"failure\":\"invalid_ops_build_plan\"}\n".as_slice()
                }
                "invalid_ops_artifact" => {
                    b"{\"passed\":false,\"failure\":\"invalid_ops_artifact\"}\n".as_slice()
                }
                "invalid_executable" => {
                    b"{\"passed\":false,\"failure\":\"invalid_executable\"}\n".as_slice()
                }
                "invalid_release_receipt" => {
                    b"{\"passed\":false,\"failure\":\"invalid_release_receipt\"}\n".as_slice()
                }
                "invalid_release_archive" => {
                    b"{\"passed\":false,\"failure\":\"invalid_release_archive\"}\n".as_slice()
                }
                _ => b"{\"passed\":false,\"failure\":\"invalid_release_manifest\"}\n".as_slice(),
            };
            let _ = std::io::stdout().lock().write_all(bytes);
            return ExitCode::FAILURE;
        }
    };
    let mut output = std::io::stdout().lock();
    if serde_json::to_writer(&mut output, &value)
        .and_then(|()| output.write_all(b"\n").map_err(serde_json::Error::io))
        .is_err()
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
