#!/usr/bin/env python3
"""Configure, build, and run the SDK smoke test against one archive."""

from __future__ import annotations

import argparse
import os
import shlex
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def extract(archive: Path, dest: Path) -> Path:
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as zf:
            zf.extractall(dest)
    else:
        with tarfile.open(archive) as tf:
            tf.extractall(dest)
    matches = list(dest.rglob("rapidpdftransConfig.cmake"))
    if len(matches) != 1:
        sys.exit(f"expected one CMake config in {archive}, found {matches}")
    # lib/cmake/rapidpdftrans/rapidpdftransConfig.cmake -> SDK root
    return matches[0].parents[3]


def find_exe(build: Path, name: str) -> Path:
    wanted = name if name.endswith(".exe") or os.name != "nt" else f"{name}.exe"
    matches = [path for path in build.rglob(wanted) if path.is_file() and os.access(path, os.X_OK)]
    if not matches:
        sys.exit(f"did not find {wanted} under {build}")
    return matches[0]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dist", type=Path, required=True)
    parser.add_argument("--pdf", type=Path, required=True)
    parser.add_argument("--cxx", default="")
    parser.add_argument("--cflags", default="")
    parser.add_argument("--emulator", default="")
    parser.add_argument("--cmake-args", default="")
    args = parser.parse_args()

    archives = sorted(
        path
        for path in args.dist.iterdir()
        if path.is_file() and (path.name.endswith(".tar.gz") or path.suffix == ".zip")
    )
    if len(archives) != 1:
        sys.exit(f"expected one SDK archive in {args.dist}, found {archives}")

    pdf = args.pdf.resolve()
    if not pdf.is_file():
        sys.exit(f"missing pdf {pdf}")

    with tempfile.TemporaryDirectory(prefix="rpt-sdk-smoke-") as tmp:
        root = extract(archives[0], Path(tmp) / "sdk")
        build = Path(tmp) / "build"
        cmake = [
            "cmake",
            "-S",
            str(ROOT / "packaging" / "smoke"),
            "-B",
            str(build),
            "-DCMAKE_BUILD_TYPE=Release",
            f"-DRPT_SDK_ROOT={root}",
        ]
        if args.cxx:
            cmake.append(f"-DCMAKE_CXX_COMPILER={args.cxx}")
        if args.cflags:
            cmake.append(f"-DCMAKE_CXX_FLAGS={args.cflags}")
            cmake.append(f"-DCMAKE_EXE_LINKER_FLAGS={args.cflags}")
        if args.cmake_args:
            cmake.extend(shlex.split(args.cmake_args))
        print("+", " ".join(cmake), flush=True)
        subprocess.check_call(cmake)
        subprocess.check_call(["cmake", "--build", str(build), "--config", "Release", "--parallel"])

        env = os.environ.copy()
        env.pop("RPT_LLM_API_KEY", None)
        lib = str(root / "lib")
        bin_dir = str(root / "bin")
        env["LD_LIBRARY_PATH"] = lib + os.pathsep + env.get("LD_LIBRARY_PATH", "")
        env["DYLD_LIBRARY_PATH"] = lib + os.pathsep + env.get("DYLD_LIBRARY_PATH", "")
        env["PATH"] = bin_dir + os.pathsep + env.get("PATH", "")
        emulator = shlex.split(args.emulator)

        for name in ("smoke_shared", "smoke_static"):
            exe = find_exe(build, name)
            cmd = [*emulator, str(exe), str(pdf)]
            print("+", " ".join(cmd), flush=True)
            subprocess.check_call(cmd, env=env)
    print("sdk smoke ok", flush=True)


if __name__ == "__main__":
    main()
