#!/bin/sh
# Attach dist/* archives or wheels to the GitHub Release for the current tag.
# The tag must be v<workspace.package.version>. Safe to run from more than one
# workflow: create is retried, and upload uses --clobber.
set -eu

dist=${1:?dist directory}
checksum_name=${2:?checksum filename}

version=$(python3 - <<'PY'
import pathlib, re, sys
text = pathlib.Path("Cargo.toml").read_text()
match = re.search(r'(?m)^version = "([^"]+)"', text)
if not match:
    sys.exit("Cargo.toml has no version")
print(match.group(1))
PY
)

tag="v${version}"
ref=${GITHUB_REF_NAME:?}
if [ "$ref" != "$tag" ]; then
  echo "ref ${ref} does not match Cargo.toml version ${tag}" >&2
  exit 1
fi

files=$(find "$dist" -type f \( -name '*.whl' -o -name '*.tar.gz' -o -name '*.zip' \) | sort)
if [ -z "$files" ]; then
  echo "no release files in ${dist}" >&2
  exit 1
fi

checksum="${dist}/${checksum_name}"
# shellcheck disable=SC2086
sha256sum $files > "$checksum"

created=0
i=1
while [ "$i" -le 5 ]; do
  if gh release view "$tag" >/dev/null 2>&1; then
    created=1
    break
  fi
  if gh release create "$tag" --verify-tag --generate-notes --title "$tag"; then
    created=1
    break
  fi
  sleep $((i * 5))
  i=$((i + 1))
done
if [ "$created" -ne 1 ]; then
  echo "could not create or see release ${tag}" >&2
  exit 1
fi

# shellcheck disable=SC2086
gh release upload "$tag" $files "$checksum" --clobber
echo "uploaded to ${tag}:"
echo "$files"
