#!/usr/bin/env bash
# Build CoachFFI.xcframework (the Rust coach-ffi crate as a static library
# for iOS device + simulator) and the generated Swift bindings.
#
# Outputs:
#   apps/ios/Frameworks/CoachFFI.xcframework
#   apps/ios/TeachMeChess/Generated/coach_ffi.swift
#
# Requirements: rustup (Homebrew cargo cannot cross-compile to iOS),
# Xcode command line tools.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

CRATE=coach-ffi
LIB_NAME=libcoach_ffi.a
DEVICE_TARGET=aarch64-apple-ios
SIM_TARGET=aarch64-apple-ios-sim
OUT_XCFRAMEWORK="$REPO_ROOT/apps/ios/Frameworks/CoachFFI.xcframework"
GEN_SWIFT_DIR="$REPO_ROOT/apps/ios/TeachMeChess/Generated"
BINDINGS_DIR="$REPO_ROOT/target/uniffi-bindings"

if ! command -v rustup >/dev/null 2>&1; then
    echo "error: rustup is required (the Homebrew cargo cannot target iOS)." >&2
    echo "       Install from https://rustup.rs and re-run." >&2
    exit 1
fi

for target in "$DEVICE_TARGET" "$SIM_TARGET"; do
    if ! rustup target list --installed | grep -qx "$target"; then
        echo "==> Adding Rust target $target"
        rustup target add "$target"
    fi
done

# Pin the rustup toolchain's cargo AND rustc explicitly: a Homebrew rust on
# PATH would otherwise shadow them (Homebrew rustc has no iOS std) — even
# under `rustup run` when the profile re-prepends /opt/homebrew/bin.
SYSROOT="$(rustup run stable rustc --print sysroot)"
CARGO="$SYSROOT/bin/cargo"
export RUSTC="$SYSROOT/bin/rustc"

# Align the deployment target across the C++ (stockfish-embedded, compiled
# via the cc crate) and Rust link steps. Without this, rustc links at its
# default arm64-apple-ios10.0 while the iOS 26 SDK's C++ objects emit
# ___chkstk_darwin stack probes that libSystem only exports at modern
# deployment targets -> undefined-symbol link failure. 17.0 matches the
# app's deployment target in apps/ios/project.yml.
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-17.0}"

echo "==> Building $CRATE staticlib for $DEVICE_TARGET and $SIM_TARGET (release)"
"$CARGO" build -p "$CRATE" --release --target "$DEVICE_TARGET"
"$CARGO" build -p "$CRATE" --release --target "$SIM_TARGET"

echo "==> Building host dylib for bindings generation"
"$CARGO" build -p "$CRATE"

echo "==> Generating Swift bindings"
rm -rf "$BINDINGS_DIR"
"$CARGO" run -p "$CRATE" --bin uniffi-bindgen -- generate \
    --library target/debug/libcoach_ffi.dylib \
    --language swift \
    --out-dir "$BINDINGS_DIR"

# Assemble the C header + modulemap into an include dir for the xcframework.
# uniffi names the Clang module coach_ffiFFI; the generated coach_ffi.swift
# imports it under exactly that name, so keep it.
INCLUDE_DIR="$BINDINGS_DIR/include"
mkdir -p "$INCLUDE_DIR"
cp "$BINDINGS_DIR/coach_ffiFFI.h" "$INCLUDE_DIR/"
cp "$BINDINGS_DIR/coach_ffiFFI.modulemap" "$INCLUDE_DIR/module.modulemap"

echo "==> Assembling $OUT_XCFRAMEWORK"
rm -rf "$OUT_XCFRAMEWORK"
mkdir -p "$(dirname "$OUT_XCFRAMEWORK")"
xcodebuild -create-xcframework \
    -library "target/$DEVICE_TARGET/release/$LIB_NAME" -headers "$INCLUDE_DIR" \
    -library "target/$SIM_TARGET/release/$LIB_NAME" -headers "$INCLUDE_DIR" \
    -output "$OUT_XCFRAMEWORK"

echo "==> Installing generated Swift into $GEN_SWIFT_DIR"
mkdir -p "$GEN_SWIFT_DIR"
cp "$BINDINGS_DIR/coach_ffi.swift" "$GEN_SWIFT_DIR/coach_ffi.swift"

echo "Done."
echo "  $OUT_XCFRAMEWORK"
echo "  $GEN_SWIFT_DIR/coach_ffi.swift"
