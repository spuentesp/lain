#!/usr/bin/env bash
# Assemble the Windows release archive from one Cargo profile directory.
# Both release.yml and the Windows CI lane call this script so the archive
# contract is tested before a release tag is pushed.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <cargo-output-dir> <archive-path>" >&2
  exit 2
fi

build_dir="$1"
archive="$2"
main_bin="$build_dir/lain.exe"
sidecar_bin="$build_dir/lain-git-sidecar.exe"

for required in "$main_bin" "$sidecar_bin"; do
  if [ ! -f "$required" ]; then
    echo "missing Windows release file: $required" >&2
    exit 1
  fi
done

dlls=()
has_directml=false
for candidate in "$build_dir"/*; do
  [ -f "$candidate" ] || continue
  case "${candidate,,}" in
    *.dll)
      dlls+=("$candidate")
      name="${candidate##*/}"
      if [ "${name,,}" = "directml.dll" ]; then
        has_directml=true
      fi
      ;;
  esac
done

if [ "${#dlls[@]}" -eq 0 ]; then
  echo "Windows build produced no runtime DLLs; DirectML.dll is required by lain.exe" >&2
  exit 1
fi
if [ "$has_directml" != true ]; then
  echo "DirectML.dll is missing from the Windows build output" >&2
  exit 1
fi

archive_dir="$(dirname "$archive")"
mkdir -p "$archive_dir"
stage="$(mktemp -d "${TMPDIR:-/tmp}/lain-windows-release.XXXXXX")"
archive_tmp="$(mktemp "$archive_dir/.lain-windows-release.XXXXXX")"
cleanup() {
  rm -rf "$stage"
  [ -z "$archive_tmp" ] || rm -f "$archive_tmp"
}
trap cleanup EXIT

files=("lain.exe" "lain-git-sidecar.exe")
cp "$main_bin" "$sidecar_bin" "$stage/"
for dll in "${dlls[@]}"; do
  name="${dll##*/}"
  cp "$dll" "$stage/$name"
  files+=("$name")
done

# Write through the shell, not `tar czf <path>`: GNU tar (Git Bash on the
# Windows runners) reads `D:\a\_temp\x.tar.gz` as a remote `host:path`
# and fails with "Cannot connect to D: resolve failed".
tar czf - -C "$stage" "${files[@]}" > "$archive_tmp"
mv -f "$archive_tmp" "$archive"
archive_tmp=""

echo "packaged Windows release archive: $archive"
