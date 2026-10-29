#!/usr/bin/env bash
# Publishes the last android/build.sh APK for the app to update itself from:
# the phone reads update.json and oasis.apk from this directory over ssh.
#
#   android/publish.sh [debug|release]   (default: debug)
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
VARIANT=${1:-debug}
OUT=$HERE/app/build/outputs/apk/$VARIANT
DEST=${OASIS_ANDROID_UPDATES:-$HOME/.local/share/oasis/android}
read -r CODE NAME < "$OUT/version"
APK=$(ls "$OUT"/*.apk | head -1)

mkdir -p "$DEST"
cp "$APK" "$DEST/oasis.apk.tmp"
mv "$DEST/oasis.apk.tmp" "$DEST/oasis.apk"
SHA=$(sha256sum "$DEST/oasis.apk" | cut -d' ' -f1)
printf '{"version_code":%s,"version_name":"%s","sha256":"%s"}\n' "$CODE" "$NAME" "$SHA" > "$DEST/update.json.tmp"
mv "$DEST/update.json.tmp" "$DEST/update.json"
echo "published $NAME ($CODE)"
