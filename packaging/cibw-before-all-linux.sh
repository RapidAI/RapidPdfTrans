#!/bin/sh
# Install a host Rust toolchain inside the manylinux/musllinux container.
# Symlinks avoid relying on $HOME expansion in later cibuildwheel steps.
set -eu

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required to install rustup" >&2
  exit 1
fi

if [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable
fi

mkdir -p /usr/local/bin
for tool in cargo rustc rustdoc rustup; do
  ln -sfn "$HOME/.cargo/bin/$tool" "/usr/local/bin/$tool"
done

rustc --version
cargo --version
