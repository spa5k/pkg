#!/bin/sh
set -eu

die() { printf '%s\n' "$*" >&2; exit 1; }
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

[ "$#" -eq 1 ] || die "usage: $0 /absolute/path/to/installer"
asset=$1
case $asset in /*) ;; *) die "asset path must be absolute" ;; esac
[ ! -L "$asset" ] || die "asset must not be a symlink"
[ -f "$asset" ] || die "asset must be a regular file"

script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
pins=$script_dir/assets.sha256
sudo=/usr/bin/sudo
platform=
if [ "${S6_TEST_MODE:-}" = 1 ]; then
    pins=${S6_TEST_ASSETS_SHA256:-$pins}
    sudo=${S6_TEST_SUDO:-$sudo}
    platform=${S6_TEST_PLATFORM:-}
fi

if [ -z "$platform" ]; then
    machine=$(uname -m)
    system=$(uname -s)
    case "$machine:$system" in
        arm64:Darwin|aarch64:Darwin) platform=aarch64-darwin ;;
        aarch64:Linux|arm64:Linux) platform=aarch64-linux ;;
        x86_64:Linux) platform=x86_64-linux ;;
        *) die "unsupported target: $machine-$system" ;;
    esac
fi
case $platform in
    aarch64-darwin|aarch64-linux|x86_64-linux) ;;
    *) die "unsupported target: $platform" ;;
esac

expected=$(awk -v platform="$platform" '$2 == platform { print $1 }' "$pins")
[ -n "$expected" ] || die "no digest for target: $platform"
actual=$(sha256 "$asset")
[ "$actual" = "$expected" ] || die "asset digest mismatch"

exec "$sudo" -- "$script_dir/stage.sh" "$asset" "$expected" "$platform"
