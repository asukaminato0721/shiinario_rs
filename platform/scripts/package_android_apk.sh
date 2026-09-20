#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"
: "${ANDROID_HOME:?Set ANDROID_HOME to the Android SDK directory}"
: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to the Android NDK directory}"
VARIANT="${VARIANT:-debug}"
case "$VARIANT" in
  debug) PROFILE=(); TASK=assembleDebug; APK=app-debug.apk ;;
  release) PROFILE=(--release); TASK=assembleRelease; APK=app-release-unsigned.apk ;;
  *) echo "VARIANT must be debug or release" >&2; exit 1 ;;
esac
command -v cargo-ndk >/dev/null || cargo install cargo-ndk --locked
cargo ndk -t arm64-v8a -t x86_64 --platform 28 \
  -o platform/android/app/src/main/jniLibs \
  build --locked "${PROFILE[@]}" -p shiinario_runtime --lib
(cd platform/android && ./gradlew ":app:$TASK")
mkdir -p dist/android
cp "platform/android/app/build/outputs/apk/$VARIANT/$APK" "dist/android/shiinario-android-$VARIANT.apk"
