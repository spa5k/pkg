#!/bin/sh
set -eu

die() { printf 'not ok - %s\n' "$*" >&2; exit 1; }
script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
host=$script_dir/run.sh
guest=$script_dir/inside.sh

need() { grep -F -- "$2" "$1" >/dev/null || die "$3"; }
reject() { reject_pattern=$1 reject_message=$2; shift 2; for reject_file in "$@"; do grep -E -- "$reject_pattern" "$reject_file" >/dev/null && die "$reject_message"; done; return 0; }
line() { grep -n -F -- "$2" "$1" | head -1 | cut -d: -f1; }
graph() {
    actual=$(sed -n "/^    $1)\$/,/^        ;;/p" "$host" | sed -n -e 's/^        run_phase /phase /p' -e 's/^        reboot_guest /reboot /p')
    [ "$actual" = "$2" ] || die "$1 execution graph changed"
}

for script in "$host" "$guest" "$0"; do sh -n "$script"; done
[ -x "$host" ] && [ -x "$guest" ] && [ -x "$0" ] || die "scripts must be executable"

# Exact interfaces and lane graphs.
need "$host" '[ "$#" -eq 7 ] || [ "$#" -eq 8 ] || die "usage: $0 --approve-destructive-vm --lane LANE --installer ABS --evidence ABS_NEW [--approve-observe-vendor-foreign-state]"' "host CLI changed"
need "$guest" 'case $# in 6|7) ;; *) die "usage: inside.sh PHASE TOKEN MARKER STAGED_INSTALLER INSTALLER_SHA256 GUEST_EVIDENCE_DIR [APPROVAL]" ;; esac' "guest argument count changed"
need "$host" "lifecycle-diagnostics) printf '%s\\n' baseline lifecycle-install reboot lifecycle-post-reboot lifecycle-repeat-install lifecycle-repair lifecycle-daemon lifecycle-uninstall lifecycle-repeat-uninstall reboot lifecycle-residue" "lifecycle graph changed"
need "$host" "crash-recovery) printf '%s\\n' baseline crash-kill reboot crash-recover" "crash graph changed"
need "$host" "printf '%s\\n' baseline foreign-synthetic-prepare reboot foreign-post-reboot foreign-refuse" "foreign graph changed"
need "$host" "upstream-input) printf '%s\\n' baseline upstream-install upstream-determinate-attempt" "upstream graph changed"
graph lifecycle-diagnostics 'phase baseline
phase lifecycle-install
reboot after-install
phase lifecycle-post-reboot
phase lifecycle-repeat-install
phase lifecycle-repair
phase lifecycle-daemon
phase lifecycle-uninstall
phase lifecycle-repeat-uninstall
reboot after-uninstall
phase lifecycle-residue'
graph crash-recovery 'phase baseline
phase crash-kill
reboot after-crash
phase crash-recover'
graph foreign-nix 'phase baseline
phase foreign-synthetic-prepare
reboot after-foreign-prepare
phase foreign-post-reboot
phase foreign-refuse'
need "$host" '[ -z "$foreign_approval" ] || run_phase foreign-observe "$foreign_approval"' "optional foreign observation changed"
graph upstream-input 'phase baseline
phase upstream-install
phase upstream-determinate-attempt'

# Capacity, pins, and default Tart NAT.
need "$host" '[ "$available_kb" -ge 33554432 ] || die "at least 32 GiB of free disk is required"' "32 GiB evidence-volume gate missing"
need "$host" '[ "$tart_available_kb" -ge 33554432 ] || die "at least 32 GiB of free Tart storage is required"' "32 GiB Tart-volume gate missing"
need "$guest" '[ "$vendor_available_kb" -ge 31457280 ] || die "at least 30 GiB of guest free disk is required before first vendor execution"' "30 GiB guest vendor gate missing"
for phase in lifecycle-install crash-kill foreign-observe upstream-install; do
    block=$(sed -n "/^    $phase)/,/^        ;;/p" "$guest")
    printf '%s\n' "$block" | grep -F 'require_first_vendor_gates' >/dev/null || die "first-vendor gate missing for $phase"
done
need "$host" '90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b' "installer pin missing"
need "$host" '4132ad07a15ee7d88c096ac7172b7afb2672866b' "vendor provenance missing"
need "$host" 'ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c' "base pin missing"
need "$host" '[ "$tart_version" = 2.35.0 ]' "Tart version pin missing"
need "$host" 'export TART_NO_AUTO_PRUNE=1' "Tart auto-prune guard missing"
need "$host" 'tart run --no-graphics --no-audio --no-clipboard --no-keyboard --no-pointer "$vm_name"' "default-NAT Tart argv changed"
reject '--net-(softnet|host|bridged)' "explicit Tart networking is forbidden" "$host"
evidence_gate_line=$(line "$host" '[ "$available_kb" -ge 33554432 ]')
tart_gate_line=$(line "$host" '[ "$tart_available_kb" -ge 33554432 ]')
clone_line=$(grep -n -F -x 'bounded_host 600 tart clone "$base" "$vm_name" >>"$out/tart.log" 2>&1' "$host" | head -1 | cut -d: -f1)
[ "$evidence_gate_line" -lt "$clone_line" ] && [ "$tart_gate_line" -lt "$clone_line" ] || die "host capacity gates do not precede clone"
need "$host" 'write_argv "$out/vm-resize.argv" tart set "$vm_name" --disk-size 80' "VM resize argv record missing"
need "$host" 'bounded_host 60 tart set "$vm_name" --disk-size 80 >>"$out/tart.log" 2>&1 || die "could not resize cloned VM"' "bounded VM resize missing"
[ "$(grep -E -c '^[[:space:]]*write_argv "\$out/vm-resize\.argv" tart set "\$vm_name" --disk-size ' "$host")" -eq 1 ] || die "lane must record exactly one VM disk resize"
[ "$(grep -F -x -c 'write_argv "$out/vm-resize.argv" tart set "$vm_name" --disk-size 80' "$host")" -eq 1 ] || die "exact VM resize argv record must be active exactly once"
[ "$(grep -E -c '^[[:space:]]*bounded_host [0-9]+ tart set "\$vm_name" --disk-size ' "$host")" -eq 1 ] || die "lane must resize exactly one VM disk"
[ "$(grep -F -x -c 'bounded_host 60 tart set "$vm_name" --disk-size 80 >>"$out/tart.log" 2>&1 || die "could not resize cloned VM"' "$host")" -eq 1 ] || die "exact VM resize command must be active exactly once"
created_line=$(grep -n -F -x 'created=1' "$host" | head -1 | cut -d: -f1)
resize_argv_line=$(grep -n -F -x 'write_argv "$out/vm-resize.argv" tart set "$vm_name" --disk-size 80' "$host" | head -1 | cut -d: -f1)
resize_line=$(grep -n -F -x 'bounded_host 60 tart set "$vm_name" --disk-size 80 >>"$out/tart.log" 2>&1 || die "could not resize cloned VM"' "$host" | head -1 | cut -d: -f1)
run_argv_line=$(grep -n -F -x 'write_argv "$out/vm-run.argv" tart run --no-graphics --no-audio --no-clipboard --no-keyboard --no-pointer "$vm_name"' "$host" | head -1 | cut -d: -f1)
run_line=$(grep -n -F -x 'tart run --no-graphics --no-audio --no-clipboard --no-keyboard --no-pointer "$vm_name" >>"$out/tart.log" 2>&1 &' "$host" | head -1 | cut -d: -f1)
[ "$clone_line" -lt "$created_line" ] && [ "$created_line" -lt "$resize_argv_line" ] && [ "$resize_argv_line" -lt "$resize_line" ] && [ "$resize_line" -lt "$run_argv_line" ] && [ "$run_argv_line" -lt "$run_line" ] || die "clone/resize/run order changed"

# Explicit stdin transport. Callers pass a file argument; only the async child redirects it.
need "$host" 'stdin_file=$2' "bounded_exec stdin file missing"
need "$host" 'tart exec -i "$vm_name" "$@" <"$stdin_file" &' "async stdin transport changed"
need "$host" 'bounded_exec 60 "$installer" /usr/bin/sudo -n /bin/sh -c' "installer upload input is not explicit"
need "$host" 'bounded_exec 30 "$out/inside.sh" /usr/bin/sudo -n /bin/sh -c' "inside upload input is not explicit"
reject 'bounded_exec .*<' "caller-level upload redirection found" "$host"
reject '(\|[[:space:]]*tart exec|tart exec.*\|)' "pipe into tart exec found" "$host"
reject '<&0' "bare inherited stdin found" "$host"

# Reboot proof is ready, then down, then ready, with raw boot-time change.
need "$host" 'Guest Agent did not become unavailable for reboot' "reboot down proof missing"
need "$host" 'wait_guest_ready' "post-reboot ready proof missing"
need "$host" '/usr/sbin/sysctl -n kern.boottime >"$out/reboots/$label.before"' "pre-reboot boot time missing"
need "$host" '/usr/sbin/sysctl -n kern.boottime >"$out/reboots/$label.after"' "post-reboot boot time missing"
need "$host" 'cmp -s "$out/reboots/$label.before" "$out/reboots/$label.after" && die' "raw boot-time comparison missing"
down_line=$(line "$host" 'Guest Agent did not become unavailable for reboot')
ready_line=$(grep -n -F '    wait_guest_ready' "$host" | tail -1 | cut -d: -f1)
[ "$down_line" -lt "$ready_line" ] || die "reboot ready/down/ready order changed"

# Foreign observation needs both the destructive approval and the exact second approval.
need "$host" '[ "$1" = --approve-destructive-vm ]' "destructive approval missing"
need "$host" '[ "$8" = --approve-observe-vendor-foreign-state ]' "host foreign approval missing"
need "$host" 'foreign_approval=approve-observe-vendor-foreign-state' "guest approval token changed"
need "$guest" '[ "$#" -eq 7 ] && [ "$approval" = approve-observe-vendor-foreign-state ]' "guest foreign approval missing"

# Phase archives validate a private part, hash it, then atomically finalize it before status classification.
need "$host" 'archive_part=$out/phases/$phase.tar.part' "partial archive missing"
need "$host" 'validate_phase_archive "$phase" "$archive_part"' "archive validation missing"
need "$host" 'sha256 "$archive_part" >"$out/phases/$phase.tar.sha256"' "archive digest missing"
need "$host" '/bin/mv "$archive_part" "$archive"' "atomic archive finalization missing"
need "$host" 'phase archive contains a link or special entry' "archive type rejection missing"
need "$host" 'phase archive has duplicate paths' "archive duplicate rejection missing"
need "$host" 'phase archive has an unexpected prefix' "archive prefix rejection missing"
validate_line=$(line "$host" 'validate_phase_archive "$phase" "$archive_part"')
hash_line=$(line "$host" 'sha256 "$archive_part"')
rename_line=$(line "$host" '/bin/mv "$archive_part" "$archive"')
classify_line=$(line "$host" 'case $phase:$guest_status in')
[ "$validate_line" -lt "$hash_line" ] && [ "$hash_line" -lt "$rename_line" ] && [ "$rename_line" -lt "$classify_line" ] || die "archive finalization does not precede status classification"
need "$host" 'foreign-refuse:20)' "semantic status 20 missing"
need "$host" 'phase-status.fail.expected' "failed phase evidence is not classified"

# Receipt contents stay opaque. Metadata and SHA-256 identity are allowed.
need "$guest" 'receipt_identity()' "opaque receipt identity helper missing"
need "$guest" "stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' \"\$receipt\"" "receipt metadata proof missing"
need "$guest" 'sha256 "$receipt"' "receipt digest proof missing"
need "$host" 'case $entry in */receipt.json) die "phase archive contains receipt bytes"' "host receipt archive rejection missing"
reject '(^|[;&|])[[:space:]]*((/bin|/usr/bin)/)?(cat|cp|dd|grep|head|tail|sed|awk|tar|tee|strings)[[:space:]].*(/nix/receipt\.json|"?\$receipt"?)' "receipt content read or copy found" "$guest"

# Exact vendor argv and observed statuses.
need "$guest" "run_recorded install 7200 \"\$staged\" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile" "install argv changed"
need "$guest" "run_recorded repeat-install 7200 \"\$staged\" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile" "repeat install argv changed"
need "$guest" "run_recorded repair 7200 /nix/nix-installer --diagnostic-endpoint '' repair --no-confirm" "repair argv changed"
need "$guest" "run_recorded repair-sequoia 7200 /nix/nix-installer --diagnostic-endpoint '' repair sequoia --no-confirm" "Sequoia repair argv changed"
need "$guest" "run_recorded uninstall 7200 /nix/nix-installer --diagnostic-endpoint '' uninstall --no-confirm /nix/receipt.json" "uninstall argv changed"
need "$guest" "run_recorded repeat-uninstall 7200 \"\$staged\" --diagnostic-endpoint '' uninstall --no-confirm /nix/receipt.json" "repeat uninstall argv changed"
need "$guest" "run_recorded recover-install 7200 \"\$staged\" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile" "recovery argv changed"
need "$guest" "run_recorded foreign-install 7200 \"\$staged\" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile" "foreign argv changed"
need "$guest" "run_recorded upstream-install 7200 \"\$staged\" --diagnostic-endpoint '' install --prefer-upstream-nix --no-confirm --no-modify-profile" "upstream argv changed"
need "$guest" "run_recorded determinate-attempt 7200 \"\$staged\" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile" "upstream refusal argv changed"
need "$guest" 'run_recorded daemon-version 60 "$daemon" version' "daemon version argv changed"
need "$guest" 'run_recorded daemon-status 60 "$daemon" status' "daemon status argv changed"
need "$guest" 'run_recorded daemon-upgrade-help 60 "$daemon" upgrade --help' "daemon upgrade probe changed"
need "$guest" 'run_recorded daemon-upgrade 7200 "$daemon" upgrade --version v3.22.1' "daemon upgrade argv changed"
need "$guest" '[ "$last_status" -eq 0 ] || die "initial Determinate install failed"' "install status changed"
need "$guest" '[ "$last_status" -eq 0 ] || die "repeat Determinate install failed"' "repeat install status changed"
need "$guest" '[ "$last_status" -eq 0 ] || die "default repair failed"' "repair status changed"
need "$guest" '[ "$last_status" -eq 0 ] || die "Sequoia repair failed"' "Sequoia repair status changed"
need "$guest" '[ "$last_status" -eq 0 ] || die "pinned determinate-nixd upgrade failed"' "daemon upgrade status changed"
need "$guest" '[ "$last_status" -eq 0 ] || die "determinate-nixd version failed"' "daemon version status changed"
need "$guest" '[ "$last_status" -eq 0 ] || die "determinate-nixd status failed"' "daemon status changed"
need "$guest" '[ "$last_status" -eq 0 ] || die "determinate-nixd upgrade help failed"' "daemon upgrade probe status changed"
need "$guest" '[ "$last_status" -eq 0 ] || { printf '\''%s\n'\'' FAIL >"$phase_dir/vendor-outcome"; die "uninstall failed"; }' "uninstall status changed"
need "$guest" '[ "$last_status" -eq 1 ] || die "repeat uninstall did not return the pinned observed status 1"' "repeat uninstall status changed"
need "$guest" '[ "$crash_status" -eq 137 ] || die "SIGKILL did not produce status 137"' "crash status changed"
need "$guest" '[ "$last_status" -eq 0 ] || die "install did not recover after the forced crash"' "recovery status changed"
need "$guest" '[ "$last_status" -eq 0 ] || die "upstream Nix install failed"' "upstream install status changed"
need "$guest" '[ "$last_status" -eq 1 ] || die "Determinate-on-upstream attempt did not return the pinned status 1"' "upstream refusal status changed"
need "$guest" 'printf '\''status=%s\n'\'' "$last_status" >"$phase_dir/vendor-outcome"' "foreign vendor outcome recording changed"
need "$guest" 'phase_exit=20' "foreign refusal status changed"

# Hard deadlines, signal-safe children, and exact cleanup.
need "$host" 'kill -TERM "$child"' "host deadline TERM missing"
need "$host" 'kill -KILL "$child"' "host deadline KILL missing"
need "$host" 'active_child=$!' "host active-child tracking missing"
need "$host" 'bounded_exec 9000 /dev/null' "phase deadline missing"
need "$guest" 'wait_bounded "$run_limit" "$run_pid"' "vendor deadline missing"
need "$guest" 'active_vendor_pid=$run_pid' "vendor child tracking missing"
need "$guest" "trap 'exit 143' TERM" "guest signal cleanup missing"
need "$guest" 'kill -TERM "$wait_child"' "guest deadline TERM missing"
need "$guest" 'kill -KILL "$wait_child"' "guest deadline KILL missing"
need "$host" 'ownership record mismatch; VM preserved' "private cleanup ownership check missing"
need "$host" 'bounded_host 60 tart stop "$vm_name"' "exact VM stop missing"
need "$host" 'bounded_host 60 tart delete "$vm_name"' "exact VM delete missing"
need "$host" 'verified exact VM absence' "cleanup absence proof missing"
need "$host" '[ "$cleanup_ok" -eq 1 ] || exit 1' "cleanup failure does not override success"
need "$host" 'original_status=$? cleanup_active=1' "cleanup signal state missing"
need "$host" "trap '' HUP INT TERM" "cleanup signal hold missing"

# Forbidden transport and execution shapes.
reject '--(net-softnet|net-host|net-bridged|dir|disk|rosetta)([=[:space:]]|$)' "forbidden Tart flag found" "$host"
reject '(^|[[:space:]])(ssh|scp)([[:space:]]|$)' "SSH transport found" "$host" "$guest"
reject '(^|[[:space:]])(/usr/bin/)?sudo([[:space:]]+-n)?[[:space:]]+tart([[:space:]]|$)' "host sudo found" "$host"
reject '(^|[;&|()])[[:space:]]*(nix-installer|determinate-nixd)([[:space:]]|$)' "vendor executable uses PATH lookup" "$guest"
reject '(^|[[:space:]])(which|type[[:space:]]+-[a-zA-Z]*p)([[:space:]]|$)' "PATH lookup helper found" "$guest"
[ "$(grep -F -c 'tart clone ' "$host")" -eq 1 ] || die "lane must clone exactly one VM"
[ "$(grep -c '^tart run ' "$host")" -eq 1 ] || die "lane must run exactly one VM"

# A raw post-phase write failure must finalize failure evidence before cleanup.
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/pkg-dn03c-finalizer.XXXXXX") || die "could not create finalizer fixture"
trap 'rm -R "$fixture_root"' EXIT HUP INT TERM
{
    printf '%s\n' '#!/bin/sh' 'set -eu' 'fixture_root=$1' 'phase_dir=$fixture_root/phase' 'ledger=$fixture_root/phase-ledger' 'active_vendor_pid=' 'capture_pid='
    printf '%s\n' 'cleanup_children() { [ "$(cat "$phase_dir/phase-status")" = FAIL ] && printf "%s\n" after-failure-evidence >"$fixture_root/cleanup-order"; }'
    sed -n '/^finalize_exit() {$/,/^}$/p' "$guest"
    printf '%s\n' 'mkdir "$fixture_root"' 'mkdir "$phase_dir"' 'printf "baseline\ncurrent\n" >"$ledger"' 'trap finalize_exit EXIT' 'mkdir "$phase_dir/post-write"' 'printf "%s\n" must-fail >"$phase_dir/post-write"'
} >"$fixture_root/fixture.sh"
set +e
sh "$fixture_root/fixture.sh" "$fixture_root/state" >/dev/null 2>&1
fixture_status=$?
set -e
[ "$fixture_status" -ne 0 ] || die "post-phase fixture did not fail"
printf '%s\n' FAIL >"$fixture_root/fail.expected"
printf '%s\n' after-failure-evidence >"$fixture_root/cleanup.expected"
cmp -s "$fixture_root/fail.expected" "$fixture_root/state/phase/phase-status" || die "EXIT finalizer did not write exact FAIL status"
cmp -s "$fixture_root/state/phase-ledger" "$fixture_root/state/phase/phase-ledger" || die "EXIT finalizer did not copy the complete phase ledger"
cmp -s "$fixture_root/cleanup.expected" "$fixture_root/state/cleanup-order" || die "EXIT finalizer did not run before cleanup"
rm -R "$fixture_root"
trap - EXIT HUP INT TERM

printf '%s\n' 'ok - destructive macOS lane static shape contract; Tart and the installer were not run'
