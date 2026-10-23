#!/usr/bin/env bash
# Starts a release: tags main as v<version> and pushes the tag. GitHub Actions
# (.github/workflows/release.yml) then builds the installer and publishes the
# GitHub release.
#
# The version comes from Cargo.toml ([workspace.package] version). Bump it
# first: an existing tag is never reused.
#
# Usage: tools/release.sh [--dry-run]
#   --dry-run  run the checks only; don't tag or push
#
# Needs: a clean checkout of main that matches origin/main.
set -euo pipefail

DRY_RUN=0
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TAG="v$VERSION"

fail() { echo "error: $*" >&2; exit 1; }

echo "Checking the checkout ..."
[ "$(git branch --show-current)" = main ] || fail "check out main first"
[ -z "$(git status --porcelain)" ] || fail "commit or stash your changes first"
git fetch --quiet --tags origin
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] \
    || fail "main doesn't match origin/main (git pull first)"
if git rev-parse --quiet --verify "refs/tags/$TAG" >/dev/null; then
    fail "$TAG already exists; bump the version in Cargo.toml"
fi

if [ "$DRY_RUN" = 1 ]; then
    echo "Dry run: ready to tag $TAG; not tagging or pushing."
    exit 0
fi

echo "Tagging $TAG ..."
git tag -a "$TAG" -m "Ezhuthu $VERSION"
git push --quiet origin "$TAG"
echo "Pushed $TAG. GitHub Actions is building the release:"
echo "  https://github.com/srisar/ezhuthu/actions/workflows/release.yml"
