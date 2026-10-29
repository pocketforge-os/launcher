#!/usr/bin/env bash
# Exercise contract tests carried by standalone runtime crates in vendor/.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

manifests=(
  vendor/pf-input-ipc/Cargo.toml
  vendor/pf-session-authority/Cargo.toml
)
locks=(
  vendor/pf-input-ipc/Cargo.lock
  vendor/pf-session-authority/Cargo.lock
)

for manifest in "${manifests[@]}"; do
  test -f "$manifest" || {
    echo "vendored-runtime-contracts: missing manifest: $manifest" >&2
    exit 1
  }
done
for lock in "${locks[@]}"; do
  test ! -e "$lock" || {
    echo "vendored-runtime-contracts: unexpected committed standalone lock: $lock" >&2
    exit 1
  }
done

target="$(mktemp -d "${RUNNER_TEMP:-/tmp}/pf-vendored-runtime-contracts.XXXXXX")"
cleanup() {
  find "$target" -mindepth 1 -delete
  rmdir "$target"
  for lock in "${locks[@]}"; do
    if test -f "$lock"; then
      find "$lock" -maxdepth 0 -type f -delete
    fi
  done
}
trap cleanup EXIT

CARGO_TARGET_DIR="$target" cargo test --offline \
  --manifest-path vendor/pf-input-ipc/Cargo.toml --test contract
CARGO_TARGET_DIR="$target" cargo test --offline \
  --manifest-path vendor/pf-session-authority/Cargo.toml --test device_unit_contract

echo "vendored-runtime-contracts: PASS input-ipc and session-authority"
