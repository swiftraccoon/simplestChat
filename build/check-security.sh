#!/usr/bin/env bash
# Shared local/CI entrypoint. Install only authenticated wheels into an isolated prefix.
set -euo pipefail
umask 077
project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"
python3 -c 'import sys; assert sys.version_info >= (3, 12), "Python 3.12 or newer is required"'
case "${1:-}" in
  fast|deep)
    rust_toolchain="$(python3 -c 'import tomllib; from pathlib import Path; print(tomllib.loads(Path("rust-toolchain.toml").read_text())["toolchain"]["channel"])')"
    rust_binary="$(rustup which --toolchain "$rust_toolchain" cargo)"
    rust_bin_directory="$(dirname -- "$rust_binary")"
    export PATH="$rust_bin_directory:$PATH"
    ;;
esac
requirements_digest="$(python3 -c 'import hashlib; from pathlib import Path; print(hashlib.sha256(Path("security/requirements.txt").read_bytes()).hexdigest())')"
# Outside the Cargo target directory: CI's rust-cache saves and restores
# `target/`, and a receipt-less prefix restored from a partial save stopped
# every scheduled policy run until the cache aged out.
security_root="$project_root/.cache/security-environments"
security_env="$security_root/$requirements_digest"
if [[ ! -f "$security_env/ready" ]]; then
  if [[ -L "$security_env" || -e "$security_env" ]]; then
    # An interrupted install (or a stale restore) leaves no receipt. Rebuild
    # from the lock instead of trusting or refusing the remains forever.
    printf 'Rebuilding an incomplete scanner environment: %s\n' "$security_env" >&2
    stale="$(readlink -- "$security_env" || true)"
    rm -rf -- "$security_env"
    if [[ -n "$stale" && "$stale" == "$security_root"/.install.* ]]; then
      rm -rf -- "$stale"
    fi
  fi
  mkdir -p "$security_root"
  staging="$(mktemp -d "$security_root/.install.XXXXXXXX")"
  trap 'rm -rf -- "$staging"' EXIT
  python3 -m venv "$staging"
  "$staging/bin/python" -m pip --isolated install --disable-pip-version-check \
    --require-hashes --only-binary=:all: --requirement security/requirements.txt
  "$staging/bin/python" -m pip --isolated check
  printf '%s\n' "$requirements_digest" > "$staging/ready"
  # venv executables contain absolute shebangs: retain the staging environment
  # as its final owned prefix and publish only a checked receipt link.
  python3 - "$staging" "$security_env" <<'PY'
import os
import sys
os.symlink(sys.argv[1], sys.argv[2], target_is_directory=True)
PY
  trap - EXIT
fi
if [[ "$(cat "$security_env/ready")" != "$requirements_digest" ]]; then
  printf 'Scanner environment receipt does not match the current lock.\n' >&2
  exit 1
fi
exec "$security_env/bin/python" build/security_check.py "$@"
