#!/bin/sh
set -e

# Release bisque-tools-cli via git subtree push
#
# Usage:
#   ./bisque-tools-cli/release.sh v0.2.0
#
# This pushes the bisque-tools-cli/ subtree to the separate
# siderakis/bisque-tools-cli repo, then tags the release.
# The v* tag triggers the GitHub Actions release workflow
# which builds binaries for all platforms.

REMOTE_NAME="bisque-cli"
REMOTE_URL="https://github.com/siderakis/bisque-tools-cli.git"
PREFIX="bisque-tools-cli"

VERSION="$1"
if [ -z "$VERSION" ]; then
  echo "Usage: $0 <version>" >&2
  echo "  e.g. $0 v0.2.0" >&2
  exit 1
fi

# Ensure version starts with 'v'
case "$VERSION" in
  v*) ;;
  *)  VERSION="v$VERSION" ;;
esac

# Ensure we're at the repo root
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

# Add remote if it doesn't exist
if ! git remote get-url "$REMOTE_NAME" >/dev/null 2>&1; then
  echo "Adding remote $REMOTE_NAME → $REMOTE_URL"
  git remote add "$REMOTE_NAME" "$REMOTE_URL"
fi

# Update Cargo.toml version to match the tag
CARGO_VERSION="${VERSION#v}"
sed -i '' "s/^version = \".*\"/version = \"$CARGO_VERSION\"/" "$PREFIX/Cargo.toml"

# If version changed, commit it
if ! git diff --quiet "$PREFIX/Cargo.toml"; then
  git add "$PREFIX/Cargo.toml"
  git commit -m "Bump bisque-tools-cli version to $CARGO_VERSION"
  echo "Committed version bump to $CARGO_VERSION"
fi

echo "Pushing subtree $PREFIX → $REMOTE_NAME main..."
# Force-push because subtree split generates new commit SHAs that may
# diverge from the remote. The CLI repo is a pure mirror of the monorepo
# subtree, so force-push is safe here.
git push "$REMOTE_NAME" "$(git subtree split --prefix="$PREFIX"):main" --force

echo "Tagging $VERSION on $REMOTE_NAME..."
# We need to tag on the remote repo's commit, not the monorepo's
# Fetch the remote, find the latest commit, and tag it
git fetch "$REMOTE_NAME" main
REMOTE_HEAD="$(git rev-parse "$REMOTE_NAME/main")"
git tag "$VERSION" "$REMOTE_HEAD"
git push "$REMOTE_NAME" "$VERSION"

echo ""
echo "Done! Release $VERSION pushed to $REMOTE_URL"
echo "GitHub Actions will build binaries automatically."
echo "Track progress: https://github.com/siderakis/bisque-tools-cli/actions"
