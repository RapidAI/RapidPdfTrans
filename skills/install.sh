#!/usr/bin/env bash
# Install the RapidPdfTrans agent skill.
#
#   curl -fsSL https://raw.githubusercontent.com/RapidAI/RapidPdfTrans/main/skills/install.sh | bash
#   curl -fsSL .../skills/install.sh | bash -s -- --claude --with-cli
#
# Options:
#   --cursor        ~/.cursor/skills/rapidpdftrans (default)
#   --claude        ~/.claude/skills/rapidpdftrans
#   --all           both of the above
#   --dir PATH      install into PATH/rapidpdftrans instead
#   --with-cli      also put the prebuilt `rpt` binary in ~/.local/bin (Linux x86_64/aarch64)
#
# Only <skills dir>/rapidpdftrans/SKILL.md is written. Re-running replaces that
# file and nothing else. RPT_SKILL_REF picks the git ref (default main).
set -euo pipefail

REPO="RapidAI/RapidPdfTrans"
REF="${RPT_SKILL_REF:-main}"
RAW="https://raw.githubusercontent.com/${REPO}/${REF}/skills/rapidpdftrans/SKILL.md"

dirs=()
with_cli=0
while [ $# -gt 0 ]; do
  case "$1" in
    --cursor) dirs+=("$HOME/.cursor/skills") ;;
    --claude) dirs+=("$HOME/.claude/skills") ;;
    --all) dirs+=("$HOME/.cursor/skills" "$HOME/.claude/skills") ;;
    --dir) shift; [ $# -gt 0 ] || { echo "install.sh: --dir needs a path" >&2; exit 2; }; dirs+=("$1") ;;
    --with-cli) with_cli=1 ;;
    -h|--help) sed -n '2,17p' "${BASH_SOURCE[0]:-/dev/null}" 2>/dev/null || true; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
  shift
done
[ ${#dirs[@]} -gt 0 ] || dirs=("$HOME/.cursor/skills")

fetch() { # url dest
  if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then wget -qO "$2" "$1"
  else echo "install.sh: need curl or wget" >&2; exit 1; fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# From a checkout, use the local file; when piped from curl, download it.
src=""
here="${BASH_SOURCE[0]:-}"
if [ -n "$here" ] && [ -f "$here" ]; then
  local_skill="$(cd "$(dirname "$here")" && pwd)/rapidpdftrans/SKILL.md"
  [ -f "$local_skill" ] && src="$local_skill"
fi
if [ -z "$src" ]; then
  fetch "$RAW" "$tmp/SKILL.md"
  src="$tmp/SKILL.md"
fi
head -1 "$src" | grep -q '^---$' || { echo "install.sh: downloaded SKILL.md looks wrong" >&2; exit 1; }

for base in "${dirs[@]}"; do
  dest="$base/rapidpdftrans"
  mkdir -p "$dest"
  cp "$src" "$dest/SKILL.md"
  echo "installed skill: $dest/SKILL.md"
done

if [ "$with_cli" = 1 ]; then
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) asset=rpt-linux-x86_64.tar.gz ;;
    Linux-aarch64|Linux-arm64) asset=rpt-linux-aarch64.tar.gz ;;
    *) echo "install.sh: no prebuilt rpt for $(uname -s)-$(uname -m); build it with: cargo build -p rpt-cli --release" >&2; exit 1 ;;
  esac
  fetch "https://github.com/${REPO}/releases/latest/download/${asset}" "$tmp/$asset"
  mkdir -p "$HOME/.local/bin"
  tar -xzf "$tmp/$asset" -C "$tmp" rpt
  install -m 0755 "$tmp/rpt" "$HOME/.local/bin/rpt"
  echo "installed cli: $HOME/.local/bin/rpt ($("$HOME/.local/bin/rpt" --version 2>/dev/null || echo rpt))"
  case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "note: add ~/.local/bin to PATH" ;; esac
fi

echo "LLM settings are read from OPENAI_API_KEY, OPENAI_BASE_URL and OPENAI_MODEL (or RPT_LLM_*)."
