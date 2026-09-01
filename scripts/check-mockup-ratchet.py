#!/usr/bin/env python3
"""Reject mockup-diff threshold decreases without a same-change waiver."""

import argparse
from collections import Counter
from decimal import Decimal, InvalidOperation
from pathlib import Path
import re
import subprocess
import sys

THRESHOLDS_PATH = Path("crates/pf-shell/tests/mockup_diff.rs")
WAIVERS_PATH = Path("crates/pf-shell/tests/goldens/RATCHET_WAIVERS.md")
TABLE_RE = re.compile(r"const\s+SCREENS\b.*?=\s*\[(.*?)\];", re.DOTALL)
ROW_RE = re.compile(
    r'^\s*\(\s*"(?P<screen>[^"]+)"\s*,\s*"[^"]+"\s*,\s*'
    r'(?P<value>[0-9][0-9_.]*(?:[eE][+-]?[0-9_]+)?)\s*\),\s*(?://.*)?$'
)
WAIVER_RE = re.compile(
    r"RATCHET-WAIVER:\s+(?P<screen>\S+)\s+(?P<old>\S+)\s+->\s+"
    r"(?P<new>\S+)\s+—\s+(?P<reason>\S.*)$"
)


def decimal(value: str) -> Decimal:
    try:
        return Decimal(value.replace("_", ""))
    except InvalidOperation as error:
        raise ValueError(f"invalid decimal value {value!r}") from error


def thresholds(source: str) -> dict[str, Decimal]:
    table = TABLE_RE.search(source)
    if not table:
        raise ValueError("could not find the SCREENS threshold table")
    result: dict[str, Decimal] = {}
    for line in table.group(1).splitlines():
        if not line.strip() or line.lstrip().startswith("//"):
            continue
        row = ROW_RE.match(line)
        if not row:
            raise ValueError(f"could not parse SCREENS row: {line.strip()}")
        screen = row.group("screen")
        if screen in result:
            raise ValueError(f"duplicate SCREENS entry: {screen}")
        result[screen] = decimal(row.group("value"))
    return result


def waiver_counts(source: str) -> Counter[tuple[str, Decimal, Decimal]]:
    result: Counter[tuple[str, Decimal, Decimal]] = Counter()
    for line in source.splitlines():
        if "RATCHET-WAIVER:" not in line:
            continue
        match = WAIVER_RE.search(line)
        if not match:
            raise ValueError(f"malformed ratchet waiver: {line.strip()}")
        result[(match.group("screen"), decimal(match.group("old")), decimal(match.group("new")))] += 1
    return result


def git_show(revision: str, path: Path, *, optional: bool = False) -> str:
    result = subprocess.run(
        ["git", "show", f"{revision}:{path.as_posix()}"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode == 0:
        return result.stdout
    if optional:
        return ""
    raise RuntimeError(result.stderr.strip() or f"could not read {path} at {revision}")


def read(path: str | None, default: Path) -> str:
    return Path(path).read_text() if path else default.read_text()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="origin/main")
    parser.add_argument("--base-thresholds")
    parser.add_argument("--current-thresholds")
    parser.add_argument("--base-waivers")
    parser.add_argument("--current-waivers")
    args = parser.parse_args()

    try:
        old = thresholds(
            read(args.base_thresholds, THRESHOLDS_PATH)
            if args.base_thresholds
            else git_show(args.base, THRESHOLDS_PATH)
        )
        new = thresholds(read(args.current_thresholds, THRESHOLDS_PATH))
        old_waivers = waiver_counts(
            read(args.base_waivers, WAIVERS_PATH)
            if args.base_waivers
            else git_show(args.base, WAIVERS_PATH, optional=True)
        )
        new_waivers = waiver_counts(read(args.current_waivers, WAIVERS_PATH))
    except (OSError, RuntimeError, ValueError) as error:
        print(f"mockup ratchet check error: {error}", file=sys.stderr)
        return 2

    added_waivers = new_waivers - old_waivers
    failures = []
    for screen, new_value in new.items():
        old_value = old.get(screen)
        if old_value is None or new_value >= old_value:
            continue
        waiver = (screen, old_value, new_value)
        if added_waivers[waiver]:
            added_waivers[waiver] -= 1
            print(f"mockup ratchet waived: {screen} {old_value} -> {new_value}")
        else:
            failures.append(f"{screen} {old_value} -> {new_value}")

    if failures:
        print("mockup ratchets decreased without a newly added exact waiver:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        print(f"add RATCHET-WAIVER lines to {WAIVERS_PATH}", file=sys.stderr)
        return 1
    print("mockup ratchet check passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
