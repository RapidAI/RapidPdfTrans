"""Smoke test for the native Python module.

Build first:

    cargo build -p rpt-python
    cp target/debug/librapidpdftrans.so rapidpdftrans.so
"""

import sys

import rapidpdftrans


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else "testdata/hello.pdf"
    if rapidpdftrans.version() != "0.1.0":
        print("unexpected version", rapidpdftrans.version())
        return 1
    text = rapidpdftrans.extract(path)
    if "Hello" not in text:
        print("extract missing Hello")
        return 1
    if "pending" not in text:
        print("coverage should start pending")
        return 1
    print("python smoke ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
