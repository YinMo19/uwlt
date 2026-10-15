#!/bin/bash
# Install the official Wallet Core 4.8.4 macOS arm64 static archive.
# Requires only Xcode Command Line Tools, curl, tar and shasum; no Node/JVM.
set -euo pipefail

readonly VERSION=4.8.4
readonly SHA256=6d87c490c16a12fb51d3ddb5bca4d277f08d26aad172c48deb773d3dd4b72567
readonly LIB_SHA256=247ba1f4ef257b6db805a540a7a6ac2e6bdd5138642b98727cadc19e7ecc6fdb
readonly URL="https://github.com/trustwallet/wallet-core/releases/download/${VERSION}/TrustWalletCore-${VERSION}.tar.xz"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly ROOT
readonly DEST="$ROOT/target/wallet-core-${VERSION}"
readonly ARCHIVE="$ROOT/target/TrustWalletCore-${VERSION}.tar.xz"

if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
    printf '%s\n' 'Wallet Core setup requires native macOS arm64.' >&2
    exit 1
fi
xcrun --find lipo >/dev/null
mkdir -p "$ROOT/target"
WORK="$(mktemp -d "$ROOT/target/wallet-core-setup.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

if [[ ! -f "$ARCHIVE" ]]; then
    curl --fail --location --proto '=https' --tlsv1.2 "$URL" -o "$WORK/release.tar.xz"
    printf '%s  %s\n' "$SHA256" "$WORK/release.tar.xz" | shasum -a 256 --check
    mv "$WORK/release.tar.xz" "$ARCHIVE"
fi
# Always authenticate the cache before extraction; never trust a previous run.
printf '%s  %s\n' "$SHA256" "$ARCHIVE" | shasum -a 256 --check
mkdir "$WORK/release"
tar -xf "$ARCHIVE" -C "$WORK/release" \
    ./WalletCoreCommon.xcframework/macos-arm64_x86_64 ./include ./License
readonly FRAMEWORK="$WORK/release/WalletCoreCommon.xcframework/macos-arm64_x86_64/WalletCoreCommon.framework/Versions/A/WalletCoreCommon"
# This framework binary is a universal ar archive, not a dynamic framework.
# It already contains Wallet Core, TrezorCrypto, protobuf and wallet_core_rs.
xcrun lipo "$FRAMEWORK" -thin arm64 -output "$WORK/libWalletCore.a"
printf '%s  %s\n' "$LIB_SHA256" "$WORK/libWalletCore.a" | shasum -a 256 --check
mkdir -p "$DEST"
mv "$WORK/libWalletCore.a" "$DEST/libWalletCore.a"
cp "$WORK/release/License" "$DEST/License"
printf 'Installed Wallet Core %s: %s\n' "$VERSION" "$DEST/libWalletCore.a"
