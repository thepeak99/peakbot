#!/bin/sh
# Compute the Google Play versionCode from a version tag.
# Formula: (MAJOR*1e6 + MINOR*1e3 + PATCH) * 100 + slot
#   slot = 99 for stable, N for -rc.N (N in 0..98)
# Exit 0 and print the code on success; exit 2 on invalid input (empty stdout).

set -eu

ver="${1:-}"

# Validate: must be X.Y.Z or X.Y.Z-rc.N with no leading zeros, no 'v' prefix
if ! printf '%s' "$ver" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$'; then
  exit 2
fi

# Split into components
base="${ver%%-rc.*}"
major="${base%%.*}"
rest="${base#*.}"
minor="${rest%%.*}"
patch="${rest#*.}"

# Leading zeros are invalid (01, 001, etc.)
case "$major" in 0) ;; 0*) exit 2 ;; esac
case "$minor" in 0) ;; 0*) exit 2 ;; esac
case "$patch" in 0) ;; 0*) exit 2 ;; esac

# Range checks: MAJOR < 21, MINOR < 1000, PATCH < 1000
if [ "$major" -ge 21 ]; then exit 2; fi
if [ "$minor" -ge 1000 ]; then exit 2; fi
if [ "$patch" -ge 1000 ]; then exit 2; fi

# Version must be at least 0.1.0 (0.0.x is development, not a release)
if [ "$major" -eq 0 ] && [ "$minor" -eq 0 ]; then exit 2; fi

# Compute base version code
base_code=$(( (major * 1000000 + minor * 1000 + patch) * 100 ))

# Determine slot
if printf '%s' "$ver" | grep -q -- '-rc.'; then
  rc="${ver##*-rc.}"
  if [ "$rc" -ge 99 ]; then exit 2; fi
  slot=$rc
else
  slot=99
fi

echo $(( base_code + slot ))