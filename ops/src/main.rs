//! The first operations command reads one local manifest; it has no installed
//! host or release runtime callsite. A manual parser keeps this small surface
//! consistent with xtask without adding a CLI framework dependency.

use simplestchat_ops::release::manifest::Manifest;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "Usage: simplestchat-ops release manifest --path PATH";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.len() != 4 || args[0] != "release" || args[1] != "manifest" || args[2] != "--path" {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let result = Manifest::read(Path::new(&args[3]));
    let bytes = match result {
        Ok(manifest) => serde_json::to_vec(&serde_json::json!({
            "manifest": manifest.fields(), "manifestSha256": manifest.sha256(),
            "manifestBytes": manifest.as_json().len(),
        }))
        .expect("validated manifest serialization"),
        Err(_) => {
            let _ = std::io::stdout()
                .lock()
                .write_all(b"{\"passed\":false,\"failure\":\"invalid_release_manifest\"}\n");
            return ExitCode::FAILURE;
        }
    };
    let mut output = std::io::stdout().lock();
    if output
        .write_all(&bytes)
        .and_then(|()| output.write_all(b"\n"))
        .is_err()
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
