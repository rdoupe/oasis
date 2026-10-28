#!/usr/bin/env bash
# Builds the Android app: OpenSSH and the JNI bridge for each ABI, the bundled
# font, then the APK.
#
#   android/build.sh [debug|release] [abi...]   (default: debug arm64-v8a x86_64)
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
VARIANT=${1:-debug}
shift || true
ABIS=("$@")
[ ${#ABIS[@]} -gt 0 ] || ABIS=(arm64-v8a x86_64)
GEN=$HERE/app/build/generated/oasis
CACHE=${OASIS_ANDROID_CACHE:-$HOME/.cache/oasis-android}

for abi in "${ABIS[@]}"; do
    case $abi in
        arm64-v8a) arch=aarch64 ;;
        x86_64) arch=x86_64 ;;
        *) echo "unknown ABI $abi" >&2; exit 1 ;;
    esac
    "$HERE/openssh/build.sh" "$arch" "$GEN/jniLibs/$abi" > "$CACHE/openssh-$arch.log" 2>&1 \
        || { tail -20 "$CACHE/openssh-$arch.log"; exit 1; }
    "$HERE/core/build.sh" "$abi" "$GEN/jniLibs"
done

# JetBrains Mono Nerd Font (OFL): the agents' glyphs and icons in one monospace face.
FONTS=$GEN/res/font
mkdir -p "$FONTS"
for style in Regular Bold; do
    out=$FONTS/jetbrains_mono_$(echo "$style" | tr 'A-Z' 'a-z').ttf
    [ -f "$out" ] && continue
    if [ -f "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-$style.ttf" ]; then
        cp "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-$style.ttf" "$out"
    else
        mkdir -p "$CACHE/fonts"
        [ -f "$CACHE/fonts/JetBrainsMono.zip" ] || curl -sSfL -o "$CACHE/fonts/JetBrainsMono.zip" \
            https://github.com/ryanoasis/nerd-fonts/releases/download/v3.4.0/JetBrainsMono.zip
        unzip -p "$CACHE/fonts/JetBrainsMono.zip" "JetBrainsMonoNerdFont-$style.ttf" > "$out"
    fi
done

# Agent logos (Lobe Icons, MIT, app/logos), single-color marks in the theme's text color.
LOGOS=$GEN/res/drawable-nodpi
mkdir -p "$LOGOS"
for name in claude:claude-color codex:openai cursor:cursor grok:grok gemini:gemini-color; do
    sed 's/currentColor/#c0caf5/g' "$HERE/app/logos/${name#*:}.svg" \
        | rsvg-convert -w 144 -h 144 -o "$LOGOS/logo_${name%%:*}.png"
done

cd "$HERE"
TASK=assemble$(echo "${VARIANT:0:1}" | tr a-z A-Z)${VARIANT:1}
gradle --quiet "$TASK"
ls -l "$HERE/app/build/outputs/apk/$VARIANT/"*.apk
