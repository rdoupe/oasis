#!/usr/bin/env bash
# Builds the JNI bridge for one Android ABI into the app's jniLibs directory.
#
#   android/core/build.sh <arm64-v8a|x86_64> <jniLibs-dir>
set -euo pipefail

ABI=${1:?abi: arm64-v8a or x86_64}
OUT=${2:?jniLibs directory}
case $ABI in
    arm64-v8a) ARCH=aarch64 ;;
    x86_64) ARCH=x86_64 ;;
    *) echo "unknown ABI $ABI" >&2; exit 1 ;;
esac
API=29
TARGET=$ARCH-linux-android
NDK=${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/28.2.13676358}
TOOLS=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
SYSROOT=$NDK/toolchains/llvm/prebuilt/linux-x86_64/sysroot
CACHE=${OASIS_ANDROID_CACHE:-$HOME/.cache/oasis-android}
HERE=$(cd "$(dirname "$0")" && pwd)

mkdir -p "$CACHE" "$OUT/$ABI"
# Zig builds libghostty-vt against the NDK's bionic through this libc description.
LIBC=$CACHE/libc-$ARCH.txt
cat > "$LIBC" <<LIBC
include_dir=$SYSROOT/usr/include
sys_include_dir=$SYSROOT/usr/include/$TARGET
crt_dir=$SYSROOT/usr/lib/$TARGET/$API
msvc_lib_dir=
kernel32_lib_dir=
gcc_dir=
LIBC

UPPER=$(echo "$TARGET" | tr 'a-z-' 'A-Z_')
LOWER=$(echo "$TARGET" | tr '-' '_')
export LIBGHOSTTY_VT_ANDROID_LIBC=$LIBC
export "CARGO_TARGET_${UPPER}_LINKER=$TOOLS/$TARGET$API-clang"
export "CC_$LOWER=$TOOLS/$TARGET$API-clang"
export "AR_$LOWER=$TOOLS/llvm-ar"
export CARGO_TARGET_DIR=$CACHE/target
cargo build --release --lib --target "$TARGET" --manifest-path "$HERE/Cargo.toml"
"$TOOLS/llvm-strip" -o "$OUT/$ABI/liboasis_android.so" "$CARGO_TARGET_DIR/$TARGET/release/liboasis_android.so"
ls -l "$OUT/$ABI/liboasis_android.so"
