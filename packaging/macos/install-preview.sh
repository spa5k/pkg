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
pkg_setup_fail() {
    local status=$1
    shift
    printf '%s\n' "$*" "Keep this log for support: $log" \
        | /usr/bin/tee -a "$log" >&2
    exit "$status"
}
pkg_cli=/usr/local/bin/pkg
pkg_bin=${pkg_cli%/*}
pkg_prefix=${pkg_bin%/*}
pkg_state() {
    printf '\n%s\n' "$1"
    shift
    "$@" /bin/ls -ldneO "$pkg_prefix" "$pkg_bin" "$pkg_cli" \
        /opt/pkg/bin /opt/pkg/bin/pkg-nix-broker /opt/pkg/bin/pkg-root-helper \
        /opt/pkg/uninstall/manifest.json /nix/receipt.json /nix/nix-installer /pkg 2>&1 || :
}
echo "Install log: $log"
pkg_state 'State before setup, as the invoking user (private paths can deny access):' >>"$log"
[ "$(/usr/bin/id -u)" -ne 0 ] || pkg_setup_fail 1 \
    'Run this script as your normal user. It will request administrator access when needed.'
echo 'Checking access to /usr/local/bin/pkg...'
for parent in "$pkg_prefix" "$pkg_bin"; do
    [ ! -L "$parent" ] || pkg_setup_fail 1 \
        "The command directory $parent is a symbolic link. Ask an administrator to check this path before setup."
    # A fresh host can lack these paths. Let the privileged installer handle
    # absent directories, then require executable user access after setup.
    [ -e "$parent" ] || continue
    [ -d "$parent" ] || pkg_setup_fail 1 \
        "The command path $parent is not a directory. Ask an administrator to check this path before setup."
    [ -x "$parent" ] || pkg_setup_fail 1 \
        "Your user cannot enter $parent. Ask an administrator to check its permissions and access rules before setup."
done
echo "Administrator access is needed to set up pkg."
# Keep the terminal's user session. Do not run PackageKit scripts or launchd.
if ! /usr/bin/sudo -n -v 2>/dev/null; then
    ( : </dev/tty ) 2>/dev/null || pkg_setup_fail 1 \
        'Administrator approval is required, but this session has no terminal for the password prompt. Open Terminal and run the installer there as your normal user.'
    echo 'Approve the administrator prompt to continue. Your password is not written to the log.'
    # The original user must open their terminal; sudo reads no password from a pipe.
    # shellcheck disable=SC2024
    if /usr/bin/sudo -v </dev/tty 2>&1 | /usr/bin/tee -a "$log"; then
        :
    else
        code=$?
        pkg_setup_fail "$code" 'Administrator approval was not granted. No installation command was run.'
    fi
fi
pkg_state 'State before setup, with administrator access:' /usr/bin/sudo -n >>"$log"
echo "Allow the macOS system configuration prompt if it appears."
if /usr/bin/sudo -n /usr/bin/env -i HOME=/var/root PATH=/usr/bin:/bin:/usr/sbin:/sbin \
    PKG_INSTALL_DEBUG="${PKG_INSTALL_DEBUG:-0}" "$payload" "$@" 2>&1 | /usr/bin/tee -a "$log"; then
    # Root can verify the installed file even when its parents block this user.
    # Verify public command access before printing user-facing completion.
    pkg_state 'State after setup, as the invoking user:' >>"$log"
    [ -x "$pkg_cli" ] || pkg_setup_fail 1 \
        "pkg command check failed: $pkg_cli is missing or not executable for your user. Check the file and parent directory permissions."
    if ! "$pkg_cli" --version >>"$log" 2>&1; then
        pkg_setup_fail 1 "pkg command check failed: $pkg_cli could not start. The command error is in the log."
    fi
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
    pkg_state 'State after failed setup, as the invoking user:' >>"$log"
    pkg_setup_fail "$code" \
        "pkg setup failed (exit $code). Check the first error and the path access details in the log. Keep Nix and its installation records for recovery."
fi
