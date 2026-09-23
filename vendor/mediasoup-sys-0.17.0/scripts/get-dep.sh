#!/usr/bin/env bash
# This optional upstream importer fetched mutable source and rewrote the checkout.
# Dependency refreshes require reviewed immutable sources; see vendor/README.md.
set -euo pipefail
printf '%s\n' 'Vendored dependency importer is disabled; see vendor/README.md.' >&2
exit 1
