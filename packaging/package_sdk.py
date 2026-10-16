#!/usr/bin/env python3
"""Build rpt-ffi and archive the C/C++ SDK for one Rust target.

The archive contains include/rapidpdftrans.h, the C++ wrapper, the shared
library, the static library, CMake package files, and (on Unix) pkg-config.
"""

from __future__ import annotations

import argparse
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ANSI = re.compile(r"\x1b\[[0-9;]*m")

# Cross-compiles from an x86_64 Linux host. Native containers (cibuildwheel)
# must not use these linker names.
CROSS_ENV = {
    "i686-unknown-linux-gnu": {
        "CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_LINKER": "gcc",
        "CARGO_TARGET_I686_UNKNOWN_LINUX_GNU_RUSTFLAGS": "-C link-arg=-m32",
        "CC_i686_unknown_linux_gnu": "gcc",
        "CXX_i686_unknown_linux_gnu": "g++",
        "CFLAGS_i686_unknown_linux_gnu": "-m32",
        "CXXFLAGS_i686_unknown_linux_gnu": "-m32",
    },
    "armv7-unknown-linux-gnueabihf": {
        "CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER": "arm-linux-gnueabihf-gcc",
        "CC_armv7_unknown_linux_gnueabihf": "arm-linux-gnueabihf-gcc",
        "CXX_armv7_unknown_linux_gnueabihf": "arm-linux-gnueabihf-g++",
    },
}

FILE_MARKERS = {
    "x86_64-unknown-linux-gnu": "x86-64",
    "i686-unknown-linux-gnu": "80386",
    "aarch64-unknown-linux-gnu": "aarch64",
    "armv7-unknown-linux-gnueabihf": "EABI5",
    "x86_64-apple-darwin": "x86_64",
    "aarch64-apple-darwin": "arm64",
    "x86_64-pc-windows-msvc": ("x86-64", "x86_64"),
    "i686-pc-windows-msvc": ("80386", "i386"),
    "aarch64-pc-windows-msvc": ("ARM64", "Aarch64", "aarch64"),
}


def workspace_version() -> str:
    text = (ROOT / "Cargo.toml").read_text()
    match = re.search(r'(?m)^version = "([^"]+)"', text)
    if not match:
        sys.exit("Cargo.toml has no version")
    return match.group(1)


def run(cmd: list[str], *, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    print("+", " ".join(cmd), flush=True)
    return subprocess.run(cmd, cwd=ROOT, env=env, text=True, capture_output=True)


def host_target() -> str:
    probe = run(["rustc", "-vV"])
    if probe.returncode != 0:
        sys.exit(probe.stderr or probe.stdout)
    for line in probe.stdout.splitlines():
        if line.startswith("host:"):
            return line.split(":", 1)[1].strip()
    sys.exit("rustc -vV did not report a host")


def apply_cross_env(target: str, env: dict[str, str]) -> None:
    for key, value in CROSS_ENV.get(target, {}).items():
        env.setdefault(key, value)


def native_static_libs(target: str, env: dict[str, str]) -> str:
    """Link rpt-ffi and return the native libraries a static consumer must add."""
    cmd = [
        "cargo",
        "rustc",
        "-p",
        "rpt-ffi",
        "--release",
        "--locked",
        "--target",
        target,
        "--",
        "--crate-type",
        "cdylib",
        "--print",
        "native-static-libs",
    ]
    proc = run(cmd, env=env)
    output = ANSI.sub("", proc.stdout + "\n" + proc.stderr)
    if proc.returncode != 0:
        sys.stderr.write(output)
        sys.exit(f"cargo rustc failed for {target}")
    marker = "native-static-libs:"
    found = [line.split(marker, 1)[1].strip() for line in output.splitlines() if marker in line]
    if not found:
        sys.stderr.write(output[-4000:])
        sys.exit("rustc did not print native-static-libs")
    libs = found[-1]
    print("native-static-libs:", libs, flush=True)
    return libs


def artifact_dir(target: str) -> Path:
    return ROOT / "target" / target / "release"


def newest_hashed(directory: Path, pattern: str) -> Path:
    prefix, suffix = pattern.split("*", 1)
    hashed = re.compile("^" + re.escape(prefix) + r"[0-9a-f]{16}" + re.escape(suffix) + "$")
    matched = [path for path in directory.glob(pattern) if hashed.fullmatch(path.name)]
    if not matched:
        listing = sorted(path.name for path in directory.glob("*"))
        sys.exit(f"no {pattern} in {directory}; directory has {listing}")
    return max(matched, key=lambda path: path.stat().st_mtime)


def windows_machine(target: str) -> str:
    if "aarch64" in target or "arm64" in target:
        return "ARM64"
    if "i686" in target or "i586" in target:
        return "X86"
    return "X64"


def rewrite_windows_implib(dll: Path, implib: Path, target: str) -> None:
    """Cargo names the DLL rapidpdftrans-<hash>.dll inside the import library.

    The archive ships rapidpdftrans.dll, so relink the import library against
    that filename. Otherwise a consumer looks for the hashed name and the
    loader returns STATUS_DLL_NOT_FOUND.
    """
    if shutil.which("dumpbin") is None or shutil.which("lib") is None:
        sys.exit("dumpbin and lib are required to retarget the Windows import library")
    text = subprocess.check_output(["dumpbin", "/EXPORTS", str(dll)], text=True, errors="replace")
    names = []
    for line in text.splitlines():
        parts = line.split()
        if len(parts) >= 4 and parts[0].isdigit() and parts[1].isdigit() and parts[2].startswith("0"):
            names.append(parts[3])
    if not names:
        sys.exit(f"dumpbin /EXPORTS produced no symbols for {dll}:\n{text}")
    def_path = implib.with_name("rapidpdftrans.def")
    def_path.write_text(
        "LIBRARY rapidpdftrans.dll\nEXPORTS\n" + "".join(f"    {name}\n" for name in names),
        encoding="ascii",
    )
    subprocess.check_call(
        [
            "lib",
            f"/DEF:{def_path}",
            f"/OUT:{implib}",
            f"/MACHINE:{windows_machine(target)}",
            "/NOLOGO",
        ]
    )
    print(f"import library references rapidpdftrans.dll ({len(names)} exports)", flush=True)


def promote_shared(target: str, directory: Path) -> None:
    """Cargo only copies crate-types listed in Cargo.toml. The cdylib is requested
    with an extra rustc flag, so it stays under deps/ until we promote it."""
    deps = directory / "deps"
    if "windows" in target:
        dll = newest_hashed(deps, "rapidpdftrans-*.dll")
        implib = newest_hashed(deps, "rapidpdftrans-*.dll.lib")
        shutil.copy2(dll, directory / "rapidpdftrans.dll")
        out_implib = directory / "rapidpdftrans.dll.lib"
        shutil.copy2(implib, out_implib)
        rewrite_windows_implib(directory / "rapidpdftrans.dll", out_implib, target)
        return
    if "apple" in target:
        shared = newest_hashed(deps, "librapidpdftrans-*.dylib")
        shutil.copy2(shared, directory / "librapidpdftrans.dylib")
        return
    shared = newest_hashed(deps, "librapidpdftrans-*.so")
    shutil.copy2(shared, directory / "librapidpdftrans.so")


def expect(path: Path) -> Path:
    if not path.is_file():
        listing = sorted(p.name for p in path.parent.glob("*")) if path.parent.is_dir() else []
        sys.exit(f"missing {path}; directory has {listing}")
    return path


def collect_libs(target: str, directory: Path) -> dict[str, Path]:
    if "windows" in target:
        return {
            "dll": expect(directory / "rapidpdftrans.dll"),
            "implib": expect(directory / "rapidpdftrans.dll.lib"),
            "static": expect(directory / "rapidpdftrans.lib"),
        }
    if "apple" in target:
        return {
            "shared": expect(directory / "librapidpdftrans.dylib"),
            "static": expect(directory / "librapidpdftrans.a"),
        }
    return {
        "shared": expect(directory / "librapidpdftrans.so"),
        "static": expect(directory / "librapidpdftrans.a"),
    }


def assert_arch(target: str, path: Path) -> None:
    marker = FILE_MARKERS.get(target)
    if not marker or shutil.which("file") is None:
        return
    markers = marker if isinstance(marker, tuple) else (marker,)
    desc = subprocess.check_output(["file", str(path)], text=True)
    print(desc.strip(), flush=True)
    if not any(item in desc for item in markers):
        sys.exit(f"{path.name} does not look like {target} ({markers!r} not in {desc!r})")


def strip_shared(path: Path) -> None:
    system = platform.system()
    if system == "Linux":
        cmd = ["strip", "--strip-unneeded", str(path)]
    elif system == "Darwin":
        cmd = ["strip", "-x", str(path)]
    else:
        return
    if shutil.which(cmd[0]) is None:
        return
    proc = subprocess.run(cmd, text=True, capture_output=True)
    if proc.returncode != 0:
        print(f"warning: {' '.join(cmd)} failed: {proc.stderr.strip()}", flush=True)


def set_install_name(dylib: Path) -> None:
    subprocess.check_call(
        ["install_name_tool", "-id", "@rpath/librapidpdftrans.dylib", str(dylib)]
    )


def set_soname(shared: Path) -> None:
    if shutil.which("patchelf") is None:
        print("patchelf not installed; leaving the shared library SONAME as rustc wrote it")
        return
    subprocess.check_call(["patchelf", "--set-soname", "librapidpdftrans.so", str(shared)])


# MSVC import libraries that ship with the Windows SDK. Anything else ending in
# .lib (for example windows-sys's windows.0.52.0.lib) is copied into the SDK.
SYSTEM_WIN_LIBS = {
    "advapi32.lib",
    "bcrypt.lib",
    "crypt32.lib",
    "dbghelp.lib",
    "kernel32.lib",
    "ncrypt.lib",
    "ntdll.lib",
    "ole32.lib",
    "oleaut32.lib",
    "secur32.lib",
    "shell32.lib",
    "synchronization.lib",
    "userenv.lib",
    "ws2_32.lib",
    "wsock32.lib",
}


def cargo_registry_src() -> Path:
    cargo_home = os.environ.get("CARGO_HOME")
    root = Path(cargo_home) if cargo_home else Path.home() / ".cargo"
    return root / "registry" / "src"


def find_bundled_lib(name: str, artifact: Path) -> Path | None:
    roots = [artifact, artifact / "deps", artifact / "build", cargo_registry_src()]
    for root in roots:
        if not root.is_dir():
            continue
        matches = [path for path in root.rglob(name) if path.is_file()]
        if matches:
            return matches[0]
    return None


def classify_native(
    native_libs: str, artifact: Path
) -> tuple[list[str], list[str], list[tuple[Path, str]]]:
    """Split rustc's native-static-libs note into CMake link items.

    `/defaultlib:msvcrt` is a linker option. Non-system `.lib` files are not on
    the Windows SDK link path, so they are copied into the archive.
    """
    libraries: list[str] = []
    options: list[str] = []
    bundled: list[tuple[Path, str]] = []
    seen: set[str] = set()
    for token in native_libs.split():
        if token.startswith("/") or token.startswith("-Wl,"):
            options.append(token)
            continue
        if token.lower().endswith(".lib") and token.lower() not in SYSTEM_WIN_LIBS:
            if token not in seen:
                found = find_bundled_lib(token, artifact)
                if found is None:
                    sys.exit(
                        f"could not find {token} under {artifact} or {cargo_registry_src()} "
                        "to bundle into the SDK"
                    )
                bundled.append((found, token))
                seen.add(token)
            libraries.append("${PACKAGE_PREFIX_DIR}/lib/" + token)
            continue
        libraries.append(token)
    return libraries, options, bundled


def write_cmake(
    dest: Path,
    target: str,
    version: str,
    libraries: list[str],
    options: list[str],
) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    if "windows" in target:
        shared_props = """
set_target_properties(rapidpdftrans::shared PROPERTIES
  IMPORTED_LOCATION "${PACKAGE_PREFIX_DIR}/bin/rapidpdftrans.dll"
  IMPORTED_LOCATION_RELEASE "${PACKAGE_PREFIX_DIR}/bin/rapidpdftrans.dll"
  IMPORTED_LOCATION_DEBUG "${PACKAGE_PREFIX_DIR}/bin/rapidpdftrans.dll"
  IMPORTED_IMPLIB "${PACKAGE_PREFIX_DIR}/lib/rapidpdftrans.dll.lib"
  IMPORTED_IMPLIB_RELEASE "${PACKAGE_PREFIX_DIR}/lib/rapidpdftrans.dll.lib"
  IMPORTED_IMPLIB_DEBUG "${PACKAGE_PREFIX_DIR}/lib/rapidpdftrans.dll.lib"
  INTERFACE_INCLUDE_DIRECTORIES "${PACKAGE_PREFIX_DIR}/include"
)
set(_RAPIDPDFTRANS_STATIC "${PACKAGE_PREFIX_DIR}/lib/rapidpdftrans.lib")
"""
    elif "apple" in target:
        shared_props = """
set_target_properties(rapidpdftrans::shared PROPERTIES
  IMPORTED_LOCATION "${PACKAGE_PREFIX_DIR}/lib/librapidpdftrans.dylib"
  INTERFACE_INCLUDE_DIRECTORIES "${PACKAGE_PREFIX_DIR}/include"
)
set(_RAPIDPDFTRANS_STATIC "${PACKAGE_PREFIX_DIR}/lib/librapidpdftrans.a")
"""
    else:
        shared_props = """
set_target_properties(rapidpdftrans::shared PROPERTIES
  IMPORTED_LOCATION "${PACKAGE_PREFIX_DIR}/lib/librapidpdftrans.so"
  IMPORTED_SONAME "librapidpdftrans.so"
  INTERFACE_INCLUDE_DIRECTORIES "${PACKAGE_PREFIX_DIR}/include"
)
set(_RAPIDPDFTRANS_STATIC "${PACKAGE_PREFIX_DIR}/lib/librapidpdftrans.a")
"""
    link_libraries = ";".join(libraries)
    link_options = ""
    if options:
        link_options = f'\n    INTERFACE_LINK_OPTIONS "{";".join(options)}"'
    text = f"""# Generated by packaging/package_sdk.py for {target}. Do not edit.
cmake_minimum_required(VERSION 3.16)

get_filename_component(PACKAGE_PREFIX_DIR "${{CMAKE_CURRENT_LIST_DIR}}/../../.." ABSOLUTE)

if(NOT TARGET rapidpdftrans::shared)
  add_library(rapidpdftrans::shared SHARED IMPORTED)
  {shared_props}
endif()

if(NOT TARGET rapidpdftrans::static)
  add_library(rapidpdftrans::static STATIC IMPORTED)
  set_target_properties(rapidpdftrans::static PROPERTIES
    IMPORTED_LOCATION "${{_RAPIDPDFTRANS_STATIC}}"
    INTERFACE_INCLUDE_DIRECTORIES "${{PACKAGE_PREFIX_DIR}}/include"
    INTERFACE_LINK_LIBRARIES "@@LINK_LIBRARIES@@"{link_options}
  )
endif()

if(NOT TARGET rapidpdftrans::rapidpdftrans)
  add_library(rapidpdftrans::rapidpdftrans ALIAS rapidpdftrans::shared)
endif()
"""
    (dest / "rapidpdftransConfig.cmake").write_text(text.replace("@@LINK_LIBRARIES@@", link_libraries))
    (dest / "rapidpdftransConfigVersion.cmake").write_text(
        f"""set(PACKAGE_VERSION "{version}")
if(PACKAGE_VERSION VERSION_LESS PACKAGE_FIND_VERSION)
  set(PACKAGE_VERSION_COMPATIBLE FALSE)
else()
  set(PACKAGE_VERSION_COMPATIBLE TRUE)
  if(PACKAGE_FIND_VERSION STREQUAL PACKAGE_VERSION)
    set(PACKAGE_VERSION_EXACT TRUE)
  endif()
endif()
"""
    )


def write_pc(dest: Path, version: str, native_libs: str) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    (dest / "rapidpdftrans.pc").write_text(
        f"""prefix=${{pcfiledir}}/../..
libdir=${{prefix}}/lib
includedir=${{prefix}}/include

Name: rapidpdftrans
Description: C ABI for RapidPdfTrans
Version: {version}
Libs: -L${{libdir}} -lrapidpdftrans
Libs.private: {native_libs}
Cflags: -I${{includedir}}
"""
    )


def write_readme(dest: Path, version: str, target: str, native_libs: str) -> None:
    (dest / "README.txt").write_text(
        f"""RapidPdfTrans C/C++ SDK {version}
Target: {target}

include/rapidpdftrans.h     C ABI
include/rapidpdftrans.hpp    header-only C++ wrapper
lib/                         static library, shared library, import library
bin/                         Windows DLL (other platforms keep the shared library in lib/)

CMake:
  find_package(rapidpdftrans REQUIRED CONFIG)
  target_link_libraries(your_target PRIVATE rapidpdftrans::shared)
  # or rapidpdftrans::static

The static library does not include libc or the C++ runtime. Link these as well:
  {native_libs}

pkg-config (Unix):
  pkg-config --cflags --libs rapidpdftrans

The shared library's consumers need it on the loader path (LD_LIBRARY_PATH,
DYLD_LIBRARY_PATH, or an rpath). On macOS the install name is
@rpath/librapidpdftrans.dylib.
"""
    )


def stage_tree(
    target: str,
    version: str,
    libs: dict[str, Path],
    native_libs: str,
    artifact: Path,
) -> Path:
    name = f"rapidpdftrans-{version}-{target}"
    stage = ROOT / "dist" / "stage" / name
    if stage.exists():
        shutil.rmtree(stage)
    include = stage / "include"
    lib = stage / "lib"
    include.mkdir(parents=True)
    lib.mkdir(parents=True)
    shutil.copy2(ROOT / "include" / "rapidpdftrans.h", include / "rapidpdftrans.h")
    shutil.copy2(ROOT / "bindings" / "cpp" / "rapidpdftrans.hpp", include / "rapidpdftrans.hpp")
    shutil.copy2(ROOT / "LICENSE", stage / "LICENSE")

    if "windows" in target:
        bin_dir = stage / "bin"
        bin_dir.mkdir()
        shutil.copy2(libs["dll"], bin_dir / "rapidpdftrans.dll")
        shutil.copy2(libs["implib"], lib / "rapidpdftrans.dll.lib")
        shutil.copy2(libs["static"], lib / "rapidpdftrans.lib")
        assert_arch(target, bin_dir / "rapidpdftrans.dll")
    else:
        shared_name = libs["shared"].name
        static_name = libs["static"].name
        shared_out = lib / shared_name
        shutil.copy2(libs["shared"], shared_out)
        shutil.copy2(libs["static"], lib / static_name)
        if "apple" in target:
            set_install_name(shared_out)
        else:
            set_soname(shared_out)
        strip_shared(shared_out)
        assert_arch(target, shared_out)
        write_pc(lib / "pkgconfig", version, native_libs)

    libraries, options, bundled = classify_native(native_libs, artifact)
    for source, name in bundled:
        shutil.copy2(source, lib / name)
        print(f"bundled {name} from {source}", flush=True)
    write_cmake(lib / "cmake" / "rapidpdftrans", target, version, libraries, options)
    (lib / "native-static-libs.txt").write_text(native_libs + "\n")
    rustc = subprocess.check_output(["rustc", "--version"], text=True).strip()
    (stage / "BUILD.txt").write_text(
        f"version={version}\ntarget={target}\nrustc={rustc}\nnative-static-libs={native_libs}\n"
    )
    write_readme(stage, version, target, native_libs)
    return stage


def make_archive(stage: Path, target: str, out_dir: Path) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    if "windows" in target:
        archive = out_dir / f"{stage.name}.zip"
        if archive.exists():
            archive.unlink()
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as zf:
            for path in sorted(stage.rglob("*")):
                if path.is_file():
                    zf.write(path, path.relative_to(stage.parent).as_posix())
    else:
        archive = out_dir / f"{stage.name}.tar.gz"
        if archive.exists():
            archive.unlink()
        with tarfile.open(archive, "w:gz") as tf:
            tf.add(stage, arcname=stage.name)
    shutil.rmtree(stage.parent, ignore_errors=True)
    print(archive, flush=True)
    return archive


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", help="Rust target triple (default: host)")
    parser.add_argument("--out", type=Path, default=ROOT / "dist")
    args = parser.parse_args()

    target = args.target or host_target()
    env = os.environ.copy()
    apply_cross_env(target, env)
    version = workspace_version()
    native_libs = native_static_libs(target, env)
    built = artifact_dir(target)
    promote_shared(target, built)
    libs = collect_libs(target, built)
    stage = stage_tree(target, version, libs, native_libs, built)
    make_archive(stage, target, args.out.resolve())


if __name__ == "__main__":
    main()
