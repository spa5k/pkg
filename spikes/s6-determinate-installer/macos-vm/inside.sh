#!/bin/sh
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
umask 077

phase_dir=
active_vendor_pid=
die() {
    if [ -n "$phase_dir" ] && [ -d "$phase_dir" ]; then
        printf '%s\n' "FAIL: $*" >>"$phase_dir/results"
        printf '%s\n' FAIL >"$phase_dir/phase-status"
    fi
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}
sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
record() { printf '%s: %s\n' "$1" "$2" >>"$phase_dir/results"; }
write_argv() { argv_file=$1; shift; : >"$argv_file"; for argv_item in "$@"; do printf '%s\n' "$argv_item" >>"$argv_file"; done; }
path_exists() { [ -e "$1" ] || [ -L "$1" ]; }

wait_bounded() {
    wait_limit=$1 wait_child=$2 wait_elapsed=0
    while kill -0 "$wait_child" 2>/dev/null; do
        if [ "$wait_elapsed" -ge "$wait_limit" ]; then
            kill -TERM "$wait_child" 2>/dev/null || :
            wait_grace=0
            while kill -0 "$wait_child" 2>/dev/null && [ "$wait_grace" -lt 5 ]; do sleep 1; wait_grace=$((wait_grace + 1)); done
            if kill -0 "$wait_child" 2>/dev/null; then kill -KILL "$wait_child" 2>/dev/null || :; fi
            wait "$wait_child" 2>/dev/null || :
            return 124
        fi
        sleep 1
        wait_elapsed=$((wait_elapsed + 1))
    done
    if wait "$wait_child"; then return 0; else return $?; fi
}
cleanup_children() {
    capture_stop
    [ -n "$active_vendor_pid" ] || return 0
    if kill -0 "$active_vendor_pid" 2>/dev/null; then
        kill -TERM "$active_vendor_pid" 2>/dev/null || :
        cleanup_grace=0
        while kill -0 "$active_vendor_pid" 2>/dev/null && [ "$cleanup_grace" -lt 5 ]; do sleep 1; cleanup_grace=$((cleanup_grace + 1)); done
        if kill -0 "$active_vendor_pid" 2>/dev/null; then kill -KILL "$active_vendor_pid" 2>/dev/null || :; fi
    fi
    wait "$active_vendor_pid" 2>/dev/null || :
    active_vendor_pid=
}
run_recorded() {
    run_name=$1 run_limit=$2
    shift 2
    write_argv "$phase_dir/$run_name.argv" "$@"
    "$@" </dev/null >"$phase_dir/$run_name.output" 2>&1 &
    run_pid=$!
    active_vendor_pid=$run_pid
    set +e
    wait_bounded "$run_limit" "$run_pid"
    last_status=$?
    set -e
    active_vendor_pid=
    printf '%s\n' "$last_status" >"$phase_dir/$run_name.status"
}

capture_pid=
capture_start() {
    capture_name=$1 capture_port=$2 capture_count=$phase_dir/$capture_name
    printf '0' >"$capture_count"
    cat >"$phase_dir/capture.py" <<'PY'
import http.server
import pathlib
import sys
counter = pathlib.Path(sys.argv[2])
class Handler(http.server.BaseHTTPRequestHandler):
    def handle_one_request(self):
        counter.write_text(str(int(counter.read_text() or "0") + 1))
        super().handle_one_request()
    def do_POST(self):
        self.send_response(204); self.end_headers()
    def do_PUT(self):
        self.send_response(204); self.end_headers()
    def log_message(self, *_): pass
http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
PY
    /usr/bin/python3 "$phase_dir/capture.py" "$capture_port" "$capture_count" >"$phase_dir/$capture_name.log" 2>&1 &
    capture_pid=$!
    sleep 1
    kill -0 "$capture_pid" 2>/dev/null || die "diagnostic capture service did not start"
}
capture_stop() {
    [ -n "$capture_pid" ] || return 0
    kill -TERM "$capture_pid" 2>/dev/null || :
    capture_grace=0
    while kill -0 "$capture_pid" 2>/dev/null && [ "$capture_grace" -lt 5 ]; do sleep 1; capture_grace=$((capture_grace + 1)); done
    if kill -0 "$capture_pid" 2>/dev/null; then kill -KILL "$capture_pid" 2>/dev/null || :; fi
    wait "$capture_pid" 2>/dev/null || :
    capture_pid=
}
trap cleanup_children EXIT
trap 'cleanup_children; exit 129' HUP
trap 'cleanup_children; exit 130' INT
trap 'cleanup_children; exit 143' TERM

receipt_identity() {
    receipt_name=$1 receipt=/nix/receipt.json
    [ -f "$receipt" ] && [ ! -L "$receipt" ] || die "receipt is not a regular non-symlink file"
    stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' "$receipt" >"$phase_dir/$receipt_name.stat"
    stat -f %z "$receipt" >"$phase_dir/$receipt_name.size"
    sha256 "$receipt" >"$phase_dir/$receipt_name.sha256"
}
snapshot() {
    snapshot_name=$1 snapshot_prefix=$phase_dir/$snapshot_name
    { sw_vers; uname -a; printf 'console-user=%s\n' "$console_user"; printf 'boot-session=%s\n' "$(sysctl -n kern.bootsessionuuid)"; } >"$snapshot_prefix.platform"
    : >"$snapshot_prefix.paths"
    for snapshot_path in /nix /nix/receipt.json /nix/nix-installer /etc/nix /usr/local/bin/determinate-nixd /etc/fstab /etc/synthetic.conf /opt/pkg '/Library/Application Support/pkg'; do
        if [ -L "$snapshot_path" ]; then
            stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' "$snapshot_path" >>"$snapshot_prefix.paths"
            printf 'link-target=%s path=%s\n' "$(readlink "$snapshot_path")" "$snapshot_path" >>"$snapshot_prefix.paths"
        elif [ -e "$snapshot_path" ]; then
            stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' "$snapshot_path" >>"$snapshot_prefix.paths"
        else printf 'absent path=%s\n' "$snapshot_path" >>"$snapshot_prefix.paths"; fi
    done
    diskutil apfs list >"$snapshot_prefix.apfs" 2>&1 || die "could not record APFS state"
    mount >"$snapshot_prefix.mounts" 2>&1 || die "could not record mounts"
    : >"$snapshot_prefix.config"
    for config_file in /etc/fstab /etc/synthetic.conf; do
        if [ -f "$config_file" ] && [ ! -L "$config_file" ]; then grep -Ein '(^|[[:space:]/])(nix|Nix Store)([[:space:]/]|$)' "$config_file" >>"$snapshot_prefix.config" || :; fi
    done
    { for launch_dir in /Library/LaunchDaemons /Library/LaunchAgents; do [ -d "$launch_dir" ] || continue; find "$launch_dir" -maxdepth 1 \( -type f -o -type l \) \( -iname '*nix*' -o -iname '*determinate*' -o -iname '*pkg*' \) -print; done; } >"$snapshot_prefix.launchd-files" 2>&1
    launchctl print system 2>/dev/null | grep -Ei '(^|[^[:alnum:]_])(nix|determinate|pkg)([^[:alnum:]_]|$)' >"$snapshot_prefix.launchd-jobs" || :
    set +e
    security find-generic-password -a 'Nix Store' -s 'Nix Store' /Library/Keychains/System.keychain >/dev/null 2>&1
    snapshot_keychain_status=$?
    set -e
    case $snapshot_keychain_status in 0) printf '%s\n' present >"$snapshot_prefix.keychain" ;; 44) printf '%s\n' absent >"$snapshot_prefix.keychain" ;; *) die "System Keychain metadata probe failed: $snapshot_keychain_status" ;; esac
    dscl . -list /Groups | grep -E '^(nixbld|_nixbld|_?pkg)$' >"$snapshot_prefix.groups" || :
    dscl . -list /Users | grep -E '^(_?nixbld[0-9]+|_?pkg)$' >"$snapshot_prefix.users" || :
    find /var/run /private/var/run -xdev -type s \( -iname '*nix*' -o -iname '*determinate*' -o -iname '*pkg*' \) -print 2>/dev/null >"$snapshot_prefix.sockets" || :
}
require_vendor_disk() {
    vendor_available_kb=$(df -Pk / | awk 'END {print $4}')
    case $vendor_available_kb in ''|*[!0-9]*) die "could not determine guest free disk" ;; esac
    printf '%s\n' "$vendor_available_kb" >"$phase_dir/vendor-free-kb"
    [ "$vendor_available_kb" -ge 31457280 ] || die "at least 30 GiB of guest free disk is required before vendor execution"
}
require_functional_nix() {
    nix_bin=/nix/var/nix/profiles/default/bin/nix
    [ -x "$nix_bin" ] || die "installed Nix executable is missing"
    run_recorded nix-version 60 "$nix_bin" --version
    [ "$last_status" -eq 0 ] || die "installed Nix version command failed"
    run_recorded nix-daemon-ping 120 "$nix_bin" store ping --store daemon
    [ "$last_status" -eq 0 ] || die "installed Nix daemon is not functional"
}
require_reboot_since() {
    reboot_phase=$1
    previous_boot=$(cat "$evidence/$reboot_phase/boot-session")
    [ "$current_boot" != "$previous_boot" ] || die "required reboot after $reboot_phase was not observed"
}

strict_residue() {
    residue_dirty=0
    : >"$phase_dir/vendor-residue"
    for residue_path in /nix /nix/receipt.json /nix/nix-installer /etc/nix /usr/local/bin/determinate-nixd; do
        if path_exists "$residue_path"; then printf 'present path=%s\n' "$residue_path" >>"$phase_dir/vendor-residue"; residue_dirty=1; fi
    done
    if diskutil apfs list | grep -E 'Name:[[:space:]]+Nix Store([[:space:]]|$)' >/dev/null; then
        printf '%s\n' 'present APFS=Nix Store' >>"$phase_dir/vendor-residue"
        residue_dirty=1
    fi
    for residue_file in /etc/fstab /etc/synthetic.conf; do
        if [ -f "$residue_file" ] && [ ! -L "$residue_file" ] && grep -Ei '(^|[[:space:]/])(nix|Nix Store)([[:space:]/]|$)' "$residue_file" >/dev/null; then
            printf 'entry file=%s\n' "$residue_file" >>"$phase_dir/vendor-residue"
            residue_dirty=1
        fi
    done
    for residue_dir in /Library/LaunchDaemons /Library/LaunchAgents; do
        [ -d "$residue_dir" ] || continue
        find "$residue_dir" -maxdepth 1 \( -type f -o -type l \) \( -iname '*nix*' -o -iname '*determinate*' \) -print >>"$phase_dir/vendor-residue"
    done
    [ ! -s "$phase_dir/vendor-residue" ] || residue_dirty=1
    launchctl print system 2>/dev/null | grep -Ei '(^|[^[:alnum:]_])(nix|determinate)([^[:alnum:]_]|$)' >"$phase_dir/vendor-launchd-residue" || :
    [ ! -s "$phase_dir/vendor-launchd-residue" ] || residue_dirty=1
    set +e
    security find-generic-password -a 'Nix Store' -s 'Nix Store' /Library/Keychains/System.keychain >/dev/null 2>&1
    residue_keychain_status=$?
    set -e
    case $residue_keychain_status in
        0) printf '%s\n' present >"$phase_dir/vendor-keychain-residue"; residue_dirty=1 ;;
        44) printf '%s\n' absent >"$phase_dir/vendor-keychain-residue" ;;
        *) die "System Keychain residue probe failed: $residue_keychain_status" ;;
    esac
    { dscl . -list /Groups | grep -E '^(nixbld|_nixbld)$' || :; dscl . -list /Users | grep -E '^_?nixbld[0-9]+$' || :; } >"$phase_dir/vendor-account-residue"
    [ ! -s "$phase_dir/vendor-account-residue" ] || residue_dirty=1
    find /var/run /private/var/run -xdev -type s \( -iname '*nix*' -o -iname '*determinate*' \) -print 2>/dev/null >"$phase_dir/vendor-socket-residue" || :
    [ ! -s "$phase_dir/vendor-socket-residue" ] || residue_dirty=1
    if [ "$residue_dirty" -eq 0 ]; then printf '%s\n' PASS >"$phase_dir/vendor-outcome"; else printf '%s\n' FAIL >"$phase_dir/vendor-outcome"; fi

    product_dirty=0
    : >"$phase_dir/product-residue"
    for product_path in /opt/pkg '/Library/Application Support/pkg'; do
        if path_exists "$product_path"; then printf 'present path=%s\n' "$product_path" >>"$phase_dir/product-residue"; product_dirty=1; fi
    done
    for product_dir in /Library/LaunchDaemons /Library/LaunchAgents; do
        [ -d "$product_dir" ] || continue
        find "$product_dir" -maxdepth 1 \( -type f -o -type l \) -iname '*pkg*' -print >>"$phase_dir/product-residue"
    done
    { dscl . -list /Groups | grep -E '^_?pkg$' || :; dscl . -list /Users | grep -E '^_?pkg$' || :; } >"$phase_dir/product-account-residue"
    [ ! -s "$phase_dir/product-residue" ] || product_dirty=1
    [ ! -s "$phase_dir/product-account-residue" ] || product_dirty=1
    if [ "$product_dirty" -eq 0 ]; then printf '%s\n' PASS >"$phase_dir/product-residue-outcome"; else printf '%s\n' FAIL >"$phase_dir/product-residue-outcome"; fi
    [ "$residue_dirty" -eq 0 ] || die "vendor residue remains"
    [ "$product_dirty" -eq 0 ] || die "product residue remains"
}

case $# in 6|7) ;; *) die "usage: inside.sh PHASE TOKEN MARKER STAGED_INSTALLER INSTALLER_SHA256 GUEST_EVIDENCE_DIR [APPROVAL]" ;; esac
phase=$1 token=$2 marker=$3 staged=$4 expected_sha=$5 evidence=$6 approval=${7-}
[ "$(id -u)" -eq 0 ] || die "guest lane requires root"
[ "$(uname -s)" = Darwin ] || die "guest must be Darwin"
[ "$(uname -m)" = arm64 ] || die "guest must be arm64"
[ "$(sysctl -n kern.hv_vmm_present)" = 1 ] || die "guest virtualization marker is absent"
case $(sysctl -n hw.model) in VirtualMac*) ;; *) die "guest model is not VirtualMac" ;; esac
case $token in ''|*[!0-9a-f]*) die "token must be lowercase hexadecimal" ;; esac
[ "${#token}" -eq 32 ] || die "token must have 32 hexadecimal characters"
case $expected_sha in *[!0-9a-f]*|'') die "installer digest must be lowercase hexadecimal" ;; esac
[ "${#expected_sha}" -eq 64 ] || die "installer digest must have 64 hexadecimal characters"
for canonical_path in "$marker" "$staged" "$evidence"; do case $canonical_path in /private/var/tmp/*) ;; *) die "guest paths must be under /private/var/tmp" ;; esac; done
[ -f "$marker" ] && [ ! -L "$marker" ] || die "guest ownership marker is missing"
[ "$(stat -f '%Su:%Sg:%Lp' "$marker")" = root:wheel:600 ] || die "guest ownership marker is not private"
[ "$(cat "$marker")" = "$token" ] || die "guest ownership marker does not match"
[ -f "$staged" ] && [ ! -L "$staged" ] || die "staged installer is not a regular non-symlink file"
[ "$(stat -f '%Su:%Sg:%Lp' "$staged")" = root:wheel:700 ] || die "staged installer must be root:wheel mode 0700"
staged_parent=$(CDPATH= cd -P "$(dirname "$staged")" && pwd) || die "staged installer parent is invalid"
[ "$staged" = "${staged_parent%/}/$(basename "$staged")" ] || die "staged installer path is not canonical"
[ "$(stat -f '%Su:%Sg:%Lp' "$staged_parent")" = root:wheel:700 ] || die "guest staging directory is not private"
[ "$(sha256 "$staged")" = "$expected_sha" ] || die "staged installer digest mismatch"
marker_parent=$(CDPATH= cd -P "$(dirname "$marker")" && pwd) || die "marker parent is invalid"
[ "$marker" = "${marker_parent%/}/$(basename "$marker")" ] || die "marker path is not canonical"
[ "$(stat -f '%Su:%Sg:%Lp' "$marker_parent")" = root:wheel:700 ] || die "marker parent is not private"

console_user=$(stat -f %Su /dev/console)
case $console_user in ''|root|loginwindow|_mbsetupuser) die "a real graphical console user is required" ;; esac
id "$console_user" >/dev/null 2>&1 || die "graphical console user does not exist"
console_uid=$(id -u "$console_user") console_gid=$(id -g "$console_user")
secure_token_state=$(sysadminctl -secureTokenStatus "$console_user" 2>&1) || die "could not read console secure-token state"
printf '%s\n' "$secure_token_state" | grep -F 'Secure token is ENABLED' >/dev/null || die "graphical console user lacks a secure token"
SUDO_USER=$console_user SUDO_UID=$console_uid SUDO_GID=$console_gid
export SUDO_USER SUDO_UID SUDO_GID

ledger=$evidence/phase-ledger
if [ "$phase" = baseline ]; then
    [ "$#" -eq 6 ] || die "baseline does not accept approval"
    [ ! -e "$evidence" ] && [ ! -L "$evidence" ] || die "baseline evidence path must be absent"
    evidence_parent=$(CDPATH= cd -P "$(dirname "$evidence")" && pwd) || die "evidence parent is invalid"
    [ "$evidence" = "${evidence_parent%/}/$(basename "$evidence")" ] || die "evidence path is not canonical"
    mkdir -m 0700 "$evidence"
    chown root:wheel "$evidence"
    : >"$ledger"
else
    [ -d "$evidence" ] && [ ! -L "$evidence" ] || die "lane evidence directory is missing or unsafe"
    [ "$(stat -f '%Su:%Sg:%Lp' "$evidence")" = root:wheel:700 ] || die "lane evidence directory is not private"
    [ -f "$ledger" ] && [ ! -L "$ledger" ] || die "phase ledger is missing or unsafe"
    evidence_parent=$(CDPATH= cd -P "$(dirname "$evidence")" && pwd) || die "evidence parent is invalid"
    [ "$evidence" = "${evidence_parent%/}/$(basename "$evidence")" ] || die "evidence path is not canonical"
fi
case $phase in
    baseline) expected_ledger='' ;;
    lifecycle-install) expected_ledger='baseline' ;;
    lifecycle-post-reboot) expected_ledger='baseline
lifecycle-install' ;;
    lifecycle-repeat-install) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot' ;;
    lifecycle-repair) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot
lifecycle-repeat-install' ;;
    lifecycle-daemon) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot
lifecycle-repeat-install
lifecycle-repair' ;;
    lifecycle-uninstall) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot
lifecycle-repeat-install
lifecycle-repair
lifecycle-daemon' ;;
    lifecycle-repeat-uninstall) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot
lifecycle-repeat-install
lifecycle-repair
lifecycle-daemon
lifecycle-uninstall' ;;
    lifecycle-residue) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot
lifecycle-repeat-install
lifecycle-repair
lifecycle-daemon
lifecycle-uninstall
lifecycle-repeat-uninstall' ;;
    crash-kill) expected_ledger='baseline' ;;
    crash-recover) expected_ledger='baseline
crash-kill' ;;
    foreign-synthetic-prepare) expected_ledger='baseline' ;;
    foreign-post-reboot) expected_ledger='baseline
foreign-synthetic-prepare' ;;
    foreign-refuse) expected_ledger='baseline
foreign-synthetic-prepare
foreign-post-reboot' ;;
    foreign-observe) expected_ledger='baseline
foreign-synthetic-prepare
foreign-post-reboot
foreign-refuse' ;;
    upstream-install) expected_ledger='baseline' ;;
    upstream-determinate-attempt) expected_ledger='baseline
upstream-install' ;;
    *) die "unsupported phase: $phase" ;;
esac
[ "$(cat "$ledger")" = "$expected_ledger" ] || die "phase order is invalid for $phase"
if [ "$phase" = foreign-observe ]; then
    [ "$#" -eq 7 ] && [ "$approval" = --approve-foreign-nix-observation ] || die "foreign observation requires exact second approval"
else
    [ "$#" -eq 6 ] || die "$phase does not accept approval"
fi
phase_dir=$evidence/$phase
[ ! -e "$phase_dir" ] && [ ! -L "$phase_dir" ] || die "phase evidence already exists"
mkdir -m 0700 "$phase_dir"
chown root:wheel "$phase_dir"
printf '%s\n' "$phase" >>"$ledger"
printf '%s\n' "$expected_sha" >"$phase_dir/installer.expected.sha256"
sha256 "$staged" >"$phase_dir/installer.actual.sha256"
printf '%s\n' "$console_user" >"$phase_dir/console-user"
current_boot=$(sysctl -n kern.bootsessionuuid)
printf '%s\n' "$current_boot" >"$phase_dir/boot-session"
snapshot before

phase_exit=0
case $phase in
    baseline)
        require_vendor_disk
        run_recorded installer-version 60 "$staged" --version
        [ "$last_status" -eq 0 ] || die "installer version command failed"
        grep -F '3.22.1' "$phase_dir/installer-version.output" >/dev/null || die "installer version is not 3.22.1"
        strict_residue
        ;;
    lifecycle-install)
        require_vendor_disk
        capture_start diagnostic-request-count 18080
        diagnostic_endpoint=http://127.0.0.1:18080
        run_recorded install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile
        sleep 2
        capture_stop
        [ "$last_status" -eq 0 ] || die "initial Determinate install failed"
        [ "$(cat "$phase_dir/diagnostic-request-count")" -gt 0 ] || die "controlled diagnostic endpoint received no request"
        receipt_identity receipt
        [ -f /nix/nix-installer ] && [ ! -L /nix/nix-installer ] || die "installed installer copy is unsafe or absent"
        sha256 /nix/nix-installer >"$phase_dir/installed-installer.sha256"
        [ "$(cat "$phase_dir/installed-installer.sha256")" = "$expected_sha" ] || die "installed installer digest differs from the pin"
        phase_exit=194
        ;;
    lifecycle-post-reboot)
        require_reboot_since lifecycle-install
        receipt_identity receipt
        require_functional_nix
        ;;
    lifecycle-repeat-install)
        require_vendor_disk
        capture_start disabled-diagnostic-request-count 18081
        DETSYS_IDS_TRANSPORT=http://127.0.0.1:18081
        export DETSYS_IDS_TRANSPORT
        run_recorded repeat-install 7200 "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile
        sleep 2
        capture_stop
        unset DETSYS_IDS_TRANSPORT
        [ "$last_status" -eq 0 ] || die "repeat Determinate install failed"
        [ "$(cat "$phase_dir/disabled-diagnostic-request-count")" -eq 0 ] || die "disabled diagnostic endpoint received a request"
        printf '%s\n' 'This proves only that the controlled endpoint received zero requests.' >"$phase_dir/diagnostic-scope"
        receipt_identity receipt
        require_functional_nix
        ;;
    lifecycle-repair)
        require_vendor_disk
        run_recorded repair 7200 /nix/nix-installer --diagnostic-endpoint '' repair --no-confirm
        [ "$last_status" -eq 0 ] || die "default repair failed"
        run_recorded repair-sequoia 7200 /nix/nix-installer --diagnostic-endpoint '' repair sequoia --no-confirm
        [ "$last_status" -eq 0 ] || die "Sequoia repair failed"
        receipt_identity receipt
        require_functional_nix
        ;;
    lifecycle-daemon)
        require_vendor_disk
        daemon=/usr/local/bin/determinate-nixd
        [ -x "$daemon" ] && [ ! -L "$daemon" ] || die "determinate-nixd is unsafe or absent"
        stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' "$daemon" >"$phase_dir/determinate-nixd.stat"
        [ "$(stat -f '%Lp:%Su:%Sg' "$daemon")" = 555:root:wheel ] || die "determinate-nixd mode or ownership is unexpected"
        run_recorded daemon-version 60 "$daemon" version
        [ "$last_status" -eq 0 ] || die "determinate-nixd version failed"
        run_recorded daemon-upgrade-help 60 "$daemon" upgrade --help
        [ "$last_status" -eq 0 ] || die "determinate-nixd upgrade help failed"
        run_recorded daemon-upgrade 7200 "$daemon" upgrade --version v3.22.1
        [ "$last_status" -eq 0 ] || die "pinned determinate-nixd upgrade failed"
        for absent_command in update upgrade self-update; do
            run_recorded "installer-$absent_command" 60 "$staged" "$absent_command" --help
            [ "$last_status" -ne 0 ] || die "installer unexpectedly accepts $absent_command"
            grep -Ei '(unrecognized|unknown|invalid).*(subcommand|command)|unexpected argument' "$phase_dir/installer-$absent_command.output" >/dev/null || die "installer $absent_command rejection was not identified as an unknown subcommand"
        done
        receipt_identity receipt
        require_functional_nix
        ;;
    lifecycle-uninstall)
        require_vendor_disk
        receipt_identity receipt-before-uninstall
        run_recorded uninstall 7200 /nix/nix-installer --diagnostic-endpoint '' uninstall --no-confirm /nix/receipt.json
        [ "$last_status" -eq 0 ] || { printf '%s\n' FAIL >"$phase_dir/vendor-outcome"; die "uninstall failed"; }
        printf '%s\n' PASS >"$phase_dir/vendor-outcome"
        ;;
    lifecycle-repeat-uninstall)
        require_vendor_disk
        run_recorded repeat-uninstall 7200 "$staged" --diagnostic-endpoint '' uninstall --no-confirm /nix/receipt.json
        [ "$last_status" -eq 1 ] || die "repeat uninstall did not return the pinned observed status 1"
        grep -F 'Reading receipt' "$phase_dir/repeat-uninstall.output" >/dev/null || die "repeat uninstall did not identify receipt reading"
        grep -F 'No such file or directory' "$phase_dir/repeat-uninstall.output" >/dev/null || die "repeat uninstall did not identify the absent receipt"
        printf '%s\n' PASS >"$phase_dir/vendor-outcome"
        phase_exit=194
        ;;
    lifecycle-residue)
        require_reboot_since lifecycle-repeat-uninstall
        strict_residue
        ;;
    crash-kill)
        require_vendor_disk
        write_argv "$phase_dir/install.argv" "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile
        "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile </dev/null >"$phase_dir/install.output" 2>&1 &
        crash_pid=$!
        active_vendor_pid=$crash_pid
        case $crash_pid in ''|*[!0-9]*) die "installer PID is invalid" ;; esac
        [ "$crash_pid" -gt 1 ] && [ "$crash_pid" -ne "$$" ] || die "installer PID is unsafe"
        crash_command=$(ps -p "$crash_pid" -o command=) || die "installer process exited before PID validation"
        case $crash_command in *"$staged"*) ;; *) die "PID does not identify the staged installer" ;; esac
        printf '%s\n' "$crash_pid" >"$phase_dir/installer.pid"
        crash_elapsed=0 crash_ready=0
        while kill -0 "$crash_pid" 2>/dev/null && [ "$crash_elapsed" -lt 1800 ]; do
            if [ -x /usr/local/bin/determinate-nixd ] && find /nix/store -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null | grep . >/dev/null; then crash_ready=1; break; fi
            sleep 1
            crash_elapsed=$((crash_elapsed + 1))
        done
        if [ "$crash_ready" -ne 1 ]; then
            kill -TERM "$crash_pid" 2>/dev/null || :
            set +e; wait_bounded 5 "$crash_pid"; set -e
            active_vendor_pid=
            die "late crash marker was not reached while the installer remained alive"
        fi
        printf '%s\n' 'determinate-nixd executable and non-empty Nix store' >"$phase_dir/crash-marker"
        kill -KILL "$crash_pid" || die "could not SIGKILL the validated installer PID"
        set +e
        wait "$crash_pid"
        crash_status=$?
        set -e
        active_vendor_pid=
        printf '%s\n' "$crash_status" >"$phase_dir/install.status"
        [ "$crash_status" -eq 137 ] || die "SIGKILL did not produce status 137"
        snapshot immediate-after-sigkill
        phase_exit=194
        ;;
    crash-recover)
        require_reboot_since crash-kill
        require_vendor_disk
        run_recorded recover-install 7200 "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile
        [ "$last_status" -eq 0 ] || die "install did not recover after the forced crash"
        receipt_identity receipt
        [ "$(sha256 /nix/nix-installer)" = "$expected_sha" ] || die "recovered installed copy digest differs from the pin"
        require_functional_nix
        ;;
    foreign-synthetic-prepare)
        path_exists /nix && die "foreign lane requires /nix to be absent before synthetic preparation"
        [ ! -L /etc/synthetic.conf ] || die "/etc/synthetic.conf must not be a symlink"
        if [ -f /etc/synthetic.conf ]; then grep -Eq '^nix([[:space:]]|$)' /etc/synthetic.conf && die "a synthetic nix entry already exists"; fi
        printf 'nix\n' >>/etc/synthetic.conf
        chown root:wheel /etc/synthetic.conf
        chmod 0644 /etc/synthetic.conf
        sync
        phase_exit=194
        ;;
    foreign-post-reboot)
        require_reboot_since foreign-synthetic-prepare
        [ -d /nix ] && [ ! -L /nix ] || die "synthetic /nix did not appear after reboot"
        sentinel=/nix/pkg-s6-foreign-sentinel
        [ ! -e "$sentinel" ] && [ ! -L "$sentinel" ] || die "foreign sentinel already exists"
        printf '%s' 'pkg-s6 foreign Nix ownership proof' >"$sentinel"
        chown root:wheel "$sentinel"
        chmod 0600 "$sentinel"
        sha256 "$sentinel" >"$phase_dir/sentinel.sha256"
        ;;
    foreign-refuse)
        sentinel=/nix/pkg-s6-foreign-sentinel
        [ -f "$sentinel" ] && [ ! -L "$sentinel" ] || die "foreign sentinel is absent or unsafe"
        sha256 "$sentinel" >"$phase_dir/sentinel.sha256"
        [ "$(cat "$phase_dir/sentinel.sha256")" = "$(cat "$evidence/foreign-post-reboot/sentinel.sha256")" ] || die "foreign sentinel changed before refusal"
        printf '%s\n' 'SECOND_APPROVAL_REQUIRED' >"$phase_dir/vendor-outcome"
        phase_exit=20
        ;;
    foreign-observe)
        require_vendor_disk
        sentinel=/nix/pkg-s6-foreign-sentinel
        [ -f "$sentinel" ] && [ ! -L "$sentinel" ] || die "foreign sentinel is absent or unsafe"
        sentinel_before=$(sha256 "$sentinel")
        [ "$sentinel_before" = "$(cat "$evidence/foreign-post-reboot/sentinel.sha256")" ] || die "foreign sentinel changed before observation"
        run_recorded foreign-install 7200 "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile
        printf 'status=%s\n' "$last_status" >"$phase_dir/vendor-outcome"
        [ -f "$sentinel" ] && [ ! -L "$sentinel" ] || die "vendor removed the foreign sentinel"
        [ "$(sha256 "$sentinel")" = "$sentinel_before" ] || die "vendor changed the foreign sentinel"
        printf '%s\n' 'No uninstall or cleanup was run after the foreign observation.' >"$phase_dir/cleanup-scope"
        ;;
    upstream-install)
        require_vendor_disk
        run_recorded upstream-install 7200 "$staged" --diagnostic-endpoint '' install --prefer-upstream-nix --no-confirm --no-modify-profile
        [ "$last_status" -eq 0 ] || die "upstream Nix install failed"
        receipt_identity receipt
        upstream_nix=/nix/var/nix/profiles/default/bin/nix
        [ -x "$upstream_nix" ] || die "upstream Nix executable is missing"
        run_recorded upstream-version 60 "$upstream_nix" --version
        [ "$last_status" -eq 0 ] || die "upstream Nix version failed"
        [ "$(sed -n '1p' "$phase_dir/upstream-version.output")" = 'nix (Nix) 2.35.2' ] || die "upstream Nix is not exactly 2.35.2"
        ;;
    upstream-determinate-attempt)
        require_vendor_disk
        receipt_identity receipt-before
        run_recorded determinate-attempt 7200 "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile
        [ "$last_status" -eq 1 ] || die "Determinate-on-upstream attempt did not return the pinned status 1"
        grep -F 'used different planner settings' "$phase_dir/determinate-attempt.output" >/dev/null || die "planner mismatch refusal was not observed"
        receipt_identity receipt-after
        [ "$(cat "$phase_dir/receipt-before.sha256")" = "$(cat "$phase_dir/receipt-after.sha256")" ] || die "refused attempt changed the opaque receipt"
        upstream_nix=/nix/var/nix/profiles/default/bin/nix
        run_recorded upstream-version-after 60 "$upstream_nix" --version
        [ "$last_status" -eq 0 ] || die "upstream Nix stopped working after refusal"
        [ "$(sed -n '1p' "$phase_dir/upstream-version-after.output")" = 'nix (Nix) 2.35.2' ] || die "refused attempt changed upstream Nix 2.35.2"
        ;;
esac

snapshot after
printf '%s\n' PASS >"$phase_dir/phase-status"
record PASS "$phase completed with expected observations"
find "$evidence" -type d -exec chmod 0700 {} \;
find "$evidence" -type f -exec chmod 0600 {} \;
capture_stop
trap - EXIT HUP INT TERM
exit "$phase_exit"
