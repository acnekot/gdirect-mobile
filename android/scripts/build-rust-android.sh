#!/usr/bin/env bash
# Builds the Rust engine for Android and places the shared libraries where
# Gradle packages them (app/android/app/src/main/jniLibs/<abi>/).
#
#   ABIS="arm64-v8a armeabi-v7a x86_64" ./scripts/build-rust-android.sh
#
# Needs the Android NDK (ANDROID_NDK_HOME, or ANDROID_HOME/ndk/<version>).
set -euo pipefail
cd "$(dirname "$0")/.."

API="${ANDROID_API:-26}"
ABIS="${ABIS:-arm64-v8a}"
PROFILE="${PROFILE:-release}"

if [ -z "${ANDROID_NDK_HOME:-}" ]; then
  if [ -n "${ANDROID_NDK_ROOT:-}" ]; then
    ANDROID_NDK_HOME="$ANDROID_NDK_ROOT"
  else
    sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
    [ -n "$sdk" ] && ANDROID_NDK_HOME="$(ls -d "$sdk"/ndk/* 2>/dev/null | sort -V | tail -1)"
  fi
fi
[ -d "${ANDROID_NDK_HOME:-}" ] || { echo "Android NDK not found; set ANDROID_NDK_HOME" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) host=darwin-x86_64 ;;
  *) host=linux-x86_64 ;;
esac
bin="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$host/bin"

flags=()
[ "$PROFILE" = release ] && flags+=(--release)

for abi in $ABIS; do
  case "$abi" in
    arm64-v8a)   triple=aarch64-linux-android;  clang="aarch64-linux-android$API-clang" ;;
    armeabi-v7a) triple=armv7-linux-androideabi; clang="armv7a-linux-androideabi$API-clang" ;;
    x86_64)      triple=x86_64-linux-android;   clang="x86_64-linux-android$API-clang" ;;
    *) echo "unknown ABI $abi" >&2; exit 1 ;;
  esac
  rustup target add "$triple" >/dev/null
  under="${triple//-/_}"
  upper="$(echo "$under" | tr '[:lower:]' '[:upper:]')"
  export "CC_$under=$bin/$clang"
  export "AR_$under=$bin/llvm-ar"
  export "CARGO_TARGET_${upper}_LINKER=$bin/$clang"
  echo "==> $abi ($triple)"
  cargo build "${flags[@]}" -p gdirect-mobile --target "$triple"
  out="app/android/app/src/main/jniLibs/$abi"
  mkdir -p "$out"
  cp "target/$triple/$PROFILE/libgdirect_mobile.so" "$out/"
  ls -la "$out/libgdirect_mobile.so"
done
