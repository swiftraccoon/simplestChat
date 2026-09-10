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
`lru` from 0.8.1 to 0.18.4.  The worker uses the compatible `new`, `contains`,
and `put` API surface; this removes RUSTSEC-2026-0253 from the active graph.

`mediasoup-sys-0.17.0` changes only `Cargo.toml`, `Cargo.toml.orig`,
`build.rs`, `tasks.py`, `meson.build`, `deps/libwebrtc/meson.build`,
`subprojects/abseil-cpp.wrap`, `subprojects/libuv.wrap`,
`subprojects/unordered-dense.wrap`, `subprojects/catch2.wrap`, the two files under
`subprojects/packagefiles/abseil-cpp/`, the four files under
`subprojects/packagefiles/libuv/`, and
`subprojects/packagefiles/ankerl-unordered-dense/meson.build`, and removes the package-local
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
The maintained `tasks.py` also selects Meson 1.12.0 and Ninja 1.13.2,
matching the exact constraints. These replace the archive's older build-tool
defaults; all other task definitions are unchanged. Keep both locations aligned
when updating these tools, and verify the complete native worker rebuild.

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

## Native dependency refresh (2026-09-09)

The following source archives are pinned to their verified upstream downloads:

| Dependency | Source archive | SHA-256 |
| --- | --- | --- |
| libuv 1.52.1 | [official distribution](https://dist.libuv.org/dist/v1.52.1/libuv-v1.52.1.tar.gz) | `66d511b9e6e334c0e62279eb234fbfb2b3110b1479c09b95b44c7afca8cff9e7` |
| unordered_dense 4.11.0 | [official tag archive](https://github.com/martinus/unordered_dense/archive/refs/tags/v4.11.0.tar.gz) | `a232f7433b45872d43e4dc74a25cbd58effc0be76e3d704b34e5de3c637eed77` |
| Catch2 3.16.0 | [official tag archive](https://github.com/catchorg/Catch2/archive/v3.16.0.tar.gz) | `0957cae5821b17ce07f0833aaa52b5137643a8382203221f363a8303c109af34` |

libuv's local Meson overlay comes from the already-pinned
[WrapDB 1.51.0-1 patch](https://wrapdb.mesonbuild.com/v2/libuv_1.51.0-1/get_patch)
(archive SHA-256
`0fb123dee5e74621a767a8f2a29dde7219c65a01a5fe63e3c8ffeed675a2d820`).
The original `meson.build` SHA-256 is
`7c106a5a406c1ef41d12dd2208f3784814df4778fae13887ae4ba527f29b88ce`;
only its project version changes to `1.52.1`, resulting in
`3eada92dfde42ed1148b17712bd363d28d73a74bf374484acd45e1faf27f487f`.
The compiler source lists still match upstream's 1.52.1 CMake build for the
supported macOS/Linux/Windows paths. The other files are byte-for-byte imports:
`meson_options.txt` (`dc02dc5b7d5bd069782529f59012e7ac25b53325b7783609c77a3b36ca819a7c`),
`link_file_in_build_dir.py` (`cba566c9f026b7c23c2460c4262ddd420e4da63986a892653d4c15a0f9e6943d`),
and `LICENSE.build` (`7939f4c45423cec4a18236ad0a88570e33508dd7462e07b1038001f90ece65fb`).
A local overlay avoids extracting the old archive into the wrong source directory;
the old 1.51.0 source fallback is removed. No libuv C source is patched.

unordered_dense keeps its existing header-only overlay; only the declared
version changes to `4.11.0` (overlay SHA-256
`8f535a2932f7074ed3c52c01de988b8e2881d94a3e2a169945c42d64ac0f3e77`).
Catch2 uses the published WrapDB `3.16.0-1` wrap unchanged. It is used only when
the native worker's `ms_build_tests` option is enabled, not by the production
worker. Its upstream native tests must be built separately from Cargo tests.

Two native source pins intentionally remain compatibility exceptions:

- Abseil stays on the security-fixed `20240722.2` LTS branch, although the
  [latest standalone release is `20260817.0`](https://github.com/abseil/abseil-cpp/releases/tag/20260817.0).
  Its newer hash-container APIs include breaking changes; updating the adapted
  libwebrtc dependency and the Meson overlay requires a coordinated native port.
- FlatBuffers stays on the worker's `24.3.25` source/tool version. Upstream's
  [latest normal release is `25.12.19`](https://github.com/google/flatbuffers/releases/tag/v25.12.19),
  with an additional `v25.12.19-2026-02-06-03fffb2` release tag whose status
  [upstream has questioned](https://github.com/google/flatbuffers/issues/8922).
  Advancing the schema compiler and C++ serialization headers is a separate
  compatibility change that needs generated-binding and protocol verification.

Remove each patch as soon as an official compatible release contains its fix.
When updating either archive, re-verify its crates.io checksum, review the
complete source diff, update this document and `Cargo.lock`, and run the full
mediasoup and application test suites.
