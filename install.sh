#!/bin/sh
# Publication replaces these values with one reviewed release. No runtime URL overrides.
set -eu
PKG_RELEASE='v0.1.0-alpha.49'
PKG_RELEASE_BASE_URL='https://github.com/spa5k/pkg/releases/download'
PKG_ARTIFACT_X86_64_LINUX='pkg-install-x86_64-linux'
PKG_SHA256_X86_64_LINUX='3643ea8c422bf768a2f2ea62712e933c5d74d3fcfb062278dd83d4181b7c64a4'
PKG_SHA256_MACOS_PACKAGE='30397fa7519c7caf388cd8f6e83207aa1a7ba0b2b03bc6a067c5b720fc529c3a'
PKG_SHA256_MACOS_WRAPPER='64ff6f29a287ce4b4759096b3a862b58a8bca681b05dcf3ea46d8a94793de4b4'

pkg_mode=install
pkg_color=auto
for pkg_arg in "$@"; do
    case "$pkg_arg" in
        --verify-only) pkg_mode=verify ;;
        --no-color) pkg_color=never ;;
        -h|--help)
            printf '%s\n' 'Install or upgrade pkg on Apple silicon macOS or Linux x86-64.' \
                'Usage: install.sh [--verify-only] [--no-color]' \
                '  --verify-only  Download and verify. Do not install or request administrator access.' \
                '  --no-color     Use plain output.'
            exit 0 ;;
        *) printf 'pkg: unknown option: %s\nUsage: install.sh [--verify-only] [--no-color]\n' "$pkg_arg" >&2; exit 2 ;;
    esac
done

pkg_note() { printf '  %s\n' "$*"; }
pkg_ok() {
    if [ "$pkg_color" = auto ] && [ -t 1 ] && [ -z "${NO_COLOR+x}" ] && [ -z "${CI+x}" ] && [ "${TERM:-dumb}" != dumb ]; then
        printf '  \033[32m✓\033[0m %s\n' "$*"
    else
        printf '  [ok] %s\n' "$*"
    fi
}
pkg_fail() { printf '\npkg: %s\n' "$*" >&2; exit 1; }
pkg_digest() {
    case "$1" in *[!0-9a-f]*|'') pkg_fail 'Invalid pinned SHA-256.' ;; esac
    [ "${#1}" -eq 64 ] || pkg_fail 'Invalid pinned SHA-256 length.'
}
pkg_fetch() {
    pkg_note "Downloading $1..."
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 20 --retry 2 --output "$pkg_tmp/$1" \
        "$PKG_RELEASE_BASE_URL/$PKG_RELEASE/$1" || pkg_fail 'Download failed. Check your connection, then run this command again.'
    if command -v shasum >/dev/null 2>&1; then
        printf '%s  %s\n' "$2" "$pkg_tmp/$1" | shasum -a 256 --check >/dev/null || pkg_fail 'Checksum mismatch. Nothing was installed. Download the installer again.'
    else
        printf '%s  %s\n' "$2" "$pkg_tmp/$1" | sha256sum --check >/dev/null || pkg_fail 'Checksum mismatch. Nothing was installed. Download the installer again.'
    fi
    pkg_ok "Verified $1"
}

case "$PKG_RELEASE $PKG_RELEASE_BASE_URL $PKG_SHA256_X86_64_LINUX $PKG_SHA256_MACOS_PACKAGE $PKG_SHA256_MACOS_WRAPPER" in
    *'@PKG_'*) pkg_fail 'This installer template belongs to an unpublished release; no download was attempted.' ;;
esac
case "$PKG_RELEASE_BASE_URL" in https://*) ;; *) pkg_fail 'Release URL is not HTTPS.' ;; esac
pkg_kernel=$(uname -s)
pkg_machine=$(uname -m)
case "$pkg_kernel:$pkg_machine" in
    Linux:x86_64) pkg_artifact=$PKG_ARTIFACT_X86_64_LINUX; pkg_sha256=$PKG_SHA256_X86_64_LINUX; pkg_platform='Linux x86-64' ;;
    Darwin:arm64) pkg_artifact="pkg-${PKG_RELEASE#v}-preview.pkg"; pkg_sha256=$PKG_SHA256_MACOS_PACKAGE; pkg_platform='macOS Apple silicon' ;;
    *) pkg_fail "Unsupported platform: $pkg_kernel $pkg_machine. Use Apple silicon macOS or Linux x86-64 with systemd." ;;
esac
pkg_digest "$pkg_sha256"
[ "$pkg_kernel" != Darwin ] || pkg_digest "$PKG_SHA256_MACOS_WRAPPER"
for pkg_tool in curl mktemp tee; do
    command -v "$pkg_tool" >/dev/null 2>&1 || pkg_fail "$pkg_tool is required. Install it, then retry."
done
command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || pkg_fail 'sha256sum or shasum is required.'
if [ "$pkg_mode" = install ]; then
    [ "$(id -u)" -ne 0 ] || pkg_fail 'Run this script as your normal user. It will request administrator access when needed.'
    command -v sudo >/dev/null 2>&1 || pkg_fail 'sudo is required for setup.'
    if [ "$pkg_kernel" = Linux ]; then
        command -v systemctl >/dev/null 2>&1 || pkg_fail 'Linux setup requires systemd.'
        [ -d /run/systemd/system ] || pkg_fail 'Start this script on a Linux host running systemd.'
    fi
fi

umask 077
pkg_tmp=$(mktemp -d "${TMPDIR:-/tmp}/pkg-install.XXXXXXXX")
trap 'rm -rf "$pkg_tmp"' 0
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
printf '\npkg %s · %s\n\n' "${PKG_RELEASE#v}" "$pkg_platform"
pkg_fetch "$pkg_artifact" "$pkg_sha256"
if [ "$pkg_kernel" = Darwin ]; then
    pkg_fetch install-preview.sh "$PKG_SHA256_MACOS_WRAPPER"
fi
if [ "$pkg_mode" = verify ]; then
    pkg_ok 'Downloads verified. No changes were made.'
    exit 0
fi

pkg_log=$(mktemp "${TMPDIR:-/tmp}/pkg-install-log.XXXXXXXX")
pkg_note "Install log: $pkg_log"
pkg_note 'Administrator access is needed to set up pkg.'
# Preserve the installer result without relying on POSIX sh pipefail.
( pkg_status=0
  if [ "$pkg_kernel" = Darwin ]; then
      /bin/bash "$pkg_tmp/install-preview.sh" "$pkg_tmp/$pkg_artifact" "$pkg_sha256" || pkg_status=$?
  else
      chmod 0700 "$pkg_tmp/$pkg_artifact"
      sudo "$pkg_tmp/$pkg_artifact" || pkg_status=$?
  fi
  printf '%s\n' "$pkg_status" >"$pkg_tmp/status"
) 2>&1 | tee "$pkg_log"
pkg_status=$(cat "$pkg_tmp/status")
if [ "$pkg_status" -ne 0 ]; then
    printf '\npkg setup failed (exit %s). Keep this log for support: %s\n' "$pkg_status" "$pkg_log" >&2
    exit "$pkg_status"
fi

# Check the installed program as the original user, with its managed commands on PATH.
pkg_cli=/usr/local/bin/pkg
[ -x "$pkg_cli" ] || pkg_fail "Setup finished, but pkg is missing. Keep this log: $pkg_log"
[ "$("$pkg_cli" --version)" = "pkg ${PKG_RELEASE#v}" ] || pkg_fail "The installed version is not the requested release. Keep this log: $pkg_log"
case "$pkg_kernel" in
    Darwin) pkg_user_bin="$HOME/Library/Application Support/pkg/current/bin" ;;
    Linux) pkg_user_bin="$HOME/.local/share/pkg/current/bin" ;;
esac
pkg_note 'Checking the installation...'
if ! PATH="$pkg_user_bin:/usr/local/bin:$PATH" "$pkg_cli" --no-color doctor; then
    pkg_fail "Setup finished, but a health check failed. Follow the steps above. Keep this log: $pkg_log"
fi
pkg_ok 'pkg is ready.'
# Shell setup is printed for the invoking shell, not evaluated by the installer.
# shellcheck disable=SC2016
printf '%s\n' '' 'Open a new terminal, or run:' '  eval "$(/usr/local/bin/pkg shellenv)"' \
    '' 'Start here:' '  pkg search ripgrep' '  pkg install ripgrep' '  pkg list' \
    '' 'Run this same installer command when you want to update pkg.'
