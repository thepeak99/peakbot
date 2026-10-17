#!/bin/sh
#
# smoke-linux.sh <AppImage> <version>
#
# Artifact smoke checks S-L1..S-L8 (desktop/DESIGN.md §9.2). Runs inside the
# build container (file, objdump, readelf are all present there). Exits
# non-zero on the first failed check; the build fails with it.

set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: smoke-linux.sh <AppImage> <version>" >&2
    exit 2
fi

APPIMAGE=$1
VERSION=$2

fail() { echo "SMOKE FAIL: $*" >&2; exit 1; }
pass() { echo "SMOKE PASS: $*"; }

# ── S-L1: file exists, mode has x, is an ELF 64-bit x86-64 executable ──
[ -f "$APPIMAGE" ] || fail "S-L1: AppImage not found: $APPIMAGE"
[ -x "$APPIMAGE" ] || fail "S-L1: AppImage is not executable: $APPIMAGE"
file_out=$(file "$APPIMAGE")
case "$file_out" in
    *"ELF 64-bit LSB executable, x86-64"*) ;;
    *"ELF 64-bit LSB pie executable, x86-64"*) ;;
    *) fail "S-L1: not an ELF 64-bit LSB x86-64 executable: $file_out" ;;
esac
pass "S-L1: ELF 64-bit LSB executable, x86-64"

# ── S-L2: --appimage-extract into a temp dir succeeds (no FUSE) ──
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
(cd "$tmp" && "$APPIMAGE" --appimage-extract >/dev/null) \
    || fail "S-L2: --appimage-extract failed"
[ -d "$tmp/squashfs-root" ] || fail "S-L2: squashfs-root/ missing after extract"
pass "S-L2: --appimage-extract (no FUSE)"

R="$tmp/squashfs-root"

# ── S-L3: both binaries present and executable ──
[ -x "$R/usr/bin/peakbot-desktop" ] \
    || fail "S-L3: usr/bin/peakbot-desktop missing or not executable"
[ -x "$R/usr/bin/peakbot" ] \
    || fail "S-L3: usr/bin/peakbot missing or not executable"
pass "S-L3: usr/bin/peakbot-desktop + usr/bin/peakbot present, executable"

# ── S-L4: sidecar --version prints exactly 'peakbot <version>' ──
ver_out=$("$R/usr/bin/peakbot" --version) \
    || fail "S-L4: sidecar --version exited non-zero"
[ "$ver_out" = "peakbot $VERSION" ] \
    || fail "S-L4: sidecar version mismatch: got '$ver_out', want 'peakbot $VERSION'"
pass "S-L4: sidecar reports 'peakbot $VERSION'"

# ── S-L5: a .desktop file at the squashfs root with Exec/Name ──
set -- "$R"/*.desktop
[ -e "$1" ] || fail "S-L5: no .desktop file at the AppImage root"
[ "$#" -eq 1 ] || fail "S-L5: expected exactly one .desktop file at the root, got: $*"
grep -q '^Exec=peakbot-desktop$' "$1" \
    || fail "S-L5: $1 missing 'Exec=peakbot-desktop'"
grep -q '^Name=PeakBot Desktop$' "$1" \
    || fail "S-L5: $1 missing 'Name=PeakBot Desktop'"
pass "S-L5: $1 has Exec=peakbot-desktop and Name=PeakBot Desktop"

# ── S-L6: sidecar's highest GLIBC symbol version is <= GLIBC_2.34 ──
max_glibc=$(objdump -T "$R/usr/bin/peakbot" | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1)
[ -n "$max_glibc" ] || fail "S-L6: no GLIBC_* dynamic symbols found in usr/bin/peakbot"
if [ "$(printf '%s\n%s\n' "$max_glibc" 'GLIBC_2.34' | sort -V | tail -1)" != "GLIBC_2.34" ]; then
    fail "S-L6: sidecar requires $max_glibc (above the GLIBC_2.34 floor)"
fi
pass "S-L6: glibc floor $max_glibc (<= GLIBC_2.34)"

# ── S-L7: desktop binary links the webkit2gtk-4.1 ABI ──
readelf -d "$R/usr/bin/peakbot-desktop" | grep -E 'NEEDED.*libwebkit2gtk-4\.1' \
    >/dev/null || fail "S-L7: peakbot-desktop does not link libwebkit2gtk-4.1"
pass "S-L7: peakbot-desktop links libwebkit2gtk-4.1"

# ── S-L8: bundled CBDT emoji font + OFL present, sha256 ok (DESIGN §4.12) ──
sh "$(dirname "$0")/fetch-emoji-font.sh" --verify "$R/usr/share/peakbot-desktop/fonts" \
    || fail "S-L8: bundled CBDT emoji font missing or sha256 mismatch in usr/share/peakbot-desktop/fonts"
pass "S-L8: bundled CBDT emoji font + OFL present, sha256 ok"

echo "smoke-linux: all checks passed (v$VERSION)"
