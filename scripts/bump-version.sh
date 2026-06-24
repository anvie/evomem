#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_ROOT"

CARGO_TOML="Cargo.toml"

# ---- helpers ---------------------------------------------------------------

die() { echo "error: $*" >&2; exit 1; }

# ---- current version -------------------------------------------------------

current_version="$(grep '^version' "$CARGO_TOML" | sed 's/version = "\(.*\)"/\1/')"
[[ -n "$current_version" ]] || die "unable to read version from $CARGO_TOML"

# ---- parse new version -----------------------------------------------------

bump_type_or_version="${1:-}"
[[ -n "$bump_type_or_version" ]] || die "usage: $0 <major|minor|patch|<semver> [--push]"

IFS='.' read -r major minor patch <<< "$current_version"

case "$bump_type_or_version" in
  major) new_version="$((major + 1)).0.0" ;;
  minor) new_version="$major.$((minor + 1)).0" ;;
  patch) new_version="$major.$minor.$((patch + 1))" ;;
  *)
    # Strip leading v if present
    new_version="${bump_type_or_version#v}"
    # Validate semver-ish
    [[ "$new_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]] || die "invalid version: $bump_type_or_version (expected semver like 0.4.0)"
    ;;
esac

# Strip 'v' prefix for storage in Cargo.toml
[[ "$new_version" == v* ]] && new_version="${new_version#v}"

echo "Current version: v$current_version"
echo "  New version:   v$new_version"
echo ""

# ---- check working tree ----------------------------------------------------

# Check for uncommitted changes to tracked files only (untracked is OK)
[[ -z "$(git status --porcelain --untracked-files=no)" ]] || die "working tree has uncommitted changes; commit or stash them first"

# ---- update Cargo.toml ------------------------------------------------------

echo "Updating $CARGO_TOML ..."
sed -i '' "s/^version = \"$current_version\"/version = \"$new_version\"/" "$CARGO_TOML"

# Verify
grep -q "version = \"$new_version\"" "$CARGO_TOML" || die "failed to update version in $CARGO_TOML"

# ---- verify compilation ----------------------------------------------------

echo "Running cargo check ..."
cargo check --quiet 2>/dev/null || die "cargo check failed after version bump"

# ---- commit & tag ----------------------------------------------------------

git add "$CARGO_TOML" Cargo.lock 2>/dev/null || true

git commit -m "chore: bump version to v$new_version"
git tag -a "v$new_version" -m "v$new_version"

echo ""
echo "Committed and tagged v$new_version."

# ---- optional push ---------------------------------------------------------

if [[ "${2:-}" == "--push" || "${1:-}" == "--push" ]]; then
  echo "Pushing ..."
  git push origin main --tags
  echo "Done."
fi
