#!/bin/sh
set -eu

die() { printf 'not ok - %s\n' "$*" >&2; exit 1; }
script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
host=$script_dir/run.sh
guest=$script_dir/inside.sh
set -- /bin/sh
if dash_path=$(command -v dash 2>/dev/null); then
    case $dash_path in
        /*) if [ -x "$dash_path" ] && [ "$dash_path" != /bin/sh ]; then set -- "$@" "$dash_path"; fi ;;
    esac
fi

need() { grep -F -- "$2" "$1" >/dev/null || die "$3"; }
need_exact() { exact_count=$(grep -F -x -c -- "$2" "$1" || :); [ "$exact_count" -eq 1 ] || die "$3"; }
reject() { reject_pattern=$1 reject_message=$2; shift 2; for reject_file in "$@"; do grep -E -- "$reject_pattern" "$reject_file" >/dev/null && die "$reject_message"; done; return 0; }
line() { grep -n -F -- "$2" "$1" | head -1 | cut -d: -f1; }
exact_line() { grep -n -F -x -- "$2" "$1" | head -1 | cut -d: -f1; }
fstab_uuid_line="    installed_fstab=\"UUID=\$(printf '%s\\n' \"\$installed_uuid\" | tr 'ABCDEF' 'abcdef') /nix apfs rw,noatime,noauto,nobrowse,nosuid,owners # Added by the Determinate Nix Installer\""
fstab_uuid_case_is_valid() {
    fstab_case_count=$(grep -F -x -c -- "$fstab_uuid_line" "$1" || :)
    [ "$fstab_case_count" -eq 1 ]
}
recorded_child_line='    (umask 022; exec "$@") </dev/null >"$phase_dir/$run_name.output" 2>&1 &'
recorded_child_unsafe_line='    "$@" </dev/null >"$phase_dir/$run_name.output" 2>&1 &'
recorded_child_boundary_is_valid() {
    recorded_child_count=$(grep -F -x -c -- "$recorded_child_line" "$1" || :)
    recorded_child_unsafe_count=$(grep -F -x -c -- "$recorded_child_unsafe_line" "$1" || :)
    [ "$recorded_child_count" -eq 1 ] && [ "$recorded_child_unsafe_count" -eq 0 ]
}
crash_child_line='        (umask 022; exec "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile) </dev/null >"$phase_dir/install.output" 2>&1 &'
crash_child_unsafe_line='        "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile </dev/null >"$phase_dir/install.output" 2>&1 &'
crash_child_boundary_is_valid() {
    crash_child_count=$(grep -F -x -c -- "$crash_child_line" "$1" || :)
    crash_child_unsafe_count=$(grep -F -x -c -- "$crash_child_unsafe_line" "$1" || :)
    [ "$crash_child_count" -eq 1 ] && [ "$crash_child_unsafe_count" -eq 0 ]
}
archive_validation_line='    validate_phase_archive "$phase" "$archive_part"'
archive_validation_variable_line='    validation_archive=$2'
archive_validation_collision_line='    archive=$2'
archive_validation_boundary_is_valid() {
    archive_validation_count=$(grep -F -x -c -- "$archive_validation_line" "$1" || :)
    archive_validation_variable_count=$(grep -F -x -c -- "$archive_validation_variable_line" "$1" || :)
    archive_validation_collision_count=$(grep -F -x -c -- "$archive_validation_collision_line" "$1" || :)
    [ "$archive_validation_count" -eq 1 ] && [ "$archive_validation_variable_count" -eq 1 ] && [ "$archive_validation_collision_count" -eq 0 ]
}
reboot_compare_error_line='                    *) die "could not compare raw kern.boottime across reboot" ;;'
reboot_compare_error_unsafe_line='                    *) rebooted=1; break ;;'
reboot_equal_line='                :'
reboot_equal_unsafe_line='                rebooted=1; break'
reboot_failure_line='    [ "$rebooted" -eq 1 ] || die "raw kern.boottime did not change before reboot deadline"'
reboot_return_line='    return 0'
reboot_set_plus_e_line='    set +e'
reboot_shutdown_line='    bounded_exec 30 /dev/null /usr/bin/sudo -n /sbin/shutdown -r now >>"$out/reboots/$label.shutdown" 2>&1'
shutdown_status_capture_line='    shutdown_status=$?'
shutdown_timeout_capture_line='    shutdown_timed_out=$wait_timed_out'
shutdown_status_write_line='    printf '\''%s\n'\'' "$shutdown_status" >"$out/reboots/$label.shutdown.status"'
shutdown_timeout_write_line='    printf '\''%s\n'\'' "$shutdown_timed_out" >"$out/reboots/$label.shutdown.timed-out"'
shutdown_pair_allow_line='        0:0|124:1) ;;'
shutdown_pair_accept_124_0_line='        0:0|124:0|124:1) ;;'
shutdown_pair_wildcard_line='        *:*) ;;'
reboot_outcome_fail_line='    printf '\''%s\n'\'' FAIL >"$out/reboots/$label.outcome"'
reboot_outcome_pass_line='    printf '\''%s\n'\'' PASS >"$out/reboots/$label.outcome"'
reboot_before_line='    bounded_exec 15 /dev/null /usr/sbin/sysctl -n kern.boottime >"$out/reboots/$label.before" 2>"$out/reboots/$label.before.stderr" || die "could not record pre-reboot kern.boottime"'
reboot_before_unsafe_line='    bounded_exec 15 /dev/null /usr/sbin/sysctl -n kern.boottime >"$out/reboots/$label.before" 2>&1 || die "could not record pre-reboot kern.boottime"'
expected_reboot_sequence='reboot_guest() {
    label=$1
    printf '\''%s\n'\'' FAIL >"$out/reboots/$label.outcome"
    bounded_exec 15 /dev/null /usr/sbin/sysctl -n kern.boottime >"$out/reboots/$label.before" 2>"$out/reboots/$label.before.stderr" || die "could not record pre-reboot kern.boottime"
    set +e
    bounded_exec 30 /dev/null /usr/bin/sudo -n /sbin/shutdown -r now >>"$out/reboots/$label.shutdown" 2>&1
    shutdown_status=$?
    shutdown_timed_out=$wait_timed_out
    set -e
    printf '\''%s\n'\'' "$shutdown_status" >"$out/reboots/$label.shutdown.status"
    printf '\''%s\n'\'' "$shutdown_timed_out" >"$out/reboots/$label.shutdown.timed-out"
    case "$shutdown_status:$shutdown_timed_out" in
        0:0|124:1) ;;
        *) die "guest shutdown command returned an invalid status/timeout pair" ;;
    esac
    rebooted=0
    i=0
    while [ "$i" -lt 150 ]; do
        kill -0 "$run_pid" 2>/dev/null || die "Tart VM exited during guest reboot"
        if bounded_exec 1 /dev/null /usr/sbin/sysctl -n kern.boottime >"$out/reboots/$label.after" 2>"$out/reboots/$label.after.stderr"; then
            if cmp -s "$out/reboots/$label.before" "$out/reboots/$label.after"; then
                :
            else
                reboot_cmp_status=$?
                case $reboot_cmp_status in
                    1) rebooted=1; break ;;
                    *) die "could not compare raw kern.boottime across reboot" ;;
                esac
            fi
        fi
        i=$((i + 1))
        sleep 2
    done
    [ "$rebooted" -eq 1 ] || die "raw kern.boottime did not change before reboot deadline"
    bounded_exec 15 /dev/null /usr/bin/sudo -n /usr/bin/true >>"$out/guest-agent.log" 2>&1 || die "passwordless guest sudo did not return after reboot"
    revalidate_guest "$label"
    kill -0 "$run_pid" 2>/dev/null || die "Tart VM exited before reboot proof was accepted"
    printf '\''%s\n'\'' PASS >"$out/reboots/$label.outcome"
    return 0
}'
reboot_status_boundary_is_valid() {
    actual_reboot_sequence=$(sed -n '/^reboot_guest() {$/,/^}$/p' "$1")
    [ "$actual_reboot_sequence" = "$expected_reboot_sequence" ]
}
installer_line_exact='        run_recorded install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile'
status_save_line_exact='        initial_install_status=$last_status'
snapshot_line_exact='        snapshot install-preassert'
determinate_probe_line_exact='        run_recorded install-preassert-determinate-nixd-status 60 /usr/local/bin/determinate-nixd status'
nix_probe_line_exact='        run_recorded install-preassert-nix-store-ping 120 /nix/var/nix/profiles/default/bin/nix store ping --store daemon'
status_gate_line_exact='        [ "$initial_install_status" -eq 0 ] || die "initial Determinate install failed"'
assert_line_exact='        assert_installed_state after-install'
expected_install_sequence='        run_recorded install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile
        initial_install_status=$last_status
        snapshot install-preassert
        run_recorded install-preassert-determinate-nixd-status 60 /usr/local/bin/determinate-nixd status
        run_recorded install-preassert-nix-store-ping 120 /nix/var/nix/profiles/default/bin/nix store ping --store daemon
        [ "$initial_install_status" -eq 0 ] || die "initial Determinate install failed"
        assert_installed_state after-install'
install_evidence_order_is_valid() {
    install_block=$(sed -n '/^phase_exit=0$/,$p' "$1" | sed -n '/^    lifecycle-install)$/,/^        ;;/p')
    actual_install_sequence=$(printf '%s\n' "$install_block" | sed -n '/^        run_recorded install 7200 "\$staged" --diagnostic-endpoint "\$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile$/,/^        assert_installed_state after-install$/p')
    [ "$actual_install_sequence" = "$expected_install_sequence" ] || return 1
    for install_line in "$installer_line_exact" "$status_save_line_exact" "$snapshot_line_exact" "$determinate_probe_line_exact" "$nix_probe_line_exact" "$status_gate_line_exact" "$assert_line_exact"; do
        install_count=$(printf '%s\n' "$install_block" | grep -F -x -c -- "$install_line" || :)
        [ "$install_count" -eq 1 ] || return 1
    done
}
inventory_find_line='    LC_ALL=C /usr/bin/find -P /etc/nix -xdev -exec /bin/sh "$0" --inventory-entries /etc/nix "$inventory_root_device" "$inventory_raw" {} + || die "could not inventory /etc/nix"'
inventory_find_unsafe_line='    LC_ALL=C /usr/bin/find -L /etc/nix -xdev -exec /bin/sh "$0" --inventory-entries /etc/nix "$inventory_root_device" "$inventory_raw" {} + || die "could not inventory /etc/nix"'
inventory_root_directory_gate='    [ "$stat_type" = Directory ] || die "/etc/nix inventory root is not a real directory"'
inventory_regular_link_gate='                [ "$inventory_nlink" -eq 1 ] || die "regular inventory file has multiple hard links"'
paired_residue_suffix_line='    for residue_suffix in etc-nix.inventory fstab.identity; do'
paired_residue_unsafe_line='    for residue_suffix in etc-nix.inventory fstab.identity determinate-nix-init-log.identity determinate-nix-daemon-log.identity; do'
single_init_log_line='    capture_fixed_identity /var/log/determinate-nix-init.log "$residue_prefix.determinate-nix-init-log.identity"'
single_daemon_log_line='    capture_fixed_identity /var/log/determinate-nix-daemon.log "$residue_prefix.determinate-nix-daemon-log.identity"'
active_uninstall_compare_line='    lifecycle-uninstall) compare_active_residue_contract "$evidence/lifecycle-daemon/after" "$phase_dir/before" "uninstall pre-state differs from daemon post-state" ;;'
unsafe_uninstall_compare_line='    lifecycle-uninstall) compare_residue_contract "$evidence/lifecycle-daemon/after" "$phase_dir/before" "uninstall pre-state differs from daemon post-state" ;;'
installed_init_log_line='    grep -E '\''^state=present path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d696e69742e6c6f67 type=f mode=[0-7]+ uid=[0-9]+ gid=[0-9]+ size=[0-9]+ nlink=1 sha256=[0-9a-f]{64}$'\'' "$installed_prefix.determinate-nix-init-log.identity" >/dev/null || die "installed snapshot lacks the Determinate init log"'
installed_daemon_log_line='    grep -E '\''^state=present path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d6461656d6f6e2e6c6f67 type=f mode=[0-7]+ uid=[0-9]+ gid=[0-9]+ size=[0-9]+ nlink=1 sha256=[0-9a-f]{64}$'\'' "$installed_prefix.determinate-nix-daemon-log.identity" >/dev/null || die "installed snapshot lacks the Determinate daemon log"'
final_residue_compare_line='    lifecycle-residue) compare_residue_contract "$phase_dir/before" "$phase_dir/after" "final post-reboot residue identity changed during observation" ;;'
residue_inventory_boundary_is_valid() {
    inventory_find_count=$(grep -F -x -c -- "$inventory_find_line" "$1" || :)
    inventory_find_unsafe_count=$(grep -F -x -c -- "$inventory_find_unsafe_line" "$1" || :)
    inventory_root_directory_count=$(grep -F -x -c -- "$inventory_root_directory_gate" "$1" || :)
    inventory_regular_link_count=$(grep -F -x -c -- "$inventory_regular_link_gate" "$1" || :)
    final_residue_compare_count=$(grep -F -x -c -- "$final_residue_compare_line" "$1" || :)
    [ "$inventory_find_count" -eq 1 ] && [ "$inventory_find_unsafe_count" -eq 0 ] && [ "$inventory_root_directory_count" -eq 1 ] \
        && [ "$inventory_regular_link_count" -eq 1 ] && [ "$final_residue_compare_count" -eq 1 ]
}
live_log_boundary_is_valid() {
    paired_residue_count=$(grep -F -x -c -- "$paired_residue_suffix_line" "$1" || :)
    paired_residue_unsafe_count=$(grep -F -x -c -- "$paired_residue_unsafe_line" "$1" || :)
    single_init_log_count=$(grep -F -x -c -- "$single_init_log_line" "$1" || :)
    single_daemon_log_count=$(grep -F -x -c -- "$single_daemon_log_line" "$1" || :)
    active_uninstall_compare_count=$(grep -F -x -c -- "$active_uninstall_compare_line" "$1" || :)
    installed_init_log_count=$(grep -F -x -c -- "$installed_init_log_line" "$1" || :)
    installed_daemon_log_count=$(grep -F -x -c -- "$installed_daemon_log_line" "$1" || :)
    [ "$paired_residue_count" -eq 2 ] && [ "$paired_residue_unsafe_count" -eq 0 ] \
        && [ "$single_init_log_count" -eq 1 ] && [ "$single_daemon_log_count" -eq 1 ] \
        && [ "$active_uninstall_compare_count" -eq 1 ] \
        && [ "$installed_init_log_count" -eq 1 ] && [ "$installed_daemon_log_count" -eq 1 ]
}
installer_process_coverage_is_valid() {
    coverage_file=$1
    version_probe='    run_recorded installer-version 60 "$staged" --version'
    help_probe='            run_recorded "installer-$absent_command" 60 "$staged" "$absent_command" --help'
    installer_processes=$(awk '
/^[[:space:]]*#/ { next }
/^[[:space:]]*run_recorded / && (index($0, " \"$staged\" ") || index($0, " $staged ") || index($0, " /nix/nix-installer ") || index($0, " \"/nix/nix-installer\" ")) { print; next }
/^[[:space:]]*"?\$staged"?[[:space:]]/ { print; next }
/^[[:space:]]*"?\/nix\/nix-installer"?[[:space:]]/ { print }
' "$coverage_file")
    version_probe_count=$(printf '%s\n' "$installer_processes" | grep -F -x -c -- "$version_probe" || :)
    help_probe_count=$(printf '%s\n' "$installer_processes" | grep -F -x -c -- "$help_probe" || :)
    [ "$version_probe_count" -eq 1 ] && [ "$help_probe_count" -eq 1 ] || return 1
    printf '%s\n' "$installer_processes" | awk -v version_probe="$version_probe" -v help_probe="$help_probe" '
index($0, "--diagnostic-endpoint \"$diagnostic_endpoint\"") { next }
$0 == version_probe || $0 == help_probe { next }
{ unsafe = 1 }
END { exit unsafe }
'
}
graph() {
    actual=$(sed -n "/^    $1)\$/,/^        ;;/p" "$host" | sed -n -e 's/^        run_phase /phase /p' -e 's/^        reboot_guest /reboot /p')
    [ "$actual" = "$2" ] || die "$1 execution graph changed"
}

for script in "$host" "$guest" "$0"; do for test_shell do "$test_shell" -n "$script"; done; done
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

# Reboot proof accepts only the two known shutdown outcomes and then observes a new boot time.
need "$host" '/usr/sbin/sysctl -n kern.boottime >"$out/reboots/$label.before"' "pre-reboot boot time missing"
need "$host" '/usr/sbin/sysctl -n kern.boottime >"$out/reboots/$label.after"' "post-reboot boot time missing"
need_exact "$host" "$reboot_before_line" "pre-reboot stderr separation changed"
need_exact "$host" "$reboot_outcome_fail_line" "initial reboot outcome changed"
need_exact "$host" "$shutdown_status_capture_line" "shutdown status capture changed"
need_exact "$host" "$shutdown_timeout_capture_line" "shutdown timeout capture changed"
need_exact "$host" "$shutdown_status_write_line" "shutdown status evidence changed"
need_exact "$host" "$shutdown_timeout_write_line" "shutdown timeout evidence changed"
need_exact "$host" "$shutdown_pair_allow_line" "shutdown status/timeout allowlist changed"
need_exact "$host" "$reboot_outcome_pass_line" "final reboot outcome changed"
reboot_status_boundary_is_valid "$host" || die "reboot proof boundary changed"
reject 'Guest Agent did not become unavailable for reboot|wait_guest_ready' "obsolete reboot down-window remains" "$host"
reject '(grep|sed|awk|cmp).*[.]shutdown(["[:space:]]|$)' "shutdown text is used as reboot proof" "$host"

# Foreign observation needs both the destructive approval and the exact second approval.
need "$host" '[ "$1" = --approve-destructive-vm ]' "destructive approval missing"
need "$host" '[ "$8" = --approve-observe-vendor-foreign-state ]' "host foreign approval missing"
need "$host" 'foreign_approval=approve-observe-vendor-foreign-state' "guest approval token changed"
need "$guest" '[ "$#" -eq 7 ] && [ "$approval" = approve-observe-vendor-foreign-state ]' "guest foreign approval missing"

# Phase archives validate a private part, hash it, then atomically finalize it before status classification.
need "$host" 'archive_part=$out/phases/$phase.tar.part' "partial archive missing"
need_exact "$host" "$archive_validation_line" "archive validation call changed"
archive_validation_boundary_is_valid "$host" || die "archive validation can overwrite capture variables"
need "$host" 'sha256 "$archive_part" >"$out/phases/$phase.tar.sha256"' "archive digest missing"
need "$host" '/bin/mv "$archive_part" "$archive"' "atomic archive finalization missing"
need "$host" 'phase archive contains a link or special entry' "archive type rejection missing"
need "$host" 'phase archive has duplicate paths' "archive duplicate rejection missing"
need "$host" 'phase archive has an unexpected prefix' "archive prefix rejection missing"
need "$host" 'checked_entry=${entry%/}' "archive path normalization changed"
need "$host" 'case "/$checked_entry/" in *'\''/../'\''*|*'\''/./'\''*|*'\''//'\''*) die "phase archive has an unsafe path: $entry" ;; esac' "archive path validation changed"
need "$host" 'case $checked_entry in */receipt.json) die "phase archive contains receipt bytes" ;; esac' "host receipt archive rejection missing"
need "$host" 'sed '\''s|/$||'\'' "$list" | LC_ALL=C sort >"$out/phases/$phase.sorted"' "normalized archive duplicate sort missing"
need "$host" 'uniq -d "$out/phases/$phase.sorted" >"$out/phases/$phase.duplicates"' "archive duplicate detection missing"
validate_line=$(exact_line "$host" "$archive_validation_line")
hash_line=$(line "$host" 'sha256 "$archive_part"')
rename_line=$(line "$host" '/bin/mv "$archive_part" "$archive"')
classify_line=$(line "$host" 'case $phase:$guest_status in')
[ "$validate_line" -lt "$hash_line" ] && [ "$hash_line" -lt "$rename_line" ] && [ "$rename_line" -lt "$classify_line" ] || die "archive finalization does not precede status classification"
need "$host" 'foreign-refuse:20)' "semantic status 20 missing"
need "$host" 'phase-status.fail.expected' "failed phase evidence is not classified"

# Private evidence keeps umask 077, but recorded child commands use their normal system mask.
need_exact "$guest" "$recorded_child_line" "recorded child umask or exec boundary changed"
recorded_child_boundary_is_valid "$guest" || die "recorded child inherits the private evidence umask"

boundary_fixture=$(mktemp -d "${TMPDIR:-/tmp}/pkg-dn03c-boundaries.XXXXXX") || die "could not create boundary fixture"
trap 'rm -R "$boundary_fixture"' EXIT HUP INT TERM
awk -v safe="$recorded_child_line" -v unsafe="$recorded_child_unsafe_line" '$0 == safe { print unsafe; next } { print }' "$guest" >"$boundary_fixture/inherited-umask.sh"
need_exact "$boundary_fixture/inherited-umask.sh" "$recorded_child_unsafe_line" "inherited-umask mutation vanished"
if recorded_child_boundary_is_valid "$boundary_fixture/inherited-umask.sh"; then die "inherited-umask mutation was accepted"; fi
awk -v safe="$archive_validation_variable_line" -v unsafe="$archive_validation_collision_line" '$0 == safe { print unsafe; next } { print }' "$host" >"$boundary_fixture/global-validation.sh"
need_exact "$boundary_fixture/global-validation.sh" "$archive_validation_collision_line" "global-validation mutation vanished"
if archive_validation_boundary_is_valid "$boundary_fixture/global-validation.sh"; then die "global-validation mutation was accepted"; fi
awk -v safe="$reboot_before_line" -v unsafe="$reboot_before_unsafe_line" '$0 == safe { print unsafe; next } { print }' "$host" >"$boundary_fixture/reboot-before-stderr.sh"
need_exact "$boundary_fixture/reboot-before-stderr.sh" "$reboot_before_unsafe_line" "pre-reboot stderr mutation vanished"
sh -n "$boundary_fixture/reboot-before-stderr.sh"
if reboot_status_boundary_is_valid "$boundary_fixture/reboot-before-stderr.sh"; then die "merged pre-reboot stderr mutation was accepted"; fi
awk -v safe="$reboot_compare_error_line" -v unsafe="$reboot_compare_error_unsafe_line" '$0 == safe { print unsafe; next } { print }' "$host" >"$boundary_fixture/reboot-status.sh"
need_exact "$boundary_fixture/reboot-status.sh" "$reboot_compare_error_unsafe_line" "reboot-status mutation vanished"
if grep -F -x -- "$reboot_compare_error_line" "$boundary_fixture/reboot-status.sh" >/dev/null; then die "safe compare-error gate survived mutation"; fi
sh -n "$boundary_fixture/reboot-status.sh"
if reboot_status_boundary_is_valid "$boundary_fixture/reboot-status.sh"; then die "reboot-status mutation was accepted"; fi
awk -v safe="$reboot_equal_line" -v unsafe="$reboot_equal_unsafe_line" '
/^reboot_guest\(\) \{$/ { in_reboot=1 }
in_reboot && $0 == safe { print unsafe; next }
{ print }
in_reboot && /^}$/ { in_reboot=0 }
' "$host" >"$boundary_fixture/reboot-equality.sh"
need_exact "$boundary_fixture/reboot-equality.sh" "$reboot_equal_unsafe_line" "reboot-equality mutation vanished"
sh -n "$boundary_fixture/reboot-equality.sh"
if reboot_status_boundary_is_valid "$boundary_fixture/reboot-equality.sh"; then die "equal boot time mutation was accepted"; fi
awk -v set_plus_e="$reboot_set_plus_e_line" -v shutdown="$reboot_shutdown_line" '
/^reboot_guest\(\) \{$/ { in_reboot=1 }
in_reboot && $0 == set_plus_e { next }
in_reboot && $0 == shutdown { print; print set_plus_e; next }
{ print }
in_reboot && /^}$/ { in_reboot=0 }
' "$host" >"$boundary_fixture/late-set-plus-e.sh"
sed -n '/^reboot_guest() {$/,/^}$/p' "$boundary_fixture/late-set-plus-e.sh" >"$boundary_fixture/late-set-plus-e.block"
need_exact "$boundary_fixture/late-set-plus-e.block" "$reboot_set_plus_e_line" "moved set +e mutation vanished"
moved_shutdown_line=$(exact_line "$boundary_fixture/late-set-plus-e.block" "$reboot_shutdown_line")
moved_set_plus_e_line=$(exact_line "$boundary_fixture/late-set-plus-e.block" "$reboot_set_plus_e_line")
[ "$moved_shutdown_line" -lt "$moved_set_plus_e_line" ] || die "set +e mutation did not move after shutdown"
sh -n "$boundary_fixture/late-set-plus-e.sh"
if reboot_status_boundary_is_valid "$boundary_fixture/late-set-plus-e.sh"; then die "late set +e mutation was accepted"; fi
awk -v capture="$shutdown_timeout_capture_line" '
/^reboot_guest\(\) \{$/ { in_reboot=1 }
in_reboot && $0 == capture { next }
in_reboot && $0 == "    set -e" { print; print capture; next }
{ print }
in_reboot && /^}$/ { in_reboot=0 }
' "$host" >"$boundary_fixture/late-timeout-capture.sh"
sed -n '/^reboot_guest() {$/,/^}$/p' "$boundary_fixture/late-timeout-capture.sh" >"$boundary_fixture/late-timeout-capture.block"
need_exact "$boundary_fixture/late-timeout-capture.block" "$shutdown_timeout_capture_line" "moved timeout capture mutation vanished"
set_e_line=$(exact_line "$boundary_fixture/late-timeout-capture.block" '    set -e')
moved_timeout_line=$(exact_line "$boundary_fixture/late-timeout-capture.block" "$shutdown_timeout_capture_line")
[ "$set_e_line" -lt "$moved_timeout_line" ] || die "timeout capture mutation did not move late"
sh -n "$boundary_fixture/late-timeout-capture.sh"
if reboot_status_boundary_is_valid "$boundary_fixture/late-timeout-capture.sh"; then die "late timeout capture mutation was accepted"; fi
awk -v return_line="$reboot_return_line" -v failure="$reboot_failure_line" '
/^reboot_guest\(\) \{$/ { in_reboot=1 }
in_reboot && $0 == return_line { next }
in_reboot && $0 == failure { print return_line; print; next }
{ print }
in_reboot && /^}$/ { in_reboot=0 }
' "$host" >"$boundary_fixture/early-reboot-return.sh"
sed -n '/^reboot_guest() {$/,/^}$/p' "$boundary_fixture/early-reboot-return.sh" >"$boundary_fixture/early-reboot-return.block"
need_exact "$boundary_fixture/early-reboot-return.block" "$reboot_return_line" "moved reboot return mutation vanished"
moved_return_line=$(exact_line "$boundary_fixture/early-reboot-return.block" "$reboot_return_line")
failure_line=$(exact_line "$boundary_fixture/early-reboot-return.block" "$reboot_failure_line")
[ "$moved_return_line" -lt "$failure_line" ] || die "return mutation did not move before reboot proof"
sh -n "$boundary_fixture/early-reboot-return.sh"
if reboot_status_boundary_is_valid "$boundary_fixture/early-reboot-return.sh"; then die "early reboot return mutation was accepted"; fi
awk -v safe="$shutdown_pair_allow_line" -v unsafe="$shutdown_pair_wildcard_line" '$0 == safe { print unsafe; next } { print }' "$host" >"$boundary_fixture/shutdown-status.sh"
need_exact "$boundary_fixture/shutdown-status.sh" "$shutdown_pair_wildcard_line" "shutdown-status mutation vanished"
sh -n "$boundary_fixture/shutdown-status.sh"
if reboot_status_boundary_is_valid "$boundary_fixture/shutdown-status.sh"; then die "wildcard shutdown status mutation was accepted"; fi
awk -v safe="$shutdown_pair_allow_line" -v unsafe="$shutdown_pair_accept_124_0_line" '$0 == safe { print unsafe; next } { print }' "$host" >"$boundary_fixture/shutdown-124-without-timeout.sh"
need_exact "$boundary_fixture/shutdown-124-without-timeout.sh" "$shutdown_pair_accept_124_0_line" "124:0 mutation vanished"
sh -n "$boundary_fixture/shutdown-124-without-timeout.sh"
if reboot_status_boundary_is_valid "$boundary_fixture/shutdown-124-without-timeout.sh"; then die "shutdown status 124 without timeout was accepted"; fi
awk -v failure="$reboot_failure_line" '
BEGIN {
    while ((getline candidate < ARGV[1]) > 0) {
        if (index(candidate, "PASS >\"$out/reboots/$label.outcome\"")) pass=candidate
    }
    close(ARGV[1])
}
/^reboot_guest\(\) \{$/ { in_reboot=1 }
in_reboot && index($0, "PASS >\"$out/reboots/$label.outcome\"") { pass=$0; next }
in_reboot && $0 == failure { print pass; print; next }
{ print }
in_reboot && /^}$/ { in_reboot=0 }
' "$host" >"$boundary_fixture/early-reboot-pass.sh"
sed -n '/^reboot_guest() {$/,/^}$/p' "$boundary_fixture/early-reboot-pass.sh" >"$boundary_fixture/early-reboot-pass.block"
need_exact "$boundary_fixture/early-reboot-pass.block" "$reboot_outcome_pass_line" "moved PASS mutation vanished"
moved_pass_line=$(exact_line "$boundary_fixture/early-reboot-pass.block" "$reboot_outcome_pass_line")
failure_line=$(exact_line "$boundary_fixture/early-reboot-pass.block" "$reboot_failure_line")
[ "$moved_pass_line" -lt "$failure_line" ] || die "PASS mutation did not move before reboot proof"
sh -n "$boundary_fixture/early-reboot-pass.sh"
if reboot_status_boundary_is_valid "$boundary_fixture/early-reboot-pass.sh"; then die "early reboot PASS mutation was accepted"; fi
awk -v safe="$inventory_find_line" -v unsafe="$inventory_find_unsafe_line" '$0 == safe { print unsafe; next } { print }' "$guest" >"$boundary_fixture/inventory-follow-links.sh"
need_exact "$boundary_fixture/inventory-follow-links.sh" "$inventory_find_unsafe_line" "find -L inventory mutation vanished"
if residue_inventory_boundary_is_valid "$boundary_fixture/inventory-follow-links.sh"; then die "find -L inventory mutation was accepted"; fi
awk -v gate="$inventory_root_directory_gate" '$0 != gate { print }' "$guest" >"$boundary_fixture/inventory-root-symlink.sh"
if grep -F -x -- "$inventory_root_directory_gate" "$boundary_fixture/inventory-root-symlink.sh" >/dev/null; then die "inventory root-directory mutation vanished"; fi
if residue_inventory_boundary_is_valid "$boundary_fixture/inventory-root-symlink.sh"; then die "missing inventory root-directory gate was accepted"; fi
awk -v gate="$inventory_regular_link_gate" '$0 != gate { print }' "$guest" >"$boundary_fixture/inventory-hardlinks.sh"
if grep -F -x -- "$inventory_regular_link_gate" "$boundary_fixture/inventory-hardlinks.sh" >/dev/null; then die "regular-file hardlink mutation vanished"; fi
if residue_inventory_boundary_is_valid "$boundary_fixture/inventory-hardlinks.sh"; then die "missing regular-file hardlink gate was accepted"; fi
awk -v comparison="$final_residue_compare_line" '$0 != comparison { print }' "$guest" >"$boundary_fixture/final-residue-comparison.sh"
if grep -F -x -- "$final_residue_compare_line" "$boundary_fixture/final-residue-comparison.sh" >/dev/null; then die "final residue comparison mutation vanished"; fi
if residue_inventory_boundary_is_valid "$boundary_fixture/final-residue-comparison.sh"; then die "missing final residue comparison was accepted"; fi
awk -v safe="$paired_residue_suffix_line" -v unsafe="$paired_residue_unsafe_line" '$0 == safe { print unsafe; next } { print }' "$guest" >"$boundary_fixture/paired-live-logs.sh"
[ "$(grep -F -x -c -- "$paired_residue_unsafe_line" "$boundary_fixture/paired-live-logs.sh" || :)" -eq 2 ] || die "paired live-log mutation vanished"
if live_log_boundary_is_valid "$boundary_fixture/paired-live-logs.sh"; then die "paired live-log mutation was accepted"; fi
awk -v safe="$active_uninstall_compare_line" -v unsafe="$unsafe_uninstall_compare_line" '$0 == safe { print unsafe; next } { print }' "$guest" >"$boundary_fixture/exact-active-logs.sh"
need_exact "$boundary_fixture/exact-active-logs.sh" "$unsafe_uninstall_compare_line" "exact active-log mutation vanished"
if live_log_boundary_is_valid "$boundary_fixture/exact-active-logs.sh"; then die "exact active-log mutation was accepted"; fi
awk -v required="$installed_daemon_log_line" '$0 != required { print }' "$guest" >"$boundary_fixture/missing-installed-log.sh"
if grep -F -x -- "$installed_daemon_log_line" "$boundary_fixture/missing-installed-log.sh" >/dev/null; then die "installed-log presence mutation vanished"; fi
if live_log_boundary_is_valid "$boundary_fixture/missing-installed-log.sh"; then die "missing installed-log presence gate was accepted"; fi
for test_shell do
    "$test_shell" -n "$boundary_fixture/paired-live-logs.sh"
    "$test_shell" -n "$boundary_fixture/exact-active-logs.sh"
    "$test_shell" -n "$boundary_fixture/missing-installed-log.sh"
done
{
    sed -n '/^capture_residue_contract() {$/,/^}$/p' "$guest"
    sed -n '/^stable_log_identity() {$/,/^}$/p' "$guest"
    sed -n '/^compare_active_residue_contract() {$/,/^}$/p' "$guest"
} >"$boundary_fixture/live-log-contract.block"
reject '(^|[;&|[:space:]])sleep([[:space:]]|$)|launchctl.*(stop|unload|bootout)|retry' "live-log capture must not retry, sleep, or pause a daemon" "$boundary_fixture/live-log-contract.block"
rm -R "$boundary_fixture"
trap - EXIT HUP INT TERM

# Byte-safe inventory uses find argv. It never parses path lines.
need_exact "$guest" "$inventory_find_line" "argv-safe /etc/nix inventory command changed"
residue_inventory_boundary_is_valid "$guest" || die "residue inventory safety boundary changed"
need_exact "$guest" '        printf '\''path_hex=%s type=%s mode=%s uid=%s gid=%s size=%s nlink=%s sha256=%s target_hex=%s\n'\'' \' "inventory record format changed"
need_exact "$guest" '        printf '\''state=absent path_hex=%s type=- mode=- uid=- gid=- size=- nlink=- sha256=-\n'\'' "$identity_path_hex" >"$identity_file"' "fixed-path absence format changed"
need_exact "$guest" '    capture_fixed_identity /etc/fstab "$residue_stem.fstab.identity"' "fstab identity capture missing"
need_exact "$guest" "$single_init_log_line" "single init-log identity capture missing"
need_exact "$guest" "$single_daemon_log_line" "single daemon-log identity capture missing"
need "$guest" 'stat_state_line=$(LC_ALL=C /usr/bin/stat -f '\''%d:%i:%p:%u:%g:%z:%l:%m:%c:%Lp:%HT'\'' "$stat_path")' "lstat stability identity missing"
need "$guest" '[ "$inventory_device" = "$inventory_root_device" ] || die "inventory path crossed a device boundary"' "inventory device gate missing"
need "$guest" '[ "$stat_state_line" = "$inventory_before" ] || die "inventory path changed while it was inspected"' "inventory stat stability gate missing"
need "$guest" '[ "$inventory_link_size" -eq $((inventory_size + 1)) ] || die "inventory readlink length differs from lstat size"' "symlink readlink length gate missing"
need "$guest" '[ "${#inventory_target_hex}" -eq $((inventory_size * 2)) ] || die "inventory symlink target hex differs from lstat size"' "symlink target hex gate missing"
need "$guest" '[ "$stat_type" = '\''Regular File'\'' ] || die "fixed identity path is not a non-symlink regular file"' "fixed-path type gate missing"
need "$guest" '[ "$identity_nlink" -eq 1 ] || die "fixed identity file has multiple hard links"' "fixed-path hardlink gate missing"
need "$guest" '[ "$stat_state_line" = "$identity_before" ] || die "fixed identity changed while it was inspected"' "fixed-path stability gate missing"
need "$guest" '/usr/bin/cmp -s "$residue_first.$residue_suffix" "$residue_second.$residue_suffix" || die "residue identity was not stable across two scans: $residue_suffix"' "double-scan comparison missing"
need "$guest" '/bin/mv "$residue_first.$residue_suffix" "$residue_prefix.$residue_suffix" || die "could not finalize residue identity: $residue_suffix"' "stable-first inventory finalization missing"
live_log_boundary_is_valid "$guest" || die "live-log residue boundary changed"
need_exact "$guest" "$installed_init_log_line" "installed init-log presence gate missing"
need_exact "$guest" "$installed_daemon_log_line" "installed daemon-log presence gate missing"
need_exact "$guest" '    lifecycle-install) compare_residue_contract "$evidence/baseline/after" "$phase_dir/before" "install pre-state differs from clean baseline" ;;' "install/baseline comparison missing"
need_exact "$guest" "$active_uninstall_compare_line" "active uninstall/daemon comparison missing"
need_exact "$guest" '    lifecycle-repeat-uninstall) compare_residue_contract "$evidence/lifecycle-uninstall/after" "$phase_dir/before" "repeat-uninstall pre-state differs from uninstall post-state" ;;' "repeat/uninstall comparison missing"
need_exact "$guest" '        compare_residue_contract "$evidence/lifecycle-repeat-uninstall/after" "$phase_dir/before" "post-reboot residue pre-state differs from repeat-uninstall post-state"' "post-reboot/repeat comparison missing"
need_exact "$guest" "$final_residue_compare_line" "final residue stability comparison missing"
snapshot_after_line=$(exact_line "$guest" 'snapshot after')
final_compare_line=$(exact_line "$guest" "$final_residue_compare_line")
strict_fail_line=$(exact_line "$guest" '[ "$strict_vendor_failed" -eq 0 ] || die "vendor residue remains"')
[ "$snapshot_after_line" -lt "$final_compare_line" ] && [ "$final_compare_line" -lt "$strict_fail_line" ] || die "final residue failure precedes snapshot-after or comparison"
reject '(^|[[:space:]/])(python|python3|perl|rustc|cargo|mtree)([[:space:]]|$)' "residue inventory added a forbidden runtime" "$guest"
reject '(^|[;&|])[[:space:]]*((/bin|/usr/bin)/)?rm[[:space:]]+-[^[:space:]]*[rR][^[:space:]]*[[:space:]]+(/etc/nix|/etc/fstab|/var/log/determinate)' "recursive fixed-path cleanup found" "$guest"
reject 'find[[:space:]]+/etc/nix[^\n]*-delete' "recursive /etc/nix deletion found" "$guest"

# The real internal scanner handles directories, files, links, and newlines under each available shell.
inventory_fixture=$(mktemp -d "/private/var/tmp/pkg-dn03c-inventory.XXXXXX") || die "could not create inventory fixture"
trap 'rm -R "$inventory_fixture"' EXIT HUP INT TERM
mkdir "$inventory_fixture/root" "$inventory_fixture/root/dir"
printf '%s\n' payload >"$inventory_fixture/root/file"
inventory_link_target='missing
target'
ln -s "$inventory_link_target" "$inventory_fixture/root/link"
inventory_newline_path="$inventory_fixture/root/name
with-newline"
printf '%s\n' newline >"$inventory_newline_path"
inventory_root_device=$(/usr/bin/stat -f %d "$inventory_fixture/root")
inventory_expected_target_hex=$(LC_ALL=C printf %s "$inventory_link_target" | /usr/bin/od -An -v -tx1 | /usr/bin/tr -d ' \n')
for inventory_shell do
    inventory_raw=$inventory_fixture/$(basename "$inventory_shell").raw
    : >"$inventory_raw"
    chmod 0600 "$inventory_raw"
    LC_ALL=C /usr/bin/find -P "$inventory_fixture/root" -xdev -exec "$inventory_shell" "$guest" --inventory-entries "$inventory_fixture/root" "$inventory_root_device" "$inventory_raw" {} + || die "inventory fixture failed under $inventory_shell"
    [ "$(wc -l <"$inventory_raw" | /usr/bin/tr -d ' ')" -eq 5 ] || die "inventory fixture path count changed under $inventory_shell"
    grep -Ev '^path_hex=[0-9a-f]+ type=[dfl] mode=[0-7]+ uid=[0-9]+ gid=[0-9]+ size=[0-9]+ nlink=[0-9]+ sha256=(-|[0-9a-f]{64}) target_hex=(-|[0-9a-f]+)$' "$inventory_raw" >"$inventory_raw.invalid" || :
    [ ! -s "$inventory_raw.invalid" ] || die "inventory fixture format changed under $inventory_shell"
    grep -F " type=l " "$inventory_raw" | grep -F " target_hex=$inventory_expected_target_hex" >/dev/null || die "inventory symlink target changed under $inventory_shell"
    find "$inventory_fixture" -name '*.link.*' -print -quit | grep . >/dev/null && die "inventory readlink temporary file remains"
done
printf '%s\n' hardlink >"$inventory_fixture/root/hard-a"
ln "$inventory_fixture/root/hard-a" "$inventory_fixture/root/hard-b"
for inventory_shell do
    inventory_raw=$inventory_fixture/hard-$(basename "$inventory_shell").raw
    : >"$inventory_raw"
    chmod 0600 "$inventory_raw"
    set +e
    "$inventory_shell" "$guest" --inventory-entries "$inventory_fixture/root" "$inventory_root_device" "$inventory_raw" "$inventory_fixture/root/hard-a" >"$inventory_fixture/hard.stdout" 2>"$inventory_fixture/hard.stderr"
    inventory_status=$?
    set -e
    [ "$inventory_status" -ne 0 ] || die "inventory hardlink passed under $inventory_shell"
    grep -F 'regular inventory file has multiple hard links' "$inventory_fixture/hard.stderr" >/dev/null || die "inventory hardlink failure changed under $inventory_shell"
done
mkfifo "$inventory_fixture/root/fifo"
inventory_raw=$inventory_fixture/fifo.raw
: >"$inventory_raw"
chmod 0600 "$inventory_raw"
set +e
/bin/sh "$guest" --inventory-entries "$inventory_fixture/root" "$inventory_root_device" "$inventory_raw" "$inventory_fixture/root/fifo" >"$inventory_fixture/fifo.stdout" 2>"$inventory_fixture/fifo.stderr"
inventory_status=$?
set -e
[ "$inventory_status" -ne 0 ] || die "inventory special file passed"
grep -F 'inventory contains an unsupported file type' "$inventory_fixture/fifo.stderr" >/dev/null || die "inventory special-file failure changed"
rm -R "$inventory_fixture"
trap - EXIT HUP INT TERM

# The active boundary allows only live-log size and hash drift.
live_log_fixture=$(mktemp -d "/private/var/tmp/pkg-dn03c-live-log.XXXXXX") || die "could not create live-log fixture"
trap 'rm -R "$live_log_fixture"' EXIT HUP INT TERM
{
    printf '%s\n' '#!/bin/sh' 'set -eu' 'die() { printf "not ok - %s\n" "$*" >&2; exit 1; }'
    sed -n '/^compare_residue_contract() {$/,/^}$/p' "$guest"
    sed -n '/^stable_log_identity() {$/,/^}$/p' "$guest"
    sed -n '/^compare_active_residue_contract() {$/,/^}$/p' "$guest"
    printf '%s\n' 'case $1 in' \
        '    active) shift; compare_active_residue_contract "$1" "$2" fixture ;;' \
        '    full) shift; compare_residue_contract "$1" "$2" fixture ;;' \
        '    *) exit 2 ;;' \
        'esac'
} >"$live_log_fixture/compare.sh"
chmod 0700 "$live_log_fixture/compare.sh"
live_left=$live_log_fixture/left
live_right=$live_log_fixture/right
printf '%s\n' 'state=absent path_hex=2f6574632f6e6978' >"$live_left.etc-nix.inventory"
printf '%s\n' 'state=absent path_hex=2f6574632f6673746162 type=- mode=- uid=- gid=- size=- nlink=- sha256=-' >"$live_left.fstab.identity"
live_sha_a=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
live_sha_b=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
printf 'state=present path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d696e69742e6c6f67 type=f mode=600 uid=0 gid=0 size=10 nlink=1 sha256=%s\n' "$live_sha_a" >"$live_left.determinate-nix-init-log.identity"
printf 'state=present path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d6461656d6f6e2e6c6f67 type=f mode=600 uid=0 gid=0 size=20 nlink=1 sha256=%s\n' "$live_sha_a" >"$live_left.determinate-nix-daemon-log.identity"
/bin/cp "$live_left.etc-nix.inventory" "$live_right.etc-nix.inventory"
/bin/cp "$live_left.fstab.identity" "$live_right.fstab.identity"
printf 'state=present path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d696e69742e6c6f67 type=f mode=600 uid=0 gid=0 size=11 nlink=1 sha256=%s\n' "$live_sha_b" >"$live_right.determinate-nix-init-log.identity"
printf 'state=present path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d6461656d6f6e2e6c6f67 type=f mode=600 uid=0 gid=0 size=21 nlink=1 sha256=%s\n' "$live_sha_b" >"$live_right.determinate-nix-daemon-log.identity"
for comparator_shell do
    "$comparator_shell" "$live_log_fixture/compare.sh" active "$live_left" "$live_right" || die "active live-log drift failed under $comparator_shell"
    set +e
    "$comparator_shell" "$live_log_fixture/compare.sh" full "$live_left" "$live_right" >/dev/null 2>&1
    comparator_status=$?
    set -e
    [ "$comparator_status" -ne 0 ] || die "full post-uninstall comparison allowed live-log drift under $comparator_shell"
done
for live_mutation in \
    'state=present state=absent' \
    'type=f type=l' \
    'mode=600 mode=640' \
    'uid=0 uid=1' \
    'gid=0 gid=1' \
    'nlink=1 nlink=2'
do
    live_safe=${live_mutation% *}
    live_unsafe=${live_mutation#* }
    sed "s/$live_safe/$live_unsafe/" "$live_left.determinate-nix-init-log.identity" >"$live_right.determinate-nix-init-log.identity"
    for comparator_shell do
        set +e
        "$comparator_shell" "$live_log_fixture/compare.sh" active "$live_left" "$live_right" >/dev/null 2>&1
        comparator_status=$?
        set -e
        [ "$comparator_status" -ne 0 ] || die "active live-log comparison allowed $live_safe drift under $comparator_shell"
    done
done
rm -R "$live_log_fixture"
trap - EXIT HUP INT TERM

# Receipt contents stay opaque. Metadata and SHA-256 identity are allowed.
need "$guest" 'receipt_identity()' "opaque receipt identity helper missing"
need "$guest" "stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' \"\$receipt\"" "receipt metadata proof missing"
need "$guest" 'sha256 "$receipt"' "receipt digest proof missing"
reject '(^|[;&|])[[:space:]]*((/bin|/usr/bin)/)?(cat|cp|dd|grep|head|tail|sed|awk|tar|tee|strings)[[:space:]].*(/nix/receipt\.json|"?\$receipt"?)' "receipt content read or copy found" "$guest"

# Private install evidence precedes the unchanged strict installed-state gate.
need_exact "$guest" 'DETSYS_IDS_TELEMETRY=disabled' "telemetry kill switch changed"
need_exact "$guest" 'diagnostic_endpoint=http://127.0.0.1:18080' "diagnostic loopback canary changed"
need_exact "$guest" 'export DETSYS_IDS_TELEMETRY' "telemetry kill switch export changed"
reject 'DETSYS_IDS_TRANSPORT' "ambient diagnostics transport is forbidden" "$guest"
reject 'capture_(pid|start|stop|name|port|count|grace)|diagnostic-request-count|disabled-diagnostic-request-count|diagnostic-scope|capture\.py' "controlled diagnostic capture code remains" "$guest"
reject "--diagnostic-endpoint[[:space:]]+''" "empty diagnostic endpoint remains" "$guest"
installer_process_coverage_is_valid "$guest" || die "an installer process lacks the loopback canary"
need "$guest" 'for snapshot_path in /nix /nix/receipt.json /nix/nix-installer /etc/nix /usr/local/bin/determinate-nixd /etc/fstab /etc/synthetic.conf /opt/pkg '\''/Library/Application Support/pkg'\''; do' "snapshot path anchors changed"
need "$guest" 'launchctl print system >"$snapshot_prefix.launchd-system" 2>&1 || die "could not record system launchd"' "snapshot launchd anchor changed"
reject 'link-target=%s' "raw snapshot symlink target remains" "$guest"
need_exact "$guest" '    for config_file in /etc/synthetic.conf; do' "snapshot config scope changed"
reject 'fstab[.]raw|cp[[:space:]]+/etc/fstab' "fstab contents are copied into evidence" "$guest"
reject '(^|[;&|])[[:space:]]*((/bin|/usr/bin)/)?(cat|cp|dd|head|tail|sed|tar|tee|strings)[[:space:]].*(/etc/fstab|/var/log/determinate-nix-(init|daemon)[.]log)' "fixed-path contents are copied or printed" "$guest"
need_exact "$guest" "$fstab_uuid_line" "fstab UUID comparison lowercasing changed"
fstab_uuid_case_is_valid "$guest" || die "fstab UUID comparison does not use the exact lowercase translation"
need "$guest" 'grep -Fxc "$installed_fstab" /etc/fstab >"$phase_dir/$installed_name.fstab-count" || die "exact Determinate fstab entry is absent"' "strict fstab assertion changed"
need_exact "$guest" '    awk '\''$2 == "/nix" {count++} END {print count + 0}'\'' /etc/fstab >"$phase_dir/$installed_name.fstab-nix-count" || die "could not count fstab /nix entries"' "fstab count-only evidence changed"
install_evidence_order_is_valid "$guest" || die "install pre-assert evidence phase or order changed"
order_fixture=$(mktemp -d "${TMPDIR:-/tmp}/pkg-dn03c-install-order.XXXXXX") || die "could not create install-order fixture"
trap 'rm -R "$order_fixture"' EXIT HUP INT TERM
awk -v installer="$installer_line_exact" -v snapshot="$snapshot_line_exact" -v determinate_probe="$determinate_probe_line_exact" -v nix_probe="$nix_probe_line_exact" '
$0 == installer {
    print snapshot
    print determinate_probe
    print nix_probe
}
$0 == snapshot || $0 == determinate_probe || $0 == nix_probe { next }
{ print }
' "$guest" >"$order_fixture/before-installer.sh"
need_exact "$order_fixture/before-installer.sh" "$snapshot_line_exact" "moved snapshot vanished from mutation"
need_exact "$order_fixture/before-installer.sh" "$determinate_probe_line_exact" "moved determinate-nixd probe vanished from mutation"
need_exact "$order_fixture/before-installer.sh" "$nix_probe_line_exact" "moved Nix probe vanished from mutation"
moved_snapshot_line=$(exact_line "$order_fixture/before-installer.sh" "$snapshot_line_exact")
moved_installer_line=$(exact_line "$order_fixture/before-installer.sh" "$installer_line_exact")
[ "$moved_snapshot_line" -lt "$moved_installer_line" ] || die "evidence mutation did not move evidence before the installer"
if install_evidence_order_is_valid "$order_fixture/before-installer.sh"; then die "install evidence before the installer was accepted"; fi
for probe_name in determinate nix; do
    case $probe_name in
        determinate) mutated_probe=$determinate_probe_line_exact ;;
        nix) mutated_probe=$nix_probe_line_exact ;;
    esac
    fatal_probe_line="        [ \"\$last_status\" -eq 0 ] || die \"$probe_name install pre-assert probe failed\""
    awk -v probe="$mutated_probe" -v fatal="$fatal_probe_line" '{ print; if ($0 == probe) print fatal }' "$guest" >"$order_fixture/fatal-$probe_name-probe.sh"
    need_exact "$order_fixture/fatal-$probe_name-probe.sh" "$fatal_probe_line" "fatal $probe_name probe mutation vanished"
    if install_evidence_order_is_valid "$order_fixture/fatal-$probe_name-probe.sh"; then die "fatal $probe_name install pre-assert probe gate was accepted"; fi
done
awk -v saved="$status_save_line_exact" -v gate="$status_gate_line_exact" '
$0 == gate { next }
{ print }
$0 == saved { print gate }
' "$guest" >"$order_fixture/early-install-gate.sh"
need_exact "$order_fixture/early-install-gate.sh" "$status_gate_line_exact" "moved installer status gate vanished from mutation"
moved_gate_line=$(exact_line "$order_fixture/early-install-gate.sh" "$status_gate_line_exact")
remaining_snapshot_line=$(exact_line "$order_fixture/early-install-gate.sh" "$snapshot_line_exact")
[ "$moved_gate_line" -lt "$remaining_snapshot_line" ] || die "installer status mutation did not move the gate before evidence"
if install_evidence_order_is_valid "$order_fixture/early-install-gate.sh"; then die "installer status gate before evidence was accepted"; fi
bare_state_change='        run_recorded missing-endpoint-install 7200 "$staged" install --determinate --no-confirm --no-modify-profile'
awk -v anchor='    lifecycle-repeat-install)' -v bare="$bare_state_change" '{ print; if ($0 == anchor) print bare }' "$guest" >"$order_fixture/missing-endpoint.sh"
need_exact "$order_fixture/missing-endpoint.sh" "$bare_state_change" "missing-endpoint mutation vanished"
if installer_process_coverage_is_valid "$order_fixture/missing-endpoint.sh"; then die "bare state-changing installer mutation was accepted"; fi
crash_child_boundary_is_valid "$guest" || die "crash installer child umask boundary changed"
awk -v safe="$crash_child_line" -v unsafe="$crash_child_unsafe_line" '$0 == safe { print unsafe; next } { print }' "$guest" >"$order_fixture/unsafe-crash-child.sh"
need_exact "$order_fixture/unsafe-crash-child.sh" "$crash_child_unsafe_line" "unsafe crash child mutation vanished"
if grep -F -x -- "$crash_child_line" "$order_fixture/unsafe-crash-child.sh" >/dev/null; then die "safe crash child survived unsafe mutation"; fi
if crash_child_boundary_is_valid "$order_fixture/unsafe-crash-child.sh"; then die "unsafe crash child mutation was accepted"; fi
fstab_uppercase_line="    installed_fstab=\"UUID=\$(printf '%s\\n' \"\$installed_uuid\" | tr 'ABCDEF' 'ABCDEF') /nix apfs rw,noatime,noauto,nobrowse,nosuid,owners # Added by the Determinate Nix Installer\""
awk -v lowercase="tr 'ABCDEF' 'abcdef'" -v uppercase="tr 'ABCDEF' 'ABCDEF'" '
index($0, lowercase) { start = index($0, lowercase); print substr($0, 1, start - 1) uppercase substr($0, start + length(lowercase)); next }
{ print }
' "$guest" >"$order_fixture/uppercase-fstab-translation.sh"
need_exact "$order_fixture/uppercase-fstab-translation.sh" "$fstab_uppercase_line" "uppercase fstab translation mutation vanished"
if grep -F -x -- "$fstab_uuid_line" "$order_fixture/uppercase-fstab-translation.sh" >/dev/null; then die "lowercase fstab translation survived uppercase mutation"; fi
if fstab_uuid_case_is_valid "$order_fixture/uppercase-fstab-translation.sh"; then die "uppercase fstab translation mutation was accepted"; fi
rm -R "$order_fixture"
trap - EXIT HUP INT TERM

# Exact vendor argv and observed statuses.
need_exact "$guest" '        run_recorded install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile' "install argv changed"
need_exact "$guest" '        run_recorded repeat-install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile' "repeat install argv changed"
need_exact "$guest" '        run_recorded repair 7200 /nix/nix-installer --diagnostic-endpoint "$diagnostic_endpoint" repair --no-confirm' "repair argv changed"
need_exact "$guest" '        run_recorded repair-sequoia 7200 /nix/nix-installer --diagnostic-endpoint "$diagnostic_endpoint" repair sequoia --no-confirm' "Sequoia repair argv changed"
need_exact "$guest" '        run_recorded uninstall 7200 /nix/nix-installer --diagnostic-endpoint "$diagnostic_endpoint" uninstall --no-confirm /nix/receipt.json' "uninstall argv changed"
need_exact "$guest" '        run_recorded repeat-uninstall 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" uninstall --no-confirm /nix/receipt.json' "repeat uninstall argv changed"
need_exact "$guest" '        write_argv "$phase_dir/install.argv" "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile' "crash argv record changed"
need_exact "$guest" "$crash_child_line" "crash installer argv or umask boundary changed"
need_exact "$guest" '        run_recorded recover-install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile' "recovery argv changed"
need_exact "$guest" '        run_recorded foreign-install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile' "foreign argv changed"
need_exact "$guest" '        run_recorded upstream-install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --prefer-upstream-nix --no-confirm --no-modify-profile' "upstream argv changed"
need_exact "$guest" '        run_recorded determinate-attempt 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile' "upstream refusal argv changed"
[ "$(grep -E -c '^[[:space:]]+[^#].*--diagnostic-endpoint "\$diagnostic_endpoint"' "$guest")" -eq 12 ] || die "vendor diagnostic endpoint coverage changed"
need "$guest" 'run_recorded daemon-version 60 "$daemon" version' "daemon version argv changed"
need "$guest" 'run_recorded daemon-status 60 "$daemon" status' "daemon status argv changed"
need "$guest" 'run_recorded daemon-upgrade-help 60 "$daemon" upgrade --help' "daemon upgrade probe changed"
need "$guest" 'run_recorded daemon-upgrade 7200 "$daemon" upgrade --version v3.22.1' "daemon upgrade argv changed"
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

# Normal tar directory entries pass, but redundant and traversing paths remain unsafe.
archive_fixture=$(mktemp -d "${TMPDIR:-/tmp}/pkg-dn03c-archive.XXXXXX") || die "could not create archive fixture"
trap 'rm -R "$archive_fixture"' EXIT HUP INT TERM
mkdir "$archive_fixture/tree" "$archive_fixture/tree/baseline" "$archive_fixture/tree/baseline/nested"
printf '%s\n' evidence >"$archive_fixture/tree/baseline/nested/file"
/usr/bin/tar -cf "$archive_fixture/normal.tar" -C "$archive_fixture/tree" baseline
/usr/bin/tar -tf "$archive_fixture/normal.tar" >"$archive_fixture/normal.list"
for safe_entry in baseline/ baseline/nested/ baseline/nested/file; do
    grep -Fx -- "$safe_entry" "$archive_fixture/normal.list" >/dev/null || die "normal archive entry missing: $safe_entry"
done
while IFS= read -r entry; do
    checked_entry=${entry%/}
    case "/$checked_entry/" in *'/../'*|*'/./'*|*'//'*) die "normal archive entry rejected: $entry" ;; esac
done <"$archive_fixture/normal.list"
for entry in baseline//file baseline/../file baseline/./file baseline// baseline///; do
    checked_entry=${entry%/}
    case "/$checked_entry/" in *'/../'*|*'/./'*|*'//'*) ;; *) die "unsafe archive entry accepted: $entry" ;; esac
done
for entry in baseline/receipt.json baseline/receipt.json/; do
    checked_entry=${entry%/}
    case $checked_entry in */receipt.json) ;; *) die "receipt archive entry accepted: $entry" ;; esac
done
mkdir "$archive_fixture/collision-tree" "$archive_fixture/collision-tree/baseline"
printf '%s\n' collision >"$archive_fixture/collision-tree/baseline/nested"
/usr/bin/tar -rf "$archive_fixture/normal.tar" -C "$archive_fixture/collision-tree" baseline/nested
/usr/bin/tar -tf "$archive_fixture/normal.tar" >"$archive_fixture/collision.list"
sed 's|/$||' "$archive_fixture/collision.list" | LC_ALL=C sort >"$archive_fixture/collision.sorted"
uniq -d "$archive_fixture/collision.sorted" >"$archive_fixture/collision.duplicates"
grep -Fx -- baseline/nested "$archive_fixture/collision.duplicates" >/dev/null || die "normalized file/directory collision was not found"
set +e
( [ ! -s "$archive_fixture/collision.duplicates" ] || die "archive has duplicate paths" ) >/dev/null 2>&1
collision_status=$?
set -e
[ "$collision_status" -ne 0 ] || die "normalized file/directory collision was accepted"
rm -R "$archive_fixture"
trap - EXIT HUP INT TERM

# A raw post-phase write failure must finalize failure evidence before cleanup.
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/pkg-dn03c-finalizer.XXXXXX") || die "could not create finalizer fixture"
trap 'rm -R "$fixture_root"' EXIT HUP INT TERM
{
    printf '%s\n' '#!/bin/sh' 'set -eu' 'fixture_root=$1' 'phase_dir=$fixture_root/phase' 'ledger=$fixture_root/phase-ledger' 'active_vendor_pid='
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
