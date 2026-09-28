#!/usr/bin/env bash
# Fail when Cargo.lock and the committed Cargo vendor tree are not one atomic input.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

marker=vendor/.pocketforge-vendor-lock
test -f "$marker" || { echo "check-vendor: missing $marker; run scripts/refresh-vendor.sh" >&2; exit 1; }

want="$(sed -n 's/^cargo_lock_sha256=//p' "$marker")"
have="$(sha256sum Cargo.lock | cut -d' ' -f1)"
test -n "$want" || { echo "check-vendor: marker has no Cargo.lock hash" >&2; exit 1; }
test "$have" = "$want" || {
  echo "check-vendor: Cargo.lock changed without a vendor refresh" >&2
  echo "check-vendor: run scripts/refresh-vendor.sh" >&2
  exit 1
}

python3 - "$root/vendor" <<'PY'
import re
import sys
import tomllib
from pathlib import Path

vendor = Path(sys.argv[1]).resolve()
failed = False


def dependency_tables(manifest: dict):
    for name in ("dependencies", "dev-dependencies", "build-dependencies"):
        yield manifest.get(name)
    workspace = manifest.get("workspace")
    if isinstance(workspace, dict):
        yield workspace.get("dependencies")
    targets = manifest.get("target")
    if isinstance(targets, dict):
        for target in targets.values():
            if not isinstance(target, dict):
                continue
            for name in ("dependencies", "dev-dependencies", "build-dependencies"):
                yield target.get(name)


def path_dependencies(manifest: dict):
    for table in dependency_tables(manifest):
        if not isinstance(table, dict):
            continue
        for dependency, value in table.items():
            if isinstance(value, dict) and isinstance(value.get("path"), str):
                yield dependency, value["path"]


for manifest_path in sorted(vendor.rglob("Cargo.toml")):
    relative_manifest = manifest_path.relative_to(vendor.parent)
    try:
        with manifest_path.open("rb") as stream:
            manifest = tomllib.load(stream)
    except (OSError, tomllib.TOMLDecodeError) as error:
        print(
            f"check-vendor: FATAL: cannot parse vendored manifest "
            f"{relative_manifest}: {error}",
            file=sys.stderr,
        )
        failed = True
        continue

    lines = manifest_path.read_text(encoding="utf-8").splitlines()
    for dependency, path_text in path_dependencies(manifest):
        target = (manifest_path.parent / path_text).resolve()
        try:
            target.relative_to(vendor)
            inside_vendor = True
        except ValueError:
            inside_vendor = False
        if inside_vendor and target.is_dir() and (target / "Cargo.toml").is_file():
            continue

        path_pattern = re.compile(r"\bpath\s*=\s*['\"]" + re.escape(path_text) + r"['\"]")
        line = next(
            (number for number, text in enumerate(lines, 1) if path_pattern.search(text)),
            1,
        )
        print(
            f"check-vendor: FATAL: unresolved vendored path dependency: "
            f"{relative_manifest}:{line}: {dependency}: {path_text}",
            file=sys.stderr,
        )
        failed = True

raise SystemExit(1 if failed else 0)
PY

cargo metadata --offline --locked --format-version 1 >/dev/null
echo "check-vendor: vendor tree matches Cargo.lock and path dependencies resolve"
