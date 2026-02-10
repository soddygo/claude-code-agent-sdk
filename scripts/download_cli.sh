#!/usr/bin/env bash
set -euo pipefail

# Download the Claude Code CLI to the bundled directory.
# Usage: ./scripts/download_cli.sh
#
# This script reads CLI_VERSION from src/version.rs and downloads
# the CLI binary to ~/.claude/sdk/bundled/{version}/claude.
# Useful for CI environments or pre-downloading before builds.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

VERSION=$(grep -m1 'pub const CLI_VERSION:' "$PROJECT_DIR/src/version.rs" | sed 's/.*"\(.*\)".*/\1/')
if [ -z "$VERSION" ]; then
    echo "ERROR: Could not parse CLI_VERSION from src/version.rs" >&2
    exit 1
fi

TARGET_DIR="$HOME/.claude/sdk/bundled/$VERSION"
TARGET_CLI="$TARGET_DIR/claude"

if [ -f "$TARGET_CLI" ]; then
    echo "CLI v$VERSION already exists at $TARGET_CLI"
    exit 0
fi

echo "Downloading Claude Code CLI v$VERSION..."
mkdir -p "$TARGET_DIR"

# Backup existing CLI binary to avoid overwriting user's installation.
# install.sh always installs to ~/.local/bin/claude, which may differ
# from the version we are pinning.
DEFAULT_CLI="$HOME/.local/bin/claude"
BACKUP_CLI="$HOME/.local/bin/.claude.sdk-backup"
HAD_EXISTING=false
if [ -f "$DEFAULT_CLI" ]; then
    HAD_EXISTING=true
    cp "$DEFAULT_CLI" "$BACKUP_CLI"
fi

restore_backup() {
    if [ "$HAD_EXISTING" = true ] && [ -f "$BACKUP_CLI" ]; then
        mv "$BACKUP_CLI" "$DEFAULT_CLI"
    fi
    rm -f "$BACKUP_CLI"
}
trap restore_backup EXIT

# Download using the official install script with pinned version
curl -fsSL https://claude.ai/install.sh | bash -s -- "$VERSION"

# Find the installed binary and copy to bundled directory atomically
INSTALLED_CLI=""
if [ -f "$HOME/.local/bin/claude" ]; then
    INSTALLED_CLI="$HOME/.local/bin/claude"
elif command -v claude >/dev/null 2>&1; then
    INSTALLED_CLI="$(command -v claude)"
else
    echo "ERROR: Could not find installed CLI binary after install.sh" >&2
    exit 1
fi

# Atomic write: copy to temp file, then rename
TMP_CLI="$TARGET_DIR/.claude.tmp"
cp "$INSTALLED_CLI" "$TMP_CLI"
chmod +x "$TMP_CLI"
mv "$TMP_CLI" "$TARGET_CLI"

echo "CLI v$VERSION downloaded to $TARGET_CLI"
