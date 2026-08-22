#!/bin/sh
set -eu

die() { printf 'not ok - %s\n' "$*" >&2; exit 1; }
script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
host=$script_dir/run.sh
guest=$script_dir/inside.sh

sh -n "$host" "$guest" "$0"
[ -x "$host" ] && [ -x "$guest" ] && [ -x "$0" ] || die "scripts must be executable"
grep -F '[ "$#" -eq 3 ]' "$host" >/dev/null && grep -F -- '--approve-destructive-vm ABS_INSTALLER ABS_NEW_EVIDENCE' "$host" >/dev/null || die "exact host arguments missing"
grep -F '[ "$(uname -s)" = Darwin ]' "$host" >/dev/null && grep -F '[ "$(uname -m)" = arm64 ]' "$host" >/dev/null || die "host platform gates missing"
grep -F 'runner worktree must be clean' "$host" >/dev/null || die "clean worktree gate missing"
grep -F '[ -f "$installer" ] && [ ! -L "$installer" ]' "$host" >/dev/null || die "installer path gate missing"
grep -F '[ ! -L "$out" ]' "$host" >/dev/null && grep -F '[ ! -e "$out" ]' "$host" >/dev/null || die "new non-symlink evidence gate missing"
grep -F 'path must be canonical and contain no symlinks' "$host" >/dev/null || die "path-component symlink gates missing"
grep -F '16777216' "$host" >/dev/null || die "16 GiB free-space gate missing"
grep -F '90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b' "$host" >/dev/null || die "installer pin missing"
grep -F 'ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c' "$host" >/dev/null || die "base pin missing"
grep -F '4132ad07a15ee7d88c096ac7172b7afb2672866b' "$host" >/dev/null || die "vendor pin missing"
grep -F 'export TART_NO_AUTO_PRUNE=1' "$host" >/dev/null || die "automatic pruning is not disabled"
grep -F 'has_exact_vm "$base"' "$host" >/dev/null && grep -F 'pinned base is not cached; refusing to pull' "$host" >/dev/null || die "exact cached-base gate missing"
grep -F 'vm_name=pkg-s6-dn03c-preflight-$token' "$host" >/dev/null && grep -F 'collision_status' "$host" >/dev/null && grep -F 'generated VM name already exists' "$host" >/dev/null || die "unique exact VM collision gate missing"
grep -F 'product-git-revision' "$host" >/dev/null && grep -F 'vendor-full-revision' "$host" >/dev/null && grep -F 'host.txt' "$host" >/dev/null && grep -F 'vm-name' "$host" >/dev/null || die "host evidence is incomplete"
grep -F 'find "$1" -type d -exec chmod 0700' "$host" >/dev/null && grep -F 'find "$1" -type f -exec chmod 0600' "$host" >/dev/null || die "private evidence modes missing"

[ "$(grep -c '^tart clone ' "$host")" -eq 1 ] || die "clone count is not exactly one"
[ "$(grep -c '^tart run ' "$host")" -eq 1 ] || die "run count is not exactly one"
grep -F 'tart run "$vm_name" --no-graphics --no-audio --no-clipboard --no-keyboard --no-pointer --net-softnet' "$host" >/dev/null || die "safe Tart run flags missing"
grep -F 'tart exec -i "$vm_name"' "$host" >/dev/null || die "Guest Agent stdin execution missing"
grep -F 'while [ "$i" -lt 60 ]' "$host" >/dev/null && grep -F 'if [ "$elapsed" -ge "$limit" ]' "$host" >/dev/null && grep -F 'wait_pid 60 "$delete_pid"' "$host" >/dev/null || die "bounded waits missing"
grep -F '/usr/bin/sudo -n /usr/bin/true' "$host" >/dev/null || die "passwordless sudo proof missing"
grep -F '/usr/sbin/chown root:wheel "$dir"' "$host" >/dev/null && grep -F '/bin/chmod 0600 "$marker"' "$host" >/dev/null || die "root-owned private marker missing"
grep -F '/bin/chmod 0600 "$1"' "$host" >/dev/null && grep -F '<"$installer"' "$host" >/dev/null || die "private streamed installer missing"
grep -F '<"$script_dir/inside.sh"' "$host" >/dev/null || die "inside.sh is not streamed"
grep -F 'sed -n '\''1p'\'' "$out/vm-owner"' "$host" >/dev/null && grep -F 'sed -n '\''2p'\'' "$out/vm-owner"' "$host" >/dev/null || die "private ownership record is not checked"
grep -F 'tart stop "$vm_name"' "$host" >/dev/null && grep -F 'tart delete "$vm_name"' "$host" >/dev/null && grep -F 'has_exact_vm "$vm_name"' "$host" >/dev/null || die "exact cleanup and absence proof missing"
grep -F 'ownership record mismatch; VM preserved' "$host" >/dev/null || die "cleanup failure does not preserve ownership record"
grep -F '[ "$cleanup_ok" -eq 1 ] || exit 1' "$host" >/dev/null || die "cleanup failure is not fatal"
pass_line=$(grep -n "PASS: macOS VM preflight" "$host" | cut -d: -f1)
delete_line=$(grep -n 'tart delete "$vm_name"' "$host" | cut -d: -f1)
[ "$pass_line" -gt "$delete_line" ] || die "PASS can precede cleanup"

for forbidden in 'tart pull' 'tart prune' ' ssh ' ' scp ' 'sudo tart' '--dir' '--disk' '--rosetta' 'rm -rf' 'tart delete --all'; do
    grep -F -- "$forbidden" "$host" "$guest" >/dev/null && die "forbidden text found: $forbidden"
done
grep -E '(^|[[:space:]])(ssh|scp)([[:space:]]|$)' "$host" "$guest" >/dev/null && die "SSH or scp command found"
grep -E '(^|[[:space:]])sudo[[:space:]]+tart([[:space:]]|$)' "$host" >/dev/null && die "host sudo found"
[ "$(grep -c 'tart exec -i' "$host")" -eq 1 ] || die "all guest execution must use the bounded wrapper"

grep -F '[ "$(id -u)" -eq 0 ]' "$guest" >/dev/null || die "guest root gate missing"
grep -F '[ "$(uname -s)" = Darwin ]' "$guest" >/dev/null && grep -F '[ "$(uname -m)" = arm64 ]' "$guest" >/dev/null || die "guest platform gates missing"
grep -F 'sysctl -n kern.hv_vmm_present' "$guest" >/dev/null && grep -F 'VirtualMac*' "$guest" >/dev/null || die "guest virtualization gates missing"
grep -F 'guest ownership marker does not match' "$guest" >/dev/null && grep -F 'root:wheel:600' "$guest" >/dev/null || die "guest marker gate missing"
grep -F 'staged installer digest mismatch' "$guest" >/dev/null && grep -F 'shasum -a 256 "$staged"' "$guest" >/dev/null || die "guest staged-hash gate missing"
grep -F 'sw_vers' "$guest" >/dev/null && grep -F 'df -Pk /' "$guest" >/dev/null || die "guest platform evidence missing"
grep -F '/nix /nix/receipt.json /nix/nix-installer /usr/local/bin/determinate-nixd /etc/nix' "$guest" >/dev/null || die "named baseline paths missing"
grep -F 'Nix Store APFS volume exists' "$guest" >/dev/null || die "APFS volume gate missing"
grep -F '/etc/fstab /etc/synthetic.conf' "$guest" >/dev/null || die "fstab and synthetic.conf gates missing"
grep -F '/Library/LaunchDaemons /Library/LaunchAgents' "$guest" >/dev/null && grep -F 'launchctl print system' "$guest" >/dev/null || die "launchd baseline gates missing"
grep -F '/Groups/nixbld' "$guest" >/dev/null && grep -F "'^_?nixbld[0-9]+$'" "$guest" >/dev/null || die "nixbld gates missing"
grep -E '(cat|sed|awk|head|tail|less|more)[[:space:]].*/nix/receipt\.json' "$guest" >/dev/null && die "receipt content is read"
grep -E '(^|[[:space:]])(rm|mv|install|mount|diskutil[[:space:]]+(erase|delete|add|rename)|launchctl[[:space:]]+(load|unload|bootstrap|bootout))([[:space:]]|$)' "$guest" >/dev/null && die "guest mutates Nix or system state"
grep -E '^[[:space:]]*(exec[[:space:]]+)?("?\$staged"?|/[^[:space:]]*/nix-installer)([[:space:]]|$)' "$guest" >/dev/null && die "installer can execute"

hash_line=$(grep -n '^actual_installer_sha=$(sha256 "$installer")' "$host" | cut -d: -f1)
clone_line=$(grep -n '^tart clone ' "$host" | cut -d: -f1)
[ "$hash_line" -lt "$clone_line" ] || die "installer hash does not precede clone"
printf '%s\n' 'ok - macOS Tart preflight static contract'
