#!/usr/bin/env bash
# Builds the installer and publishes it as a GitHub release of this repo.
#
# The version comes from Cargo.toml ([workspace.package] version); the
# release is tagged v<version>. Bump the version first: an existing tag is
# never reused.
#
# Usage: tools/release.sh [--draft] [--dry-run]
#   --draft    create the release as a draft (publish it later on GitHub)
#   --dry-run  run the checks and build, but don't tag or publish
#
# Needs: a clean checkout of main that matches origin/main, the model
# assets (tools/fetch-assets.sh) and the GitHub CLI (gh auth login).
set -euo pipefail

DRAFT=()
DRY_RUN=0
for arg in "$@"; do
    case "$arg" in
        --draft) DRAFT=(--draft) ;;
        --dry-run) DRY_RUN=1 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TAG="v$VERSION"
SETUP="dist/ezhuthu-setup-$VERSION.exe"

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

echo "Checking formatting, lints and tests ..."
cargo fmt --all --check
cargo clippy --workspace --all-targets --quiet -- -D warnings
cargo test --workspace --quiet

tools/build-installer.sh
(cd dist && sha256sum "$(basename "$SETUP")" > "$(basename "$SETUP").sha256")

if [ "$DRY_RUN" = 1 ]; then
    echo "Dry run: built $SETUP; not tagging or publishing $TAG."
    exit 0
fi

echo "Tagging $TAG ..."
git tag -a "$TAG" -m "Ezhuthu $VERSION"
git push --quiet origin "$TAG"

echo "Publishing the release ..."
NOTES="Download **ezhuthu-setup-$VERSION.exe** and run it. It installs for the current user, with no administrator rights. Running it over an older version updates it in place and keeps your settings.

The setup isn't code-signed yet, so Windows SmartScreen may warn the first time: choose **More info > Run anyway**. To check the download, compare its SHA-256 with the \`.sha256\` file."
gh release create "$TAG" "$SETUP" "$SETUP.sha256" \
    --title "Ezhuthu $VERSION" \
    --notes "$NOTES" \
    --generate-notes \
    --latest \
    "${DRAFT[@]}"
echo "Released $TAG."
