# Maintained Cargo patches

This directory contains source for two exact crates.io releases that need
small, auditable security fixes which have not yet shipped upstream.  Their
package names and versions are intentionally unchanged: Cargo's
`[patch.crates-io]` mechanism records that the source is local without
pretending this is a new upstream release.

## Provenance

| Directory | crates.io archive | SHA-256 | Upstream VCS revision |
| --- | --- | --- | --- |
| `mediasoup-0.27.0` | `https://crates.io/api/v1/crates/mediasoup/0.27.0/download` | `c79e3ce92e845fb431052e2bf0083a2c9c3772367588874a891826b50dda4c33` | `7b896f1743b6f4d9d7b237fc1d642828fa6c5b6c` (`rust/`) |
| `mediasoup-sys-0.17.0` | `https://crates.io/api/v1/crates/mediasoup-sys/0.17.0/download` | `b4193421652913559e68e6640d5fbe862241f313fa00cc446561e0d3f69ec684` | `3a6f865307c00ea1c5793b7de6de16d003854f29` (`worker/`) |

The VCS revisions above are the values embedded by crates.io in each
`.cargo_vcs_info.json`.  The original license and third-party license files
from both archives are retained in place.

To independently reproduce an import, download to a temporary directory,
verify the archive before extraction, and compare it with the corresponding
directory while excluding the maintained files listed below:

```bash
curl --fail --show-error --location --proto '=https' --tlsv1.2 \
  https://crates.io/api/v1/crates/mediasoup/0.27.0/download \
  --output mediasoup-0.27.0.crate
echo 'c79e3ce92e845fb431052e2bf0083a2c9c3772367588874a891826b50dda4c33  mediasoup-0.27.0.crate' \
  | shasum -a 256 --check

curl --fail --show-error --location --proto '=https' --tlsv1.2 \
  https://crates.io/api/v1/crates/mediasoup-sys/0.17.0/download \
  --output mediasoup-sys-0.17.0.crate
echo 'b4193421652913559e68e6640d5fbe862241f313fa00cc446561e0d3f69ec684  mediasoup-sys-0.17.0.crate' \
  | shasum -a 256 --check
```

## Maintained changes

`mediasoup-0.27.0` changes only `Cargo.toml` and `Cargo.toml.orig`, raising
`lru` from 0.8.1 to 0.18.3.  The worker uses the compatible `new`, `contains`,
and `put` API surface; this removes RUSTSEC-2026-0253 from the active graph.

`mediasoup-sys-0.17.0` changes only `Cargo.toml`, `Cargo.toml.orig`,
`build.rs`, `meson.build`, `deps/libwebrtc/meson.build`,
`subprojects/abseil-cpp.wrap`, and the two files under
`subprojects/packagefiles/abseil-cpp/`, and removes the package-local
`Cargo.lock` and `subprojects/openssl.wrap`. Cargo ignores a dependency's
nested lockfile, so removing that generated package artifact does not change
workspace resolution. The replacement build:

- requires pkg-config to find OpenSSL 3.5.8 or newer on the 3.5 LTS line
  (versions before 3.6.0);
- forbids Meson fallback to a bundled copy;
- requires static `libssl` and `libcrypto`; and
- makes libsrtp use the same resolved dependency.

The build script copies the crate's allowlisted package inputs into a fresh
Cargo `OUT_DIR` snapshot and runs Meson there. It tracks the complete immutable
crate source, the relevant declared tool/compiler and pip environment, resolved
Python configuration files, and the supported `OPENSSL_DIR` headers, pkg-config
metadata, and static archives. Each build-script attempt removes only its own
prior snapshot, native build directory, Python tool directory, and worker
archive before rebuilding.
This makes source/wrap changes and interrupted builds deterministic without
letting Meson download or extract files into the maintained vendor tree. Keep
`PACKAGE_SOURCE_PATHS` in `build.rs` synchronized with `package.include` in
`Cargo.toml` when updating the crate.

Non-doc builds fail closed unless `PIP_CONSTRAINT` names an existing file. The
repository uses `build/pip-constraints.txt`; CI, containers, checked-in VS Code
settings, deployment scripts, and the README build examples set it explicitly
so PyPI build tools cannot float between otherwise identical Cargo builds.

The Abseil wrap is pinned to the official `20240722.2` LTS archive (SHA-256
`ec820b01d9b328ca1f1b9c4e5b305d7a9fa03dc410ef64ba6654b637f9a4c3a8`).
That patch release carries the hash-container sizing fix for CVE-2025-0838.
Its local Meson overlay comes from the WrapDB `20240722.0-4` patch archive
(SHA-256
`e39d535c4707f6e342e84e3e616449e1cc98cb7fadda92a09820b0ae67c6d0d6`).
That archive contains only `meson.build` (original SHA-256
`25bfa2c796c3c8bac2c737ad9acb2fffcc27136df82e198b5a9f19720ace54ab`)
and `LICENSE.build` (SHA-256
`7939f4c45423cec4a18236ad0a88570e33508dd7462e07b1038001f90ece65fb`),
not C or C++ source.  `LICENSE.build` is retained byte-for-byte.  The local
`meson.build` changes only its declared project version from `20240722.0` to
`20240722.2` (resulting SHA-256
`454b10520ba4ba4a9995612ba2d9e6490b5477bb5093eff54af4c84917f71f19`)
so build metadata matches the source.  A local `patch_directory` is necessary
because the published patch archive is rooted at `abseil-cpp-20240722.0` and
therefore cannot overlay the `abseil-cpp-20240722.2` source directory directly.
The vulnerable source fallback remains removed.

The repository's pinned OpenSSL build helper supplies the dependency in Linux
CI and container builds.  Do not relax the minimum version or restore an
OpenSSL wrap to make a build pass.

Remove each patch as soon as an official compatible release contains its fix.
When updating either archive, re-verify its crates.io checksum, review the
complete source diff, update this document and `Cargo.lock`, and run the full
mediasoup and application test suites.
