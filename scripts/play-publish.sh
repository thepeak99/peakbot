#!/bin/sh
# Publish an AAB to the Google Play INTERNAL track via the Play Developer API.
#
# Usage: play-publish.sh <package> <aab-path> <versionCode>
# Env:   PLAY_SERVICE_ACCOUNT_JSON  service-account key JSON content (required)
#        PLAY_RELEASE_STATUS        draft (default) | completed
# Exit:  0 published, or versionCode already on Play and the track updated
#        1 API / network failure
#        2 usage or precondition error (zero network calls)
#
# Secrets (private key, JWT, access token) never appear in argv or output:
# they live in 0600 files inside a 0700 mktemp dir that a trap removes on
# every exit path; curl reads them through `-H @file` / `-d @file`.
# POSIX sh + curl + jq + openssl only.

set -eu

API="https://androidpublisher.googleapis.com/androidpublisher/v3"
UPLOAD_API="https://androidpublisher.googleapis.com/upload/androidpublisher/v3"

die2() { echo "❌ $1"; exit 2; }

# ── Preconditions (no network) ──────────────────────────────────────────────
[ $# -eq 3 ] || die2 "usage: play-publish.sh <package> <aab-path> <versionCode>"
pkg=$1
aab=$2
vc=$3

[ -f "$aab" ] && [ -r "$aab" ] || die2 "AAB not found or unreadable: $aab"

case "$vc" in
  ''|*[!0-9]*|0*) die2 "versionCode must be a positive integer, got '$vc'" ;;
esac

status=${PLAY_RELEASE_STATUS:-draft}
case "$status" in
  draft|completed) ;;
  *) die2 "PLAY_RELEASE_STATUS must be 'draft' or 'completed', got '$status'" ;;
esac

for tool in curl jq openssl; do
  command -v "$tool" >/dev/null 2>&1 || die2 "'$tool' is required"
done

[ -n "${PLAY_SERVICE_ACCOUNT_JSON:-}" ] || die2 "PLAY_SERVICE_ACCOUNT_JSON is empty"
printf '%s' "$PLAY_SERVICE_ACCOUNT_JSON" | jq -e . >/dev/null 2>&1 \
  || die2 "PLAY_SERVICE_ACCOUNT_JSON is not valid JSON"

# A real key file stores the PEM newlines as \n escapes; jq -r decodes them.
sa_field() {
  printf '%s' "$PLAY_SERVICE_ACCOUNT_JSON" | jq -r --arg f "$1" '.[$f] // empty | strings'
}
client_email=$(sa_field client_email)
private_key=$(sa_field private_key)
token_uri=$(sa_field token_uri)
[ -n "$client_email" ] || die2 "PLAY_SERVICE_ACCOUNT_JSON has no client_email"
[ -n "$private_key" ]  || die2 "PLAY_SERVICE_ACCOUNT_JSON has no private_key"
[ -n "$token_uri" ]    || die2 "PLAY_SERVICE_ACCOUNT_JSON has no token_uri"

# ── Secret workspace ────────────────────────────────────────────────────────
umask 077
tmp=$(mktemp -d "${TMPDIR:-/tmp}/play-publish.XXXXXX")
chmod 700 "$tmp"
trap 'rm -rf "$tmp"' EXIT INT TERM

printf '%s\n' "$private_key" > "$tmp/key.pem"
unset private_key

b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }

# ── JWT (RS256) ─────────────────────────────────────────────────────────────
now=$(date +%s)
header=$(printf '%s' '{"alg":"RS256","typ":"JWT"}' | b64url)
claims=$(jq -nc --arg iss "$client_email" --arg aud "$token_uri" \
  --argjson iat "$now" --argjson exp "$((now + 3600))" \
  '{iss:$iss, scope:"https://www.googleapis.com/auth/androidpublisher", aud:$aud, iat:$iat, exp:$exp}' \
  | b64url)
printf '%s.%s' "$header" "$claims" > "$tmp/jwt.unsigned"
sig=$(openssl dgst -sha256 -sign "$tmp/key.pem" -binary "$tmp/jwt.unsigned" | b64url)
printf 'grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer&assertion=%s.%s' \
  "$(cat "$tmp/jwt.unsigned")" "$sig" > "$tmp/token.form"

# ── call: one curl invocation (URL last); anything but 2xx is fatal ─────────
# call <step> <resp-file> <curl args...> <url>
call() {
  step=$1; resp=$2; shift 2
  code=$(curl -sS -o "$resp" -w '%{http_code}' "$@") || code=000
  case "$code" in
    2??) return 0 ;;
  esac
  msg=$(jq -r '(.error | objects | .message) // .error_description // (.error | strings) // "no message"' "$resp" 2>/dev/null) \
    || msg="no message"
  echo "❌ $step: HTTP $code: ${msg:-no message}"
  exit 1
}

# ── 1. access token ─────────────────────────────────────────────────────────
call token "$tmp/token.json" -X POST -d @"$tmp/token.form" "$token_uri"
access_token=$(jq -r '.access_token // empty' "$tmp/token.json")
[ -n "$access_token" ] || { echo "❌ token: response has no access_token"; exit 1; }
printf 'Authorization: Bearer %s\n' "$access_token" > "$tmp/auth.hdr"
unset access_token

# ── 2. open an edit ─────────────────────────────────────────────────────────
call edits.insert "$tmp/edit.json" -X POST -H @"$tmp/auth.hdr" "$API/applications/$pkg/edits"
edit=$(jq -r '.id // empty' "$tmp/edit.json")
[ -n "$edit" ] || { echo "❌ edits.insert: response has no edit id"; exit 1; }

# ── 3. is the versionCode already on Play? ──────────────────────────────────
call bundles.list "$tmp/bundles.json" -H @"$tmp/auth.hdr" "$API/applications/$pkg/edits/$edit/bundles"
if jq -e --argjson vc "$vc" '[.bundles[]? | select(.versionCode == $vc)] | length > 0' \
     "$tmp/bundles.json" >/dev/null 2>&1; then
  echo "ℹ️ versionCode $vc already on Play, skipping upload"
else
  # ── 4. upload the bundle (the only call with a long timeout) ──────────────
  call upload "$tmp/upload.json" -X POST -H @"$tmp/auth.hdr" \
    -H 'Content-Type: application/octet-stream' --data-binary @"$aab" --max-time 1800 \
    "$UPLOAD_API/applications/$pkg/edits/$edit/bundles?uploadType=media"
  got=$(jq -r '.versionCode // empty' "$tmp/upload.json")
  if [ "$got" != "$vc" ]; then
    echo "❌ upload: versionCode mismatch — expected $vc, Play reports '${got:-none}'"
    exit 1
  fi
  echo "✅ uploaded versionCode $vc"
fi

# ── 5. point the internal track at it ───────────────────────────────────────
printf '{"track":"internal","releases":[{"versionCodes":["%s"],"status":"%s"}]}' "$vc" "$status" \
  > "$tmp/track.json"
call tracks.update "$tmp/track-resp.json" -X PUT -H @"$tmp/auth.hdr" \
  -H 'Content-Type: application/json' -d @"$tmp/track.json" \
  "$API/applications/$pkg/edits/$edit/tracks/internal"

# ── 6. commit ───────────────────────────────────────────────────────────────
call edits.commit "$tmp/commit.json" -X POST -H @"$tmp/auth.hdr" "$API/applications/$pkg/edits/$edit:commit"
[ -n "$(jq -r '.id // empty' "$tmp/commit.json")" ] || { echo "❌ edits.commit: response has no id"; exit 1; }

echo "✅ Play internal: versionCode $vc ($status)"
