//! Allowlisted read-only operations. No installed host/runtime callsite exists;
//! the manual parser cannot start another command, engine or network operation.

mod receipt_cli;

use simplestchat_ops::{
    distribution,
    distribution::manifest::{ArtifactManifest, Selection, verify_pair},
    release::{archive::verify_archive, manifest::Manifest},
};
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = concat!(
    "Usage: simplestchat-ops release manifest --path PATH\n",
    "       simplestchat-ops release archive --manifest PATH --path PATH\n",
    "       simplestchat-ops release receipt --envelope PATH --receipt PATH\n",
    "       simplestchat-ops distribution inspect-executable --path PATH\n",
    "       simplestchat-ops distribution verify-pair --manifest PATH ",
    "--path PATH --selection PATH",
);

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let (result, failure) = if args.len() == 8
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
