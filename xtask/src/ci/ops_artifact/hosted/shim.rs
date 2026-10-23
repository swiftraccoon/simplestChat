//! One generated bootstrap adapter. It replaces its process with the exact
//! authenticated GCC role; it has no tool discovery, subprocess or pipeline.
//! The hosted /bin/sh and dynamic loader remain explicit runtime trust inputs.

use super::failure;
use crate::tooling::Result;
use std::path::Path;

pub(super) const GCC: &str = "host-tools/sysroot/usr/bin/x86_64-linux-gnu-gcc-13";
pub(super) const GCC_DIRECTORY: &str = "host-tools/sysroot/usr/lib/gcc/x86_64-linux-gnu/13";
pub(super) const BINUTILS_DIRECTORY: &str = "host-tools/sysroot/usr/bin";

/// `$0` is the absolute path supplied by the fixed producer recipe. Deriving
/// the prefix at execution time keeps the measured shim valid after staging.
pub(super) const CC: &[u8] = br##"#!/bin/sh
case "$0" in
  /*/host-tools/bin/cc) ;;
  *) exit 1 ;;
esac
ops_host_tools=${0%/bin/cc}
exec "$ops_host_tools/sysroot/usr/bin/x86_64-linux-gnu-gcc-13" \
  "--sysroot=$ops_host_tools/sysroot" \
  "-B$ops_host_tools/sysroot/usr/lib/gcc/x86_64-linux-gnu/13/" \
  "-B$ops_host_tools/sysroot/usr/libexec/gcc/x86_64-linux-gnu/13/" \
  "-B$ops_host_tools/sysroot/usr/bin/" "$@"
"##;

pub(super) fn environment(root: &Path) -> Result<[(String, std::ffi::OsString); 3]> {
    if !super::super::namespace::path(root) {
        return Err(failure(1));
    }
    let gcc = root.join(GCC_DIRECTORY);
    let programs = root.join(BINUTILS_DIRECTORY);
    let libexec = root.join("host-tools/sysroot/usr/libexec/gcc/x86_64-linux-gnu/13");
    let libraries = root.join("host-tools/sysroot/usr/lib/x86_64-linux-gnu");
    Ok([
        (
            "GCC_EXEC_PREFIX".into(),
            format!("{}/", gcc.display()).into(),
        ),
        (
            "COMPILER_PATH".into(),
            std::env::join_paths([libexec, programs]).map_err(|_| failure(1))?,
        ),
        (
            "LIBRARY_PATH".into(),
            std::env::join_paths([gcc, libraries]).map_err(|_| failure(1))?,
        ),
    ])
}
