# Password selection corpus

This is an unmodified snapshot of SecLists' common-credential list, used only to
reject weak new account passwords. It is not a complete breach database, an
entropy estimator, or a claim of NIST certification. No submitted password is
sent to SecLists or another external service.

- Upstream: <https://github.com/danielmiessler/SecLists>
- Revision: `98e99c1e6e98f36044d830b35d55f1620808eb32`
- Path: `Passwords/Common-Credentials/10k-most-common.txt`
- Entries: 10,001 unique lowercase ASCII lines; 73,026 bytes.
- Data SHA-256: `68782d6a4a19a4768d5f15dd66bd534e7a33055cc755411e33f16d18c50fdcce`
- License: upstream MIT, copyright 2018 Daniel Miessler; copied unchanged in
  `LICENSE`, SHA-256 `3dbdc93d5f8829de0941744841730a09c106d0732e5ae0e98ca1d77be7ded66c`.

The file name describes an approximate common-password corpus. Only one entry
already reaches this application's 15-character minimum. Its main additional
value is refusing commonly guessed stems even when someone pads them with years,
digits or punctuation. `src/auth/common_passwords.rs` combines this snapshot with
the project's curated entries and checks both the complete comparison form and
one candidate with at most 16 ASCII digits/punctuation removed from each end.
It does not split phrases into words or require particular character classes.
Comparison uses compatibility normalization, case folding and default-ignorable
removal only for refusal matching; password hashing and verification use NFC.

The application includes these fixed bytes at compile time and performs all
checks locally. Runtime and build never download an updated list. Unit tests pin
the source bytes, license and shape. A deliberate corpus update must review the
new revision and license, refresh both checksums and tests, and rerun password
selection tests. Native source distributions retain this directory. Container
images retain the license and this provenance notice under
`/usr/share/licenses/simplestchat/seclists/`.
