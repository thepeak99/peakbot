#!/bin/sh
#
# check-version.sh — verify that the desktop workspace version matches the
# root peakbot version. Per DESIGN.md §5.7 / P0.3:
#   desktop/Cargo.toml [workspace.package].version == root Cargo.toml [package].version
#
# Exit 0 when they match; exit 1 with a message on stderr when they differ
# (or a version cannot be read).
#
# This script lives at <root>/desktop/scripts/. It resolves the repo root
# relative to its own path so it works regardless of the current directory.

set -eu

# <root>/desktop/scripts/check-version.sh  ->  <root>
script_dir=$(cd "$(dirname "$0")" >/dev/null 2>&1 && pwd)
root=$(cd "$script_dir/../.." >/dev/null 2>&1 && pwd)

# Extract the first `version = "<v>"` line inside the named section of a TOML
# file. Args: $1 = section name (e.g. package, workspace.package),
#            $2 = file path.
version_of() {
    awk -v sec="$1" '
        insec && $0 ~ /^\[/      { insec = 0; next }
        insec && $0 ~ /^version[[:space:]]*=/ {
            v = $0
            sub(/^[^=]*=[[:space:]]*"/, "", v)   # drop through the opening quote
            sub(/".*$/, "", v)                    # drop closing quote + any trailing comment
            print v
            exit
        }
        $0 == "[" sec "]"        { insec = 1 }
    ' "$2"
}

root_ver=$(version_of "package" "$root/Cargo.toml")
desk_ver=$(version_of "workspace.package" "$root/desktop/Cargo.toml")

if [ -z "$root_ver" ]; then
    echo "check-version.sh: could not read version from root Cargo.toml ($root/Cargo.toml)" >&2
    exit 1
fi
if [ -z "$desk_ver" ]; then
    echo "check-version.sh: could not read version from desktop/Cargo.toml [workspace.package] ($root/desktop/Cargo.toml)" >&2
    exit 1
fi

if [ "$root_ver" != "$desk_ver" ]; then
    echo "check-version.sh: version mismatch — root Cargo.toml is $root_ver, desktop/Cargo.toml is $desk_ver" >&2
    exit 1
fi

echo "check-version.sh: OK (root=$root_ver desktop=$desk_ver)"
exit 0
