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

/// The compiler consumes only measured target headers/CRT and the fixed specs.
/// The producer supplies OPS_MUSL_ROOT in its cleared, checked environment.
pub(super) const MUSL_CC: &[u8] = br##"#!/bin/sh
case "$0" in
  /*/host-tools/bin/musl-cc) ;;
  *) exit 1 ;;
esac
ops_host_tools=${0%/bin/musl-cc}
exec "$ops_host_tools/sysroot/usr/bin/x86_64-linux-gnu-gcc-13" \
  "--sysroot=$ops_host_tools/sysroot" \
  "-B$ops_host_tools/sysroot/usr/lib/gcc/x86_64-linux-gnu/13/" \
  "-B$ops_host_tools/sysroot/usr/libexec/gcc/x86_64-linux-gnu/13/" \
  "-B$ops_host_tools/sysroot/usr/bin/" \
  "-specs=$ops_host_tools/musl-gcc.specs" "$@"
"##;

/// Upstream musl's GCC specs with explicit static PIE startup and no ambient
/// target prefix. GCC's builtin getenv function preserves relocation of the kit.
pub(super) const MUSL_SPECS: &[u8] = concat!(
    "%rename cpp_options old_cpp_options\n",
    "\n",
    "*cpp_options:\n",
    "-nostdinc -isystem %:getenv(OPS_MUSL_ROOT /include) -isystem include%s %",
    "(old_cpp_options)\n",
    "\n",
    "*cc1:\n",
    "%(cc1_cpu) -nostdinc -isystem %:getenv(OPS_MUSL_ROOT /include) -isystem ",
    "include%s\n",
    "\n",
    "*link_libgcc:\n",
    "-L%:getenv(OPS_MUSL_ROOT /lib) -L .%s\n",
    "\n",
    "*libgcc:\n",
    "libgcc.a%s %:if-exists(libgcc_eh.a%s)\n",
    "\n",
    "*startfile:\n",
    "%{!shared:%{static-pie:%:getenv(OPS_MUSL_ROOT /lib/rcrt1.o);:%:getenv(OP",
    "S_MUSL_ROOT /lib/Scrt1.o)}} %:getenv(OPS_MUSL_ROOT /lib/crti.o) crtbegin",
    "S.o%s\n",
    "\n",
    "*endfile:\n",
    "crtendS.o%s %:getenv(OPS_MUSL_ROOT /lib/crtn.o)\n",
    "\n",
    "*link:\n",
    "-L%:getenv(OPS_MUSL_ROOT /lib) -nostdlib %{shared:-shared} %{static:-sta",
    "tic} %{static-pie:-static -pie --no-dynamic-linker -z text} %{rdynamic:-",
    "export-dynamic}\n",
    "\n",
    // Preserve upstream's two blank lines after each empty specification.
    // GCC otherwise consumes the next header as its body and rejects EOF.
    "*esp_link:\n\n\n",
    "*esp_options:\n\n\n",
    "*esp_cpp_options:\n\n\n",
)
.as_bytes();

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
