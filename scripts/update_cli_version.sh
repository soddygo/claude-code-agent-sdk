#!/usr/bin/env bash
set -euo pipefail

# Update CLI_VERSION in src/version.rs.
# Usage: ./scripts/update_cli_version.sh <new_version>
#
# Example:
#   ./scripts/update_cli_version.sh 2.1.39

if [ $# -ne 1 ]; then
    echo "Usage: $0 <new_version>" >&2
    echo "Example: $0 2.1.39" >&2
    exit 1
fi

NEW_VERSION="$1"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION_FILE="$(dirname "$SCRIPT_DIR")/src/version.rs"

if [ ! -f "$VERSION_FILE" ]; then
    echo "ERROR: $VERSION_FILE not found" >&2
    exit 1
fi

# Validate version format (x.y.z)
if ! echo "$NEW_VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "ERROR: Invalid version format '$NEW_VERSION'. Expected: x.y.z" >&2
    exit 1
fi

OLD_VERSION=$(grep -m1 'pub const CLI_VERSION:' "$VERSION_FILE" | sed 's/.*"\(.*\)".*/\1/')
if [ -z "$OLD_VERSION" ]; then
    echo "ERROR: Could not parse current CLI_VERSION from $VERSION_FILE" >&2
    exit 1
fi

sed -i.bak "s/pub const CLI_VERSION: &str = \"[^\"]*\"/pub const CLI_VERSION: \&str = \"$NEW_VERSION\"/" "$VERSION_FILE"
rm -f "${VERSION_FILE}.bak"

echo "Updated CLI_VERSION: $OLD_VERSION -> $NEW_VERSION"
