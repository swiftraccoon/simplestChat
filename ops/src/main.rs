//! Allowlisted read-only operations. No installed host/runtime callsite exists;
//! the manual parser cannot start another command, engine or network operation.

mod receipt_cli;

use simplestchat_ops::release::manifest::Manifest;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = concat!(
    "Usage: simplestchat-ops release manifest --path PATH\n",
    "       simplestchat-ops release receipt --envelope PATH --receipt PATH",
);

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let (result, failure) = if args.len() == 4
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
    } else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let value = match result {
        Ok(value) => value,
        Err(()) => {
            let bytes = match failure {
                "invalid_release_receipt" => {
                    b"{\"passed\":false,\"failure\":\"invalid_release_receipt\"}\n".as_slice()
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
