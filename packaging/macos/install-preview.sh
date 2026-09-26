#!/bin/bash
# Run with bash; never source this file into the interactive shell.
set -euo pipefail
umask 077

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "usage: install-preview.sh /path/to/preview.pkg expected-sha256 [--resume]" >&2
    exit 64
fi
package=$1
expected=$2
if [ "$#" -eq 3 ]; then
    [ "$3" = --resume ] || { echo "Invalid recovery option." >&2; exit 64; }
fi
shift 2
case "$expected" in *[!0-9a-f]*|'') echo "Invalid SHA-256." >&2; exit 64 ;; esac
[ "${#expected}" -eq 64 ] || { echo "Invalid SHA-256 length." >&2; exit 64; }
[ -f "$package" ] || { echo "Package not found: $package" >&2; exit 66; }

echo "Checking the downloaded pkg installer..."
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
/usr/bin/codesign --verify --strict "$payload"

log=$(/usr/bin/mktemp "${TMPDIR:-/tmp}/pkg-install-log.XXXXXX")
echo "Install log: $log"
echo "Administrator access is needed to set up pkg."
echo "Allow the macOS system configuration prompt if it appears."
# Keep the terminal's user session. Do not run PackageKit scripts or launchd.
/usr/bin/sudo -v
if /usr/bin/sudo /usr/bin/env -i HOME=/var/root PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    PKG_INSTALL_DEBUG="${PKG_INSTALL_DEBUG:-0}" "$payload" "$@" 2>&1 | /usr/bin/tee "$log"; then
    echo "pkg setup completed."
    echo
    echo 'Next steps:'
    echo '  [ ! -x /usr/local/bin/pkg ] || eval "$(/usr/local/bin/pkg shellenv)"'
    echo '  pkg doctor'
    echo '  pkg install fzf'
    echo
    echo 'For future zsh sessions, add [ ! -x /usr/local/bin/pkg ] || eval "$(/usr/local/bin/pkg shellenv)" to ~/.zshrc.'
    echo 'For Bash, add the same line to your shell startup file.'
else
    code=$?
    echo "pkg setup failed (exit $code). Log: $log" >&2
    if /usr/bin/grep -Eq 'Operation not permitted|Permission denied' "$log"; then
        echo "macOS denied access. Check Privacy & Security > Full Disk Access for this terminal, then reopen it." >&2
    fi
    echo "Keep this log for support. Do not delete Nix or its installation records." >&2
    exit "$code"
fi
