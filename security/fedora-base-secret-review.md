# Fedora base-image secret findings

Reviewed **2026-09-30** by agents under the owner-authorized security rollout.
Owner: `swiftraccoon`. Review expiry: **2026-11-29**.

The [retained evidence](secret-evidence/fedora-base-2026-09-30.json) supports
15 exact Gitleaks findings in five files from the pinned Fedora 44 amd64 base
layer. These findings contain public upstream test data or format-recognition
data. They are not application or deployment credentials. The review does not
approve other paths, other detected bytes, package behavior, or an entire
detector class.

The original [CI scan](https://github.com/swiftraccoon/simplestChat/actions/runs/36743690076)
failed with 20 secret findings. This review addresses only the 15 listed here.
The frontend bundle and updated RPM database are outside its scope. A failed
image scan remains ineligible for signing or deployment.

## Exact evidence

The canonical revision is `463303ae4c41842a7f4f53f12d330f11f50bee71`, with image
ID `sha256:af47243c41cb21d6099bbaaee80dd3a5b14b9c9206deee9dd13458a8f4849a08`.
The downloaded evidence artifact is `11112446228`; its API-reported archive
SHA-256 was independently checked before the reports were read.

The Fedora multi-architecture index, selected amd64 manifest and decompressed
base-layer digest were authenticated before extracting the five regular files.
All original file sizes and SHA-256 values match the canonical secret path map.
The maintained printable-ASCII projection reproduces every recorded projection
size and hash. Checksum-pinned Gitleaks 8.30.1 reproduces all 15 rule/path/line
tuples. This local reproduction used Darwin arm64; the original scanner ran on
Linux amd64. The original image itself was not executed for this review.

| Exact base-layer content | Findings | Source assessment |
| --- | ---: | --- |
| Two TPM FAPI profiles, `P_ECCP384SHA384.json` and `P_RSA3072SHA384.json` | 2 | Each complete file matches upstream tpm2-tss 4.1.3. The matched `nvPublic.authPolicy` field is a public SHA-384 policy digest, not a secret authorization value. |
| `libgnutls.so.30.42.0` | 10 | Each complete PEM key matches a deliberately published constant in GnuTLS 3.8.13 `lib/crypto-selftests-pk.c`. The constants feed local algorithm self-tests. They are complete keys; they are not being dismissed as mere delimiter strings. |
| `libssh.so.4.12.0` | 1 | The matched nonempty segments are the BEGIN delimiter, a parser diagnostic and the END delimiter from libssh 0.12.2. There is no encoded key payload. |
| `magic.mgc` | 2 | The printable projection joins distinct compiled file-recognition records. Their markers match file 5.46 `magic/Magdir/ssh` and `ssl`; neither finding contains a paired private-key end marker and encoded key payload. |

The evidence records each original path, file/projection hash, exact finding
fingerprint, package identity, primary source URL and source-byte hash. GnuTLS
entries also identify the matching constant and its complete decoded hash.
Source archives came from the upstream HTTPS publishers; this review does not
claim independent detached-signature verification of those archives.

## Enforcement and limits

Each ledger entry matches the pinned scanner/projection format, detector rule,
exact original layer path, complete uniquely resolved match-region length and
SHA-256. All 15 regions were independently resolved and compared with the private
scanner's complete `Match` bytes. No captured-secret substring or normalized
content is substituted. A changed region, path, rule or format cannot inherit
the review. Changed surrounding file bytes or line positions preserve fresh
whole-file/projection evidence without changing the identity of these exact
reviewed public bytes. This is not an approval of the containing file or package.
There is no general exception for binaries, RPM files,
public-key libraries, TPM configuration, or private-key findings. The detector
self-test and all-layer scanning, including deleted files, remain enabled.

No candidate key or match text is committed or uploaded by this review. The
retained source and content hashes support independent comparison; they do not
make the referenced public test keys suitable for use as real credentials.
Changed matched bytes or scope require a new assessment, and expiry is never
renewed automatically. The original full-file hashes and package/source records
remain provenance for this review; per-image reports retain their current hashes.
