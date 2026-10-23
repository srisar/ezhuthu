#!/usr/bin/env bash
# Download the model assets from the GitHub Release into assets/ and verify
# them against tools/assets.sha256. Needs the GitHub CLI.
#
# Usage: tools/fetch-assets.sh [release-tag]   (default: the tag below)
set -euo pipefail

TAG="${1:-assets-v1}"
REPO="srisar/ezhuthu"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SUMS="$ROOT/tools/assets.sha256"

mkdir -p "$ROOT/assets"
cd "$ROOT/assets"

# Download only the files listed in the checksum file.
patterns=()
while read -r _ name; do
    patterns+=(--pattern "${name#\*}")  # sha256sum marks binary mode with '*'
done < "$SUMS"

echo "Downloading $TAG from $REPO ..."
gh release download "$TAG" --repo "$REPO" --clobber "${patterns[@]}"

echo "Verifying checksums ..."
sha256sum --check --strict "$SUMS"
echo "Assets ready in $ROOT/assets"
