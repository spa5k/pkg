#!/bin/sh
set -eu

die() { printf 'not ok - %s\n' "$*" >&2; exit 1; }
script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
host=$script_dir/run.sh
guest=$script_dir/inside.sh
arm_guest=$script_dir/inside-aarch64-container.sh

sh -n "$host" "$guest" "$arm_guest" "$0"
[ -x "$host" ] && [ -x "$guest" ] && [ -x "$arm_guest" ] && [ -x "$0" ] || die "scripts must be executable"
grep -F 'lifecycle|diagnostics-disabled|crash-recovery|foreign-nix|upstream-input' "$host" >/dev/null || die "exact host lanes missing"
grep -F 'lifecycle|diagnostics-disabled|crash-recovery|foreign-nix|upstream-input' "$guest" >/dev/null || die "exact guest lanes missing"
grep -F -- '--approve-destructive-vm' "$host" >/dev/null || die "approval gate missing"
grep -F '6e40c07ae715f744f84af0bec76415cc1987dd115b4b8de437818561f01a3733' "$host" >/dev/null || die "Ubuntu pin missing"
grep -F 'x86_64-linux' "$host" >/dev/null || die "installer pin lookup missing"
grep -F -- '-F qcow2' "$host" >/dev/null || die "qcow2 backing format missing"
grep -F '30G' "$host" >/dev/null || die "30G overlay missing"
grep -F '16777216' "$host" >/dev/null || die "16 GiB free-space floor missing"
grep -F 'hostfwd=tcp:127.0.0.1:' "$host" >/dev/null || die "localhost-only SSH forwarding missing"
grep -F '[ ! -L "$out" ]' "$host" >/dev/null || die "dangling output symlink check missing"
grep -F '/etc/pkg-s6-disposable-vm' "$host" >/dev/null && grep -F '/etc/pkg-s6-disposable-vm' "$guest" >/dev/null || die "random VM marker missing"
grep -F 'systemd-detect-virt' "$guest" >/dev/null || die "VM check missing"
grep -F 'QEMU|Standard PC' "$guest" >/dev/null || die "QEMU DMI check missing"
grep -F '[ ! -w "$base" ]' "$host" >/dev/null || die "read-only base check missing"
grep -F 'chmod 0700 "$staged"' "$guest" >/dev/null || die "private staged installer missing"
grep -F '"$(sha256 "$staged")"' "$guest" >/dev/null || die "post-stage hash missing"
grep -F -- "--diagnostic-endpoint '' install" "$guest" >/dev/null || die "empty diagnostic argument missing"
grep -F 'DETSYS_IDS_TRANSPORT=$transport' "$guest" >/dev/null || die "disabled diagnostics transport proof missing"
grep -F 'exec "$staged"' "$guest" >/dev/null && die "generic staged executor is forbidden"
grep -E 'command -v[[:space:]]+nix-installer|exec[[:space:]]+nix-installer' "$host" "$guest" >/dev/null && die "PATH installer lookup found"
grep -E 'curl[^|]*\|[[:space:]]*(sh|bash)|wget[^|]*\|[[:space:]]*(sh|bash)' "$host" "$guest" >/dev/null && die "download-to-shell found"
grep -F 'find "$1" -type d -exec chmod 0700' "$host" >/dev/null || die "private host directories missing"
grep -F 'find "$1" -type f -exec chmod 0600' "$host" >/dev/null || die "private host files missing"
grep -F "'timeout 300 cloud-init status --wait'" "$host" >/dev/null || die "cloud-init wait is not bounded"
grep -F 'timeout 120 systemctl is-system-running --wait' "$guest" >/dev/null || die "systemd wait is not bounded"
grep -F 'guest-evidence.tar' "$host" >/dev/null && grep -F 'guest-evidence/results' "$host" >/dev/null || die "evidence transfer cannot prove completion"
grep -F 'UNPROVED: $lane' "$host" >/dev/null || die "UNPROVED host outcome missing"
grep -F 'timeout --kill-after=60 7200' "$host" >/dev/null || die "guest lane timeout missing"
grep -F 'timeout --preserve-status' "$host" >/dev/null && die "timeout can hide expiry behind a child status"
grep -F 'product-git-revision' "$host" >/dev/null || die "product revision evidence missing"
grep -F '4132ad07a15ee7d88c096ac7172b7afb2672866b' "$host" >/dev/null || die "vendor revision evidence missing"
grep -F 'runner worktree must be clean' "$host" >/dev/null || die "clean worktree gate missing"
grep -F 'boot-id.before' "$guest" >/dev/null && grep -F 'boot-id.after' "$guest" >/dev/null || die "clean reboot boot-ID proof missing"
grep -F '/run/reboot-required' "$guest" >/dev/null && die "lifecycle still depends on reboot-required"
grep -F "/nix/nix-installer --diagnostic-endpoint '' repair --no-confirm" "$guest" >/dev/null || die "default repair argv missing"
grep -F "/nix/nix-installer --diagnostic-endpoint '' repair sequoia --no-confirm" "$guest" >/dev/null || die "sequoia refusal argv missing"
grep -F "grep -F 'only available on macOS'" "$guest" >/dev/null || die "sequoia refusal message gate missing"
grep -F 'for command_name in update upgrade self-update' "$guest" >/dev/null || die "installer update refusal probes missing"
grep -F '[ "$command_rc" -eq 0 ] || ! grep -Ei' "$guest" >/dev/null || die "installer update status and output gate missing"
grep -F 'required pinned same-version daemon upgrade' "$guest" >/dev/null || die "required daemon upgrade probe missing"
grep -F 'S6_ENABLE_PINNED_UPGRADE_PROBE' "$host" "$guest" >/dev/null && die "optional daemon upgrade gate remains"
grep -F "/nix/nix-installer --diagnostic-endpoint '' uninstall --no-confirm /nix/receipt.json" "$guest" >/dev/null || die "installed-copy uninstall argv missing"
grep -F 'uninstall observations satisfy the pinned residue contract' "$guest" >/dev/null || die "uninstall residue gate missing"
grep -F "'^nixbld[0-9]+$'" "$guest" >/dev/null || die "all nixbld users are not checked"
grep -F 'find /usr/local/bin -maxdepth 1' "$guest" >/dev/null || die "bounded local-bin residue scan missing"
grep -F '[ ! -L "$path" ]' "$guest" >/dev/null || die "dangling named residue is not checked"
grep -F "etc_nix_mode_owner\" = '755 root:root'" "$guest" >/dev/null || die "empty /etc/nix contract missing"
grep -F 'for path in /nix/receipt.json /nix /usr/local/bin/determinate-nixd' "$guest" >/dev/null || die "/etc/nix is not separated from forbidden residue"
grep -F 'find /etc/nix -mindepth 1 -print -quit' "$guest" >/dev/null || die "/etc/nix entry evidence missing"
grep -F 'repeat_uninstall_rc" -eq 1' "$guest" >/dev/null || die "repeat uninstall pinned status missing"
grep -F "grep -F 'Reading receipt'" "$guest" >/dev/null || die "repeat uninstall receipt error proof missing"
grep -F "grep -F 'No such file or directory'" "$guest" >/dev/null || die "repeat uninstall missing-path proof missing"
for counter in diagnostic-initial-requests diagnostic-repeat-requests diagnostic-disabled-requests; do
    grep -F "$counter" "$guest" >/dev/null || die "stable diagnostic counter missing: $counter"
done
grep -F 'repeat_requests=$(cat "$evidence/diagnostic-repeat-requests")' "$guest" >/dev/null || die "repeat diagnostic counter is not read"
grep -F 'capture_sentry_identity()' "$guest" >/dev/null || die "sentry identity helper missing"
grep -F 'if [ -L "$sentry" ]' "$guest" >/dev/null || die "sentry symlink is not classified first"
grep -F "sentry_stat_format='type=%F numeric-owner=%u:%g named-owner=%U:%G mode=0%a size=%s path=%n'" "$guest" >/dev/null || die "sentry no-follow stat evidence incomplete"
grep -F 'readlink -- "$sentry"' "$guest" >/dev/null || die "sentry link target evidence missing"
grep -F 'stat -L' "$guest" >/dev/null && die "sentry stat can follow links"
grep -F 'sha256 "$sentry"' "$guest" >/dev/null && die "sentry hash can follow links"
grep -F 'cp -P -- "$sentry" "$prefix.bytes"' "$guest" >/dev/null || die "sentry regular file copy can follow links"
grep -F 'readlink -- "$sentry" >"$prefix.link-target" || die' "$guest" >/dev/null || die "sentry link evidence can fail open"
grep -F 'cp -P -- "$sentry" "$prefix.bytes" || die' "$guest" >/dev/null || die "sentry byte copy can fail open"
grep -F 'sha256sum -- "$prefix.bytes"' "$guest" >/dev/null || die "sentry byte-copy hash missing"
grep -F 'chmod 0600 "$prefix.bytes"' "$guest" >/dev/null || die "sentry byte copy is not private"
for sentry_stage in before-initial after-initial after-determinate-nixd-upgrade after-uninstall; do
    grep -F "capture_sentry_identity $sentry_stage" "$guest" >/dev/null || die "sentry stage missing: $sentry_stage"
done
[ "$(grep -c '^[[:space:]]*capture_sentry_identity ' "$guest")" -eq 4 ] || die "sentry capture must run at exactly four stages"
grep -F 'determinate-nixd plus non-empty Nix store' "$guest" >/dev/null || die "late crash marker missing"
grep -F 'while [ "$i" -lt 1800 ]' "$guest" >/dev/null || die "late crash marker wait is not bounded for slow TCG"
grep -F '${capture_pid:-0}' "$guest" >/dev/null && die "capture stop can still signal process group 0"
drains=$(grep -c '^[[:space:]]*sleep 2$' "$guest")
[ "$drains" -ge 3 ] || die "diagnostic capture drains missing"
git check-ignore -q "$script_dir/evidence/raw-private.log" || die "raw evidence is not ignored"
if git check-ignore -q "$script_dir/evidence/README.md"; then die "evidence README is ignored"; fi
grep -F 'installer version recorded' "$guest" >/dev/null || die "common installer version evidence missing"
grep -F '2|124|137|143)' "$host" >/dev/null || die "timeout statuses are not UNPROVED"
grep -F 'DETSYS_IDS_TELEMETRY=disabled' "$arm_guest" >/dev/null || die "aarch64 telemetry-disable policy missing"
grep -F 'endpoint=http://127.0.0.1:18080' "$arm_guest" >/dev/null || die "aarch64 loopback endpoint missing"
grep -F '[ "$(id -u)" -eq 0 ]' "$arm_guest" >/dev/null && grep -F '[ "$(uname -s)" = Linux ]' "$arm_guest" >/dev/null || die "aarch64 root or Linux gate missing"
grep -F '[ ! -e "$evidence/results" ] && [ ! -L "$evidence/results" ]' "$arm_guest" >/dev/null || die "aarch64 fresh-evidence gate missing"
grep -F 'diagnostic-install.requests' "$arm_guest" >/dev/null && grep -F 'diagnostic-total.requests' "$arm_guest" >/dev/null || die "aarch64 zero-request evidence missing"
[ "$(grep -c 'kill -0 "$canary_pid"' "$arm_guest")" -eq 3 ] || die "aarch64 canary liveness gates missing"
grep -F 'sha256 "$receipt" >"$evidence/receipt.sha256"' "$arm_guest" >/dev/null || die "aarch64 private receipt digest missing"
grep -F 'links=%h' "$arm_guest" >/dev/null || die "aarch64 link-count evidence missing"
grep -F 'cp "$receipt"' "$arm_guest" >/dev/null && die "aarch64 receipt contents can be archived"
grep -F 'cat "$receipt"' "$arm_guest" >/dev/null && die "aarch64 receipt contents can be printed"
grep -F 'sha256 "$sentry" >"$prefix.sha256"' "$arm_guest" >/dev/null || die "aarch64 private sentry digest missing"
grep -F 'strict clean-uninstall residue contract' "$arm_guest" >/dev/null || die "aarch64 strict residue gate missing"

base_hash_line=$(grep -n 'sha256 "$base"' "$host" | head -1 | cut -d: -f1)
qemu_line=$(grep -n '^qemu-system-x86_64 ' "$host" | cut -d: -f1)
[ "$base_hash_line" -lt "$qemu_line" ] || die "base hash must precede QEMU"
stage_copy_line=$(grep -n '^cp "$installer" "$staged"' "$guest" | cut -d: -f1)
stage_hash_line=$(grep -n 'sha256 "$staged"' "$guest" | head -1 | cut -d: -f1)
first_exec_line=$(grep -n '"$staged" --version' "$guest" | head -1 | cut -d: -f1)
lane_case_line=$(grep -n '^case \$lane in' "$guest" | tail -1 | cut -d: -f1)
[ "$stage_copy_line" -lt "$stage_hash_line" ] && [ "$stage_hash_line" -lt "$first_exec_line" ] && [ "$first_exec_line" -lt "$lane_case_line" ] || die "stage/hash/version/lane order is unsafe"
sentry_before_line=$(grep -n 'capture_sentry_identity before-initial' "$guest" | cut -d: -f1)
install_line=$(grep -n '^        "\$staged" --diagnostic-endpoint "\$endpoint" install ' "$guest" | head -1 | cut -d: -f1)
sentry_after_install_line=$(grep -n 'capture_sentry_identity after-initial' "$guest" | cut -d: -f1)
upgrade_line=$(grep -n '^        "\$nixd" upgrade --version v3.22.1 ' "$guest" | cut -d: -f1)
sentry_after_upgrade_line=$(grep -n 'capture_sentry_identity after-determinate-nixd-upgrade' "$guest" | cut -d: -f1)
uninstall_line=$(grep -n '^    /nix/nix-installer --diagnostic-endpoint.* uninstall ' "$guest" | cut -d: -f1)
sentry_after_uninstall_line=$(grep -n 'capture_sentry_identity after-uninstall' "$guest" | cut -d: -f1)
[ "$sentry_before_line" -lt "$install_line" ] && [ "$install_line" -lt "$sentry_after_install_line" ] && [ "$sentry_after_install_line" -lt "$upgrade_line" ] && [ "$upgrade_line" -lt "$sentry_after_upgrade_line" ] && [ "$sentry_after_upgrade_line" -lt "$uninstall_line" ] && [ "$uninstall_line" -lt "$sentry_after_uninstall_line" ] || die "sentry lifecycle stage order is wrong"

tmp=${TMPDIR:-/tmp}/pkg-s6-static.$$
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
mkdir -m 0700 "$tmp"
if "$host" --approve-destructive-vm /no/base /no/installer unknown "$tmp/out" >"$tmp/out.log" 2>&1; then
    die "unsupported lane was accepted"
fi
grep -F 'unsupported lane: unknown' "$tmp/out.log" >/dev/null || die "unsupported lane did not fail first"

changed=$(git -C "$script_dir/../../.." status --porcelain=v1 --untracked-files=all | awk '
    { path = substr($0, 4) }
    path !~ /^spikes\/s6-determinate-installer\/linux-vm\// && path != "spikes/s6-determinate-installer/.gitignore" { print path }
')
[ -z "$changed" ] || die "production or accepted DN-03a files changed: $changed"
printf '%s\n' 'ok - linux destructive VM harness static contract'
