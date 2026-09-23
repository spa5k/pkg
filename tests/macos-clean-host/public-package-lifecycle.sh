#!/bin/bash
# Run only inside a disposable Tart guest with a working pkg installation.
set -euo pipefail
case "$(/usr/sbin/sysctl -n hw.model)" in VirtualMac*) ;; *) echo 'Use a disposable macOS VM.' >&2; exit 64 ;; esac
[ "$(/usr/bin/id -u)" -ne 0 ] || { echo 'Run as the VM test user.' >&2; exit 64; }
pkg=${1:-/usr/local/bin/pkg}
[ -x "$pkg" ] || exit 66
state="$HOME/Library/Application Support/pkg"

"$pkg" --yes install fzf
printf 'apple\nbanana\n' | "$state/current/bin/fzf" --filter=banana | /usr/bin/grep -Fx banana
"$pkg" --yes remove fzf
test ! -e "$state/current/bin/fzf"
"$pkg" --yes rollback
printf 'apple\nbanana\n' | "$state/current/bin/fzf" --filter=banana | /usr/bin/grep -Fx banana

# Damage only the known disposable fzf store file, then exercise real package repair.
fzf=$(/usr/bin/python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$state/current/bin/fzf")
case "$fzf" in /nix/store/*-fzf-*/bin/fzf) ;; *) echo 'Unexpected fzf path.' >&2; exit 1 ;; esac
before=$(/usr/bin/shasum -a 256 "$fzf")
/usr/bin/sudo /bin/chmod u+w "$fzf"
printf 'damaged by the disposable VM test\n' | /usr/bin/sudo /usr/bin/tee "$fzf" >/dev/null
if "$pkg" repair --verify-only; then
    echo 'Verification did not detect the damaged file.' >&2
    exit 1
fi
"$pkg" --yes repair
[ "$(/usr/bin/shasum -a 256 "$fzf")" = "$before" ]
printf 'apple\nbanana\n' | "$state/current/bin/fzf" --filter=banana | /usr/bin/grep -Fx banana
"$pkg" repair --verify-only
"$pkg" --json list
"$pkg" --json history
echo 'PASS: cached install, remove, rollback, damage detection, and package repair'
