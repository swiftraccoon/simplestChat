#!/usr/bin/env bash
# Run pinned source-quality checks over every maintained Python file and stub.
set -euo pipefail

project_root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$project_root"
check_env="${PYTHON_CHECK_ENV:-$project_root/ops/ansible/.venv}"
for executable in python ruff basedpyright; do
  if [[ ! -x "$check_env/bin/$executable" ]]; then
    printf 'Missing %s; install build/python-requirements.txt in %s.\n' "$executable" "$check_env" >&2
    exit 1
  fi
done

# NUL-delimited discovery includes new, non-ignored files and cannot silently
# omit a future source directory. New roots must be added here AND in pyproject.
source_list="$(mktemp "${TMPDIR:-/tmp}/simplestchat-python-check.XXXXXXXX")"
trap 'rm -f -- "$source_list"' EXIT
git ls-files --cached --others --exclude-standard -z -- '*.py' '*.pyi' > "$source_list"
sources=()
while IFS= read -r -d '' source; do
  case "$source" in
    vendor/*|reference/*) continue ;; # Third-party source has its own upstream policy.
    build/*.py|ops/ansible/files/*.py|ops/ansible/callback_plugins/*.py|ops/ansible/tests/*.py|typings/*.pyi)
      sources+=("$source") ;;
    *) printf 'Python source outside the checked roots: %s\n' "$source" >&2; exit 1 ;;
  esac
done < "$source_list"
if [[ ${#sources[@]} -eq 0 ]]; then
  printf 'No maintained Python sources found.\n' >&2
  exit 1
fi

printf 'Checking %s maintained Python sources and stubs.\n' "${#sources[@]}"
"$check_env/bin/ruff" check -- "${sources[@]}"
"$check_env/bin/ruff" format --check -- "${sources[@]}"
# basedpyright has no `--` terminator; every selected path has an allowlisted,
# non-option root, so none can be interpreted as an extra checker option.
"$check_env/bin/basedpyright" --pythonpath "$check_env/bin/python" "${sources[@]}"
