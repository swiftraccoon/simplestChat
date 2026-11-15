//! `simplestchat-security tools install|path` and `rulesets plan|check|apply`:
//! the security gate's command line.

use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

const USAGE: &str = "usage: simplestchat-security tools|rulesets ...";

/// The checkout this executable was built from holds the lock and the cache.
fn repository_root() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

fn main() -> ExitCode {
    let arguments: Option<Vec<String>> = std::env::args_os()
        .skip(1)
        .map(|argument| argument.into_string().ok())
        .collect();
    match (repository_root(), arguments.as_deref()) {
        (Some(root), Some([group, rest @ ..])) if group == "tools" => {
            simplestchat_security::tools::run(&root, rest)
        }
        (Some(root), Some([group, rest @ ..])) if group == "rulesets" => {
            let search = std::env::var_os("PATH");
            simplestchat_security::rulesets::run(&root, rest, search.as_deref())
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
