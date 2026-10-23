#!/usr/bin/env python3
"""Write workspace package versions in Cargo.lock from Cargo.toml.

`cargo --locked` refuses to build when the manifest version moved and the
lock still names the previous one. v0.1.4 is that case: Cargo.toml says
0.1.4 and the five workspace packages in Cargo.lock still say 0.1.3.
Dependency versions and checksums are left untouched.
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path


def workspace_packages(root: Path) -> tuple[str, set[str]]:
    cargo = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    version = cargo["workspace"]["package"]["version"]
    names: set[str] = set()
    for member in cargo["workspace"]["members"]:
        manifest = tomllib.loads((root / member / "Cargo.toml").read_text(encoding="utf-8"))
        names.add(manifest["package"]["name"])
    if not names:
        raise SystemExit("workspace has no members")
    return version, names


def align(lock_text: str, version: str, names: set[str]) -> tuple[str, list[str]]:
    lines = lock_text.splitlines(keepends=True)
    changes: list[str] = []
    for index, line in enumerate(lines):
        if not line.startswith("name = "):
            continue
        previous = lines[index - 1].strip() if index else ""
        if previous != "[[package]]":
            continue
        name = line.split("=", 1)[1].strip().strip('"')
        if name not in names:
            continue
        version_line = lines[index + 1]
        if not version_line.startswith("version = "):
            raise SystemExit(f"{name} has no version line after its name")
        current = version_line.split("=", 1)[1].strip().strip('"')
        if current == version:
            continue
        newline = "\n" if version_line.endswith("\n") else ""
        lines[index + 1] = f'version = "{version}"{newline}'
        changes.append(f"{name} {current} -> {version}")
    return "".join(lines), changes


def main() -> int:
    root = Path.cwd()
    version, names = workspace_packages(root)
    path = root / "Cargo.lock"
    original = path.read_text(encoding="utf-8")
    updated, changes = align(original, version, names)
    if not changes:
        print(f"Cargo.lock already records workspace version {version}")
        return 0
    path.write_text(updated, encoding="utf-8")
    print(f"aligned Cargo.lock to workspace version {version}:")
    for change in changes:
        print(f"  {change}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
