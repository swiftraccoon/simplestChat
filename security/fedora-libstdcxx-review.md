# Fedora static C++ runtime license metadata review

Reviewed 2026-09-30; the two exact `image-license` records expire 2026-11-29.
They apply only to Fedora 44 `libstdc++-static` version `16.2.1-2.fc44`, epoch 0,
source RPM `gcc-16.2.1-2.fc44.src.rpm`, and the complete unchanged license expression
retained in [the evidence record](license-evidence/fedora-libstdcxx-16.2.1-2.fc44.json).
They do not add any term to the general image license allowlist.

The observed ARM64 native receipt reports that exact archive owner and Fedora's
complete GCC aggregate license expression. The retained evidence includes the
receipt's SHA-256, the full expression, the source-spec SHA-256 and GCC source
revision. It does not assert that every member of the archive survives final
linking. The separate x86_64 record is an explicit policy inference from the same
source RPM and inherited aggregate; it is not evidence of an observed x86_64
binary. A different architecture, epoch, package, release, source RPM or aggregate
expression requires another review.

The [Fedora GCC spec](https://src.fedoraproject.org/rpms/gcc/blob/f44/f/gcc.spec)
declares a source-wide aggregate including compiler, documentation, newlib and
runtime terms. Its `libstdc++-static` subpackage has no separate `License:`
override. The observed RPM therefore carries that aggregate rather than a
library-specific expression. The evidence records the exact spec bytes reviewed;
the branch URL alone is not an immutable identity.

The [upstream libstdc++ license documentation](https://gcc.gnu.org/onlinedocs/libstdc++/manual/license.html)
identifies its code as GPLv3 with the GCC Runtime Library Exception 3.1 and treats
documentation separately. The exception permits certain combinations with
independent modules under its stated compilation conditions. This supports a
narrow review of the runtime input; it does not establish that every term in the
source-wide aggregate applies to the final program, or waive obligations for
other GCC/newlib components.

The checker preserves Fedora's complete declared expression in the enriched SBOM
and reports this result under `waived`, including its exact fingerprint and RPM
PURL. It does not relabel the archive as a simpler SPDX license, remove package
metadata, or infer licenses for unknown components. Changed and missing
expressions remain blocking. The maintained tests also require a nonzero RPM
epoch to change the PURL, so a new epoch cannot inherit the epoch-zero review.

This is a scoped scanner-policy decision for the current application build, not
a distribution license or a finding that all notice/source obligations have been
fulfilled. Distribution changes require a fresh review of the actual components,
license texts and applicable obligations. Expiry, a changed Fedora package or
new build evidence must be reviewed rather than mechanically renewed.
