//! The image tier: what is checked of a saved production image, without
//! running it and without trusting a path, a link or a size that it names.

use crate::{Fault, elf};
use std::process::ExitCode;

pub mod advisories;
pub mod archive;
pub mod bindings;
pub mod enrich;
pub mod license_identity;
pub mod licenses;
pub mod notices;
pub mod policy;
pub mod runtime;
pub mod secrets;
pub mod spans;

/// Whether `text` is exactly `digits` hexadecimal digits in small letters.
pub(crate) fn small_hex(text: &str, digits: usize) -> bool {
    let digit = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    text.len() == digits && text.bytes().all(digit)
}

/// Whether `path` is absolute and spelled the one way it can be: no empty
/// step, but exactly two slashes may begin it, no step that stays and none
/// up.
pub(crate) fn canonical(path: &str) -> bool {
    let rest = path.strip_prefix("//").or_else(|| path.strip_prefix('/'));
    rest.is_some_and(|rest| {
        rest.is_empty() || rest.split('/').all(|step| !matches!(step, "" | "." | ".."))
    })
}

/// The code of a stop of the ELF module's readers of the image's files: a
/// refusal of the path through the image's own links or of the reader of
/// a binary keeps its own, and `missing` and `unreadable` name a file that
/// is not there and one that cannot be read.
pub(crate) fn stopped(stop: elf::Stop, missing: &'static str, unreadable: &'static str) -> Fault {
    const KNOWN: [&str; 7] = [
        "image_path_invalid",
        "image_path_escape",
        "image_symlink_limit",
        "image_file_kind",
        "image_path_empty",
        "elf_file_size",
        "elf_file_changed",
    ];
    match stop {
        elf::Stop::Refused(refusal) => {
            let known = KNOWN.iter().find(|known| **known == refusal.reason);
            Fault(known.copied().unwrap_or("image_path_invalid"))
        }
        elf::Stop::Missing => Fault(missing),
        elf::Stop::Unreadable => Fault(unreadable),
    }
}

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
