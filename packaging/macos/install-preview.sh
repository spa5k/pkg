#!/bin/bash
# Run with bash; never source this file into the interactive shell.
set -euo pipefail
umask 077

if [ "$#" -ne 2 ]; then
    echo "usage: install-preview.sh /path/to/preview.pkg expected-sha256" >&2
    exit 64
fi
package=$1
expected=$2
case "$expected" in *[!0-9a-f]*|'') echo "Invalid SHA-256." >&2; exit 64 ;; esac
[ "${#expected}" -eq 64 ] || { echo "Invalid SHA-256 length." >&2; exit 64; }
[ -f "$package" ] || { echo "Package not found: $package" >&2; exit 66; }

work=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/pkg-preview.XXXXXX")
trap '/bin/rm -rf "$work"' EXIT
# Verify the private copy that will be expanded, not a replaceable input path.
/bin/cp "$package" "$work/preview.pkg"
actual=$(/usr/bin/shasum -a 256 "$work/preview.pkg")
[ "${actual%% *}" = "$expected" ] || { echo "Package checksum mismatch." >&2; exit 1; }
/usr/sbin/pkgutil --expand-full "$work/preview.pkg" "$work/expanded"
payload="$work/expanded/Scripts/pkg-install"
[ -f "$payload" ] && [ ! -L "$payload" ] && [ -x "$payload" ] || {
    echo "Package does not contain the preview installer." >&2
    exit 1
}
/usr/bin/codesign --verify --strict --verbose=2 "$payload"

log=$(/usr/bin/mktemp "${TMPDIR:-/tmp}/pkg-install-log.XXXXXX")
echo "Install log: $log"
echo "Allow the macOS system configuration prompt if it appears."
# Keep the terminal's user session. Do not run PackageKit scripts or launchd.
/usr/bin/sudo -v
if /usr/bin/sudo /usr/bin/env -i HOME=/var/root PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    PKG_INSTALL_DEBUG=1 "$payload" 2>&1 | /usr/bin/tee "$log"; then
    echo "pkg setup completed."
else
    code=$?
    echo "pkg setup failed (exit $code). Log: $log" >&2
    echo "For a macOS permission denial, allow this terminal in Privacy & Security > Full Disk Access." >&2
    exit "$code"
fi
