#!/bin/sh
# Release template. The publication job replaces every @PKG_*@ token with a
# reviewed immutable value. An unrendered checkout exits before network access.
set -eu

PKG_RELEASE='@PKG_RELEASE@'
PKG_RELEASE_BASE_URL='@PKG_RELEASE_BASE_URL@'
PKG_SHA256_X86_64_LINUX='@PKG_SHA256_X86_64_LINUX@'

pkg_install_mode='install'
if [ "${1-}" = '--verify-only' ] && [ "$#" -eq 1 ]; then
    pkg_install_mode='verify-only'
elif [ "$#" -ne 0 ]; then
    printf '%s\n' 'usage: install.sh [--verify-only]' >&2
    exit 2
fi

case "$PKG_RELEASE $PKG_RELEASE_BASE_URL $PKG_SHA256_X86_64_LINUX" in
    *'@PKG_'*)
        printf '%s\n' 'pkg: this installer template belongs to an unpublished release; no download was attempted' >&2
        exit 1
        ;;
esac

case "$PKG_RELEASE_BASE_URL" in
    https://*) ;;
    *) printf '%s\n' 'pkg: release URL is not HTTPS' >&2; exit 1 ;;
esac

pkg_kernel=$(uname -s)
pkg_machine=$(uname -m)
case "$pkg_kernel:$pkg_machine" in
    Linux:x86_64)
        pkg_artifact='pkg-installer-x86_64-linux'
        pkg_sha256=$PKG_SHA256_X86_64_LINUX
        ;;
    *)
        printf 'pkg: unsupported platform %s %s\n' "$pkg_kernel" "$pkg_machine" >&2
        exit 1
        ;;
esac

case "$pkg_sha256" in
    *[!0-9a-f]*|'') printf '%s\n' 'pkg: invalid pinned SHA-256' >&2; exit 1 ;;
esac
if [ "${#pkg_sha256}" -ne 64 ]; then
    printf '%s\n' 'pkg: invalid pinned SHA-256 length' >&2
    exit 1
fi

command -v curl >/dev/null 2>&1 || {
    printf '%s\n' 'pkg: curl is required' >&2
    exit 1
}

umask 077
pkg_tmp=$(mktemp -d "${TMPDIR:-/tmp}/pkg-install.XXXXXXXX")
trap 'rm -rf "$pkg_tmp"' EXIT HUP INT TERM
pkg_download="$pkg_tmp/$pkg_artifact"
pkg_url="$PKG_RELEASE_BASE_URL/$PKG_RELEASE/$pkg_artifact"

printf 'Downloading pkg %s...\n' "$PKG_RELEASE"
curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
    --output "$pkg_download" "$pkg_url"

printf '%s\n' 'Checking the download...'
if command -v sha256sum >/dev/null 2>&1; then
    printf '%s  %s\n' "$pkg_sha256" "$pkg_download" | sha256sum --check --status
elif command -v shasum >/dev/null 2>&1; then
    printf '%s  %s\n' "$pkg_sha256" "$pkg_download" | shasum -a 256 --check >/dev/null
else
    printf '%s\n' 'pkg: sha256sum or shasum is required' >&2
    exit 1
fi

printf 'pkg: verified %s (%s)\n' "$pkg_artifact" "$pkg_sha256"
[ "$pkg_install_mode" = 'verify-only' ] && exit 0

chmod 0700 "$pkg_download"
printf '%s\n' 'Administrator access is needed to set up pkg.'
# Keep a private log after the temporary executable is removed.
pkg_log=$(mktemp "${TMPDIR:-/tmp}/pkg-install-log.XXXXXXXX")
printf 'Install log: %s\n' "$pkg_log"
# POSIX sh has no pipefail. Save the actual installer status in the private directory.
( pkg_status=0
  sudo "$pkg_download" 2>&1 || pkg_status=$?
  printf '%s\n' "$pkg_status" >"$pkg_tmp/status"
) | tee "$pkg_log"
pkg_status=$(cat "$pkg_tmp/status")
if [ "$pkg_status" -ne 0 ]; then
    printf 'pkg setup failed (exit %s). Keep this log for support: %s\n' "$pkg_status" "$pkg_log" >&2
    exit "$pkg_status"
fi

printf '%s\n' 'pkg setup completed.' 'Open a new terminal, then run: pkg doctor' 'Install your first package with: pkg install fzf'
