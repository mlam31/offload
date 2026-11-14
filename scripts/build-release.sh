#!/bin/zsh
# Build release binaries for macOS (arm64 + x86_64), Windows x64, and Linux x64,
# package them into dist/, and print sha256 checksums.
set -euo pipefail
cd "$(dirname "$0")/.."

# Use the rustup toolchain (its extra std targets are installed there);
# Homebrew's rustc only ships host-target std libs.
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"

VERSION="$1" # e.g. 0.1.0
NAME="offload"

rm -rf dist
mkdir -p dist

echo "==> macOS arm64 (Apple Silicon)"
cargo build --release --target aarch64-apple-darwin
echo "==> macOS x86_64 (Intel)"
cargo build --release --target x86_64-apple-darwin
echo "==> Windows x86_64"
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --release --target x86_64-pc-windows-gnu
echo "==> Linux x86_64 (static musl)"
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
  cargo build --release --target x86_64-unknown-linux-musl

echo "==> Packaging"
staging=$(mktemp -d)
cp README.md "$staging/"

package() {
  local bin=$1 out=$2 ext=$3
  rm -rf "$staging/bin"
  mkdir -p "$staging/bin"
  cp "target/$4/release/$bin$ext" "$staging/bin/${NAME}${ext}"
  if [[ $ext == ".exe" ]]; then
    (cd "$staging" && zip -q -r "$OLDPWD/dist/${NAME}-v${VERSION}-${out}.zip" bin README.md)
  else
    tar -czf "dist/${NAME}-v${VERSION}-${out}.tar.gz" -C "$staging" bin README.md
  fi
}

package offload aarch64-apple-darwin "" aarch64-apple-darwin
package offload x86_64-apple-darwin "" x86_64-apple-darwin
package offload x86_64-pc-windows-gnu ".exe" x86_64-pc-windows-gnu
package offload x86_64-unknown-linux-musl "" x86_64-unknown-linux-musl

echo "==> Checksums"
shasum -a 256 dist/* | tee dist/checksums.txt
