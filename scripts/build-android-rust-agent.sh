#!/usr/bin/env bash
set -euo pipefail

TARGET="${1:-arm64-v8a}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
CRATE_DIR="$REPO_ROOT/native/claude-code-rust"
JNI_ROOT="$REPO_ROOT/app/src/main/jniLibs"

resolve_cargo() {
  if command -v cargo >/dev/null 2>&1; then
    command -v cargo
    return 0
  fi

  if [ -x "$HOME/.cargo/bin/cargo" ]; then
    printf '%s\n' "$HOME/.cargo/bin/cargo"
    return 0
  fi

  return 1
}

resolve_rustup() {
  if command -v rustup >/dev/null 2>&1; then
    command -v rustup
    return 0
  fi

  if [ -x "$HOME/.cargo/bin/rustup" ]; then
    printf '%s\n' "$HOME/.cargo/bin/rustup"
    return 0
  fi

  return 1
}

read_android_sdk_dir_from_local_properties() {
  local local_properties_path="$REPO_ROOT/local.properties"
  [ -f "$local_properties_path" ] || return 1

  local raw_value
  raw_value="$(grep -E '^sdk\.dir=' "$local_properties_path" | head -n 1 | cut -d= -f2-)"
  [ -n "$raw_value" ] || return 1

  raw_value="${raw_value//\\:/:}"
  raw_value="${raw_value//\\\\/\\}"
  printf '%s\n' "$raw_value"
}

resolve_android_sdk_dir() {
  local candidate
  for candidate in "${ANDROID_SDK_ROOT:-}" "${ANDROID_HOME:-}" "$(read_android_sdk_dir_from_local_properties 2>/dev/null || true)"; do
    if [ -n "$candidate" ] && [ -d "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  return 1
}

resolve_android_ndk_dir() {
  local sdk_dir="$1"

  if [ -n "${ANDROID_NDK_HOME:-}" ] && [ -d "${ANDROID_NDK_HOME:-}" ]; then
    printf '%s\n' "$ANDROID_NDK_HOME"
    return 0
  fi

  if [ -n "${ANDROID_NDK_ROOT:-}" ] && [ -d "${ANDROID_NDK_ROOT:-}" ]; then
    printf '%s\n' "$ANDROID_NDK_ROOT"
    return 0
  fi

  local ndk_root="$sdk_dir/ndk"
  [ -d "$ndk_root" ] || return 1

  find "$ndk_root" -mindepth 1 -maxdepth 1 -type d | sort | tail -n 1
}

case "$TARGET" in
  arm64-v8a)
    ABI_DIR="arm64-v8a"
    CARGO_TARGET="aarch64-linux-android"
    CLANG_EXECUTABLE="aarch64-linux-android26-clang"
    CC_ENV_NAME="CC_aarch64_linux_android"
    AR_ENV_NAME="AR_aarch64_linux_android"
    CARGO_LINKER_ENV="CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
    ;;
  x86_64)
    ABI_DIR="x86_64"
    CARGO_TARGET="x86_64-linux-android"
    CLANG_EXECUTABLE="x86_64-linux-android26-clang"
    CC_ENV_NAME="CC_x86_64_linux_android"
    AR_ENV_NAME="AR_x86_64_linux_android"
    CARGO_LINKER_ENV="CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER"
    ;;
  *)
    echo "Unsupported Android ABI target: $TARGET" >&2
    exit 1
    ;;
esac

case "$(uname -s)" in
  Linux)
    HOST_TAG="linux-x86_64"
    ;;
  Darwin)
    case "$(uname -m)" in
      arm64|aarch64)
        HOST_TAG="darwin-arm64"
        ;;
      *)
        HOST_TAG="darwin-x86_64"
        ;;
    esac
    ;;
  *)
    echo "Unsupported host OS for Android NDK toolchain resolution: $(uname -s)" >&2
    exit 1
    ;;
esac

CARGO_PATH="$(resolve_cargo)" || {
  echo "cargo was not found. Install Rust or expose ~/.cargo/bin to PATH." >&2
  exit 1
}
RUSTUP_PATH="$(resolve_rustup 2>/dev/null || true)"
if [ -n "$RUSTUP_PATH" ] && ! "$RUSTUP_PATH" target list --installed | grep -qx "$CARGO_TARGET"; then
  echo "Rust target $CARGO_TARGET is not installed. Installing with rustup..."
  "$RUSTUP_PATH" target add "$CARGO_TARGET"
fi

SDK_DIR="$(resolve_android_sdk_dir)" || {
  echo "Android SDK was not found. Set ANDROID_SDK_ROOT/ANDROID_HOME or configure sdk.dir in local.properties." >&2
  exit 1
}

NDK_DIR="$(resolve_android_ndk_dir "$SDK_DIR")" || {
  echo "Android NDK was not found. Install an NDK or set ANDROID_NDK_HOME." >&2
  exit 1
}

NDK_BIN_DIR="$NDK_DIR/toolchains/llvm/prebuilt/$HOST_TAG/bin"
[ -d "$NDK_BIN_DIR" ] || {
  echo "Android NDK LLVM toolchain bin directory was not found: $NDK_BIN_DIR" >&2
  exit 1
}

TARGET_LINKER="$NDK_BIN_DIR/$CLANG_EXECUTABLE"
TARGET_AR="$NDK_BIN_DIR/llvm-ar"
[ -x "$TARGET_LINKER" ] || {
  echo "Android target linker was not found: $TARGET_LINKER" >&2
  exit 1
}
[ -x "$TARGET_AR" ] || {
  echo "Android target archiver was not found: $TARGET_AR" >&2
  exit 1
}

PROFILE="release"
OUT_DIR="$JNI_ROOT/$ABI_DIR"
mkdir -p "$OUT_DIR"

export PATH="$(dirname "$CARGO_PATH"):$NDK_BIN_DIR:$PATH"
export "$CC_ENV_NAME=$TARGET_LINKER"
export "$AR_ENV_NAME=$TARGET_AR"
export "$CARGO_LINKER_ENV=$TARGET_LINKER"

cd "$CRATE_DIR"
"$CARGO_PATH" build --lib --target "$CARGO_TARGET" --release --no-default-features --features mobile-bridge

BUILT_LIB_PATH="$CRATE_DIR/target/$CARGO_TARGET/$PROFILE/libclaude_code_rs.so"
[ -f "$BUILT_LIB_PATH" ] || {
  echo "Expected native library was not produced: $BUILT_LIB_PATH" >&2
  exit 1
}

cp "$BUILT_LIB_PATH" "$OUT_DIR/libclaude_code_rs.so"
echo "Embedded Rust agent built for $TARGET and copied to $OUT_DIR/libclaude_code_rs.so."
