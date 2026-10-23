#!/usr/bin/env bash
# Builds dist/ezhuthu-setup-<version>.exe: the release app plus the model
# assets, embedded in a single per-user installer.
#
# Usage: tools/build-installer.sh   (run tools/fetch-assets.sh first if assets/ is empty)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "Checking assets ..."
(cd assets && sha256sum --check --strict --quiet ../tools/assets.sha256)

echo "Building the app (release, LTO; this takes several minutes) ..."
cargo build --release -p ezhuthu

echo "Building the installer ..."
EZHUTHU_REQUIRE_PAYLOAD=1 cargo build --release -p ezhuthu-setup

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
mkdir -p dist
OUT="dist/ezhuthu-setup-$VERSION.exe"
cp target/release/ezhuthu-setup.exe "$OUT"
ls -l "$OUT"
