#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd -- "$script_dir/../.." && pwd)"
skip_build=false
case "${1:-}" in
    "") ;;
    --no-build) skip_build=true ;;
    *) echo "Usage: $0 [--no-build]" >&2; exit 2 ;;
esac
if (( $# > 1 )); then
    echo "Usage: $0 [--no-build]" >&2
    exit 2
fi
for tool in rpmbuild rpm magick desktop-file-validate python3 tar; do
    command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 1; }
done

cd -- "$repo_dir"
version="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["workspace"]["package"]["version"])')"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Unsupported RPM version: $version" >&2
    exit 1
fi
if [[ "$skip_build" == false ]]; then
    cargo build --locked --release --package lyrune
fi
binary="$repo_dir/target/release/lyrune"
[[ -x "$binary" ]] || { echo "Build the release binary first: $binary" >&2; exit 1; }
desktop-file-validate "$script_dir/lyrune.desktop"

work="$(mktemp -d "${TMPDIR:-/tmp}/lyrune-rpm.XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT
source_dir="$work/source/lyrune-$version"
mkdir -p "$source_dir/icons" "$work"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}
install -m755 "$binary" "$source_dir/lyrune"
install -m644 "$script_dir/lyrune.desktop" "$source_dir/lyrune.desktop"
install -m644 "$repo_dir/crates/lyrune-app/assets/lyrune.svg" "$source_dir/lyrune.svg"
install -m644 "$repo_dir/README.md" "$repo_dir/THIRD_PARTY_NOTICES.md" "$source_dir/"
install -m644 "$repo_dir/packaging/arch/license-unknown.txt" "$source_dir/license-unknown.txt"
for size in 16 22 24 32 48 64 128 256 512; do
    magick -background none "$source_dir/lyrune.svg" -resize "${size}x${size}" \
        "PNG32:$source_dir/icons/${size}.png"
done
tar -C "$work/source" -czf "$work/SOURCES/lyrune-$version.tar.gz" "lyrune-$version"
rpmbuild --define "_topdir $work" --define "lyrune_version $version" \
    -bb "$script_dir/lyrune.spec"

mkdir -p "$repo_dir/dist"
shopt -s nullglob
packages=("$work"/RPMS/*/lyrune-*.rpm)
(( ${#packages[@]} == 1 )) || { echo "Expected one RPM package" >&2; exit 1; }
package="$repo_dir/dist/$(basename -- "${packages[0]}")"
install -m644 "${packages[0]}" "$package"
rpm -K "$package"
printf '\nPackage: %s\nInstall: bash %q %q\n' "$package" "$script_dir/install-rpm.sh" "$package"
