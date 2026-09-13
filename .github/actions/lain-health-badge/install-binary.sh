#!/usr/bin/env bash
# Install exactly the requested release without running its latest-version installer.
set -euo pipefail
version="${LAIN_VERSION:?LAIN_VERSION must be a release tag}"
version="${version#v}"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Invalid release version: $version" >&2
  exit 1
fi
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=x86_64-unknown-linux-gnu; binary=lain ;;
  Darwin-arm64) target=aarch64-apple-darwin; binary=lain ;;
  MINGW*-x86_64|MSYS*-x86_64|CYGWIN*-x86_64) target=x86_64-pc-windows-msvc; binary=lain.exe ;;
  *) echo "Unsupported runner platform" >&2; exit 1 ;;
esac
install_dir="${LAIN_INSTALL_DIR:-$HOME/.local/lain}"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
url="https://github.com/spuentesp/lain/releases/download/v${version}/lain-${version}-${target}.tar.gz"
curl -fSL --retry 3 "$url" -o "$scratch/lain.tar.gz"
tar -xzf "$scratch/lain.tar.gz" -C "$scratch" "$binary"
chmod +x "$scratch/$binary"
actual=$("$scratch/$binary" --version)
if [ "$actual" != "lain $version" ]; then
  echo "Release version mismatch: requested $version, got $actual" >&2
  exit 1
fi
mkdir -p "$install_dir"
cp "$scratch/$binary" "$install_dir/$binary"
printf '%s\n' "$install_dir" >> "${GITHUB_PATH:?GITHUB_PATH must be set}"
