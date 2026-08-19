#!/bin/sh
set -eu

usage() {
    echo "usage: build-preview.sh /absolute/path/to/pkg-install /absolute/output.pkg [version]" >&2
    exit 64
}

[ "$#" -ge 2 ] && [ "$#" -le 3 ] || usage
[ "$(/usr/bin/uname -s)" = Darwin ] || {
    echo "The macOS preview package must be built on macOS." >&2
    exit 1
}

installer=$1
output=$2
version=${3:-0.1.0-alpha.3}

case "$installer" in /*) ;; *) usage ;; esac
case "$output" in /*.pkg) ;; *) usage ;; esac
case "$version" in *[!A-Za-z0-9._-]*|'') usage ;; esac

[ -f "$installer" ] && [ ! -L "$installer" ] && [ -x "$installer" ] || {
    echo "pkg-install must be an executable regular file." >&2
    exit 1
}
[ ! -e "$output" ] || {
    echo "The output package already exists." >&2
    exit 1
}
[ -d "$(/usr/bin/dirname "$output")" ] || {
    echo "The output directory does not exist." >&2
    exit 1
}

script_dir=$(CDPATH= cd -- "$(/usr/bin/dirname "$0")" && /bin/pwd -P)
work=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/pkg-macos-preview.XXXXXX")
trap '/bin/rm -rf "$work"' EXIT HUP INT TERM

/bin/mkdir -m 0700 "$work/scripts"
/usr/bin/install -m 0755 "$installer" "$work/scripts/pkg-install"
/usr/bin/codesign --force --sign - --options runtime --timestamp=none \
    "$work/scripts/pkg-install"
/usr/bin/codesign --verify --strict --verbose=2 "$work/scripts/pkg-install"
/usr/bin/install -m 0755 "$script_dir/postinstall" "$work/scripts/postinstall"

/usr/bin/pkgbuild \
    --nopayload \
    --scripts "$work/scripts" \
    --identifier org.pkg.installer.preview \
    --version "$version" \
    "$output"

echo "Created local unnotarized preview: $output"
