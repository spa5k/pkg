#!/bin/sh
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

die() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
residue() { printf 'RESIDUE: %s\n' "$*"; dirty=1; }

[ "$#" -eq 4 ] || die "usage: inside.sh TOKEN MARKER STAGED_INSTALLER EXPECTED_SHA256"
token=$1
marker=$2
staged=$3
expected=$4
[ "$(id -u)" -eq 0 ] || die "guest preflight requires root"
[ "$(uname -s)" = Darwin ] || die "guest must be Darwin"
[ "$(uname -m)" = arm64 ] || die "guest must be arm64"
[ "$(sysctl -n kern.hv_vmm_present)" = 1 ] || die "guest virtualization marker is absent"
case $(sysctl -n hw.model) in VirtualMac*) ;; *) die "guest model is not VirtualMac" ;; esac
[ -f "$marker" ] && [ ! -L "$marker" ] || die "guest ownership marker is missing"
[ "$(stat -f '%Su:%Sg:%Lp' "$marker")" = root:wheel:600 ] || die "guest ownership marker is not private"
[ "$(cat "$marker")" = "$token" ] || die "guest ownership marker does not match"
[ "$(stat -f '%Su:%Sg:%Lp' "$(dirname "$staged")")" = root:wheel:700 ] || die "guest staging directory is not private"
[ -f "$staged" ] && [ ! -L "$staged" ] || die "staged installer is not a regular file"
[ "$(stat -f '%Su:%Sg:%Lp' "$staged")" = root:wheel:600 ] || die "staged installer is not private"
[ "$(shasum -a 256 "$staged" | awk '{print $1}')" = "$expected" ] || die "staged installer digest mismatch"

printf '%s\n' 'guest sw_vers:'
sw_vers
printf '%s\n' 'guest free disk (KiB):'
guest_available_kb=$(df -Pk / | awk 'END {print $4}')
case $guest_available_kb in ''|*[!0-9]*) die "could not determine guest free disk" ;; esac
printf '%s\n' "$guest_available_kb"
[ "$guest_available_kb" -ge 16777216 ] || die "at least 16 GiB of guest free disk is required"

dirty=0
for path in /nix /nix/receipt.json /nix/nix-installer /usr/local/bin/determinate-nixd /etc/nix; do
    if [ -e "$path" ] || [ -L "$path" ]; then residue "$path exists"; fi
done
apfs=$(diskutil apfs list) || die "could not inspect APFS volumes"
if printf '%s\n' "$apfs" | grep -E 'Name:[[:space:]]+Nix Store([[:space:]]|$)' >/dev/null; then
    residue 'Nix Store APFS volume exists'
fi
for file in /etc/fstab /etc/synthetic.conf; do
    if [ -f "$file" ] && grep -Ei '(^|[[:space:]/])(nix|Nix Store)([[:space:]/]|$)' "$file" >/dev/null; then
        residue "$file contains a Nix entry"
    fi
done
for directory in /Library/LaunchDaemons /Library/LaunchAgents; do
    [ -d "$directory" ] || continue
    plists=$(find "$directory" -maxdepth 1 \( -type f -o -type l \) \( -iname '*nix*' -o -iname '*determinate*' \) -print -quit) || die "could not inspect $directory"
    if [ -n "$plists" ]; then
        residue "$directory contains a Nix or Determinate plist"
    fi
done
launchd=$(launchctl print system 2>/dev/null) || die "could not inspect system launchd"
if printf '%s\n' "$launchd" | grep -Ei '(^|[^[:alnum:]_])(nix|determinate)([^[:alnum:]_]|$)' >/dev/null; then
    residue 'system launchd contains a Nix or Determinate job'
fi
set +e
/usr/bin/security find-generic-password -a 'Nix Store' -s 'Nix Store' /Library/Keychains/System.keychain >/dev/null 2>&1
keychain_status=$?
set -e
case $keychain_status in
    0) residue 'Determinate Nix Store System Keychain item exists' ;;
    44) ;;
    *) die "System Keychain probe failed: $keychain_status" ;;
esac
groups=$(dscl . -list /Groups) || die "could not inspect local groups"
if printf '%s\n' "$groups" | grep -Fx nixbld >/dev/null; then residue 'nixbld group exists'; fi
users=$(dscl . -list /Users) || die "could not inspect local users"
if printf '%s\n' "$users" | grep -E '^_?nixbld[0-9]+$' >/dev/null; then residue 'nixbld users exist'; fi
[ "$dirty" -eq 0 ] || die "guest Nix baseline is not clean"
printf '%s\n' 'PASS: clean macOS Nix baseline'
