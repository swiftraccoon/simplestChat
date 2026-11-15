//! The image tier: what is checked of a saved production image, without
//! running it and without trusting a path, a link or a size that it names.

use std::process::ExitCode;

pub mod archive;

const USAGE: &str =
    "usage: simplestchat-security image archive --archive FILE --manifest FILE --output DIR";

/// Run a command of the image tier.
pub fn run(arguments: &[String]) -> ExitCode {
    let asked = match arguments.split_first() {
        Some((command, rest)) if command == "archive" => archive::asked(rest),
        _ => None,
    };
    let Some([image, manifest, output]) = asked else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match archive::extract(&image, &manifest, &output, &archive::Limits::REVIEWED) {
        Ok(_) => ExitCode::SUCCESS,
        Err(fault) => {
            eprintln!("Image archive check failed: {fault}");
            ExitCode::from(1)
        }
    }
}
