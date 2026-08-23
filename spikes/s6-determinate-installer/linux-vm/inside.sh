#!/bin/sh
set -u
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH

die() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
sha256() { sha256sum "$1" | awk '{print $1}'; }
record() { printf '%s: %s\n' "$1" "$2" >>"$evidence/results"; }
snapshot() {
    name=$1
    {
        date -u '+%Y-%m-%dT%H:%M:%SZ'
        uname -a
        find /nix -maxdepth 3 -xdev -printf '%M %u:%g %p\n' 2>&1 | sort
        systemctl list-unit-files '*nix*' '*determinate*' --no-pager 2>&1
        systemctl --no-pager --full status nix-daemon.service determinate-nixd.service 2>&1
    } >"$evidence/$name" 2>&1
}
write_argv() { file=$1; shift; : >"$file"; for arg in "$@"; do printf '%s\n' "$arg" >>"$file"; done; }

[ "$#" -eq 4 ] || die "usage: inside.sh LANE TOKEN /absolute/installer /absolute/pins"
lane=$1
token=$2
installer=$3
pins=$4
phase=${S6_PHASE:-initial}
case $lane in
    lifecycle|diagnostics-disabled|crash-recovery|foreign-nix|upstream-input) ;;
    *) die "unsupported lane: $lane" ;;
esac
[ "$(id -u)" -eq 0 ] || die "guest runner requires EUID 0"
[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] || die "guest must be x86_64 Linux"
[ -d /run/systemd/system ] || die "systemd is not active"
systemd_state=$(timeout 120 systemctl is-system-running --wait 2>/dev/null || true)
case $systemd_state in running|degraded) ;; *) die "systemd is not active: $systemd_state" ;; esac
virt=$(systemd-detect-virt 2>/dev/null || true)
[ "$virt" != none ] && [ -n "$virt" ] || die "guest is not virtualized"
dmi=$(cat /sys/class/dmi/id/sys_vendor /sys/class/dmi/id/product_name 2>/dev/null || true)
printf '%s' "$dmi" | grep -E 'QEMU|Standard PC' >/dev/null || die "guest DMI does not identify QEMU"
[ -f /etc/pkg-s6-disposable-vm ] && [ ! -L /etc/pkg-s6-disposable-vm ] || die "disposable VM marker is missing"
[ "$(cat /etc/pkg-s6-disposable-vm)" = "$token" ] || die "disposable VM marker does not match"
for input in "$installer" "$pins"; do
    case $input in /*) ;; *) die "guest inputs must be absolute" ;; esac
    [ -f "$input" ] && [ ! -L "$input" ] || die "guest input must be a regular non-symlink file"
done

expected=$(awk '$2 == "x86_64-linux" {print $1}' "$pins")
[ "$(sha256 "$installer")" = "$expected" ] || die "guest installer digest mismatch"
umask 077
evidence=/var/lib/pkg-s6-evidence
if [ "$phase" = initial ]; then
    [ ! -e "$evidence" ] || die "evidence directory already exists"
    mkdir -m 0700 "$evidence"
elif [ "$phase" != resume ] || [ ! -d "$evidence" ]; then
    die "invalid continuation phase"
fi
run_dir=$(mktemp -d /var/tmp/pkg-s6-run.XXXXXX)
chmod 0700 "$run_dir"
staged=$run_dir/nix-installer
cp "$installer" "$staged"
chown root:root "$staged"
chmod 0700 "$staged"
[ "$(sha256 "$staged")" = "$expected" ] || die "staged installer digest mismatch"
printf '%s\n' "$expected" >"$evidence/installer.sha256"
printf '%s\n' "$virt" >"$evidence/virtualization"
printf '%s\n' "$dmi" >"$evidence/dmi"
date -u '+%Y-%m-%dT%H:%M:%SZ' >"$evidence/run-date"
printf '%s\n' 'Ubuntu 24.04 amd64 release 20260814' >"$evidence/image"
snapshot "before-$phase.txt"

capture_start() {
    capture_port=$1
    capture_name=$2
    capture_count=$evidence/$capture_name
    cat >"$run_dir/capture.py" <<'PY'
import http.server, pathlib, sys
path = pathlib.Path(sys.argv[2])
class Handler(http.server.BaseHTTPRequestHandler):
    def handle_one_request(self):
        path.write_text(str(int(path.read_text() or "0") + 1))
        super().handle_one_request()
    def do_POST(self):
        self.send_response(204); self.end_headers()
    def do_PUT(self):
        self.send_response(204); self.end_headers()
    def log_message(self, *_): pass
http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
PY
    printf '0' >"$capture_count"
    python3 "$run_dir/capture.py" "$capture_port" "$capture_count" >"$evidence/$capture_name.capture.log" 2>&1 &
    capture_pid=$!
    sleep 1
    kill -0 "$capture_pid" 2>/dev/null || die "diagnostic capture service did not start"
}
capture_stop() {
    [ -n "${capture_pid:-}" ] || return 0
    kill "$capture_pid" 2>/dev/null || :
    wait "$capture_pid" 2>/dev/null || :
    capture_pid=
}
capture_sentry_identity() {
    sentry_stage=$1
    sentry=/etc/nix/sentry-endpoint
    prefix=$evidence/sentry-$sentry_stage
    sentry_stat_format='type=%F numeric-owner=%u:%g named-owner=%U:%G mode=0%a size=%s path=%n'
    if [ -L "$sentry" ]; then
        printf '%s\n' symlink >"$prefix.kind" || die "could not record sentry kind"
        stat -c "$sentry_stat_format" -- "$sentry" >"$prefix.stat" || die "could not stat sentry symlink"
        readlink -- "$sentry" >"$prefix.link-target" || die "could not record sentry link target"
    elif [ ! -e "$sentry" ]; then
        printf '%s\n' absent >"$prefix.kind" || die "could not record absent sentry"
    elif [ -f "$sentry" ]; then
        printf '%s\n' regular-file >"$prefix.kind" || die "could not record sentry kind"
        stat -c "$sentry_stat_format" -- "$sentry" >"$prefix.stat" || die "could not stat sentry file"
        cp -P -- "$sentry" "$prefix.bytes" || die "could not copy sentry bytes"
        [ -f "$prefix.bytes" ] && [ ! -L "$prefix.bytes" ] || die "sentry identity changed during capture"
        chmod 0600 "$prefix.bytes" || die "could not make sentry bytes private"
        sha256sum -- "$prefix.bytes" >"$prefix.sha256" || die "could not hash sentry bytes"
    elif [ -d "$sentry" ]; then
        printf '%s\n' directory >"$prefix.kind" || die "could not record sentry kind"
        stat -c "$sentry_stat_format" -- "$sentry" >"$prefix.stat" || die "could not stat sentry directory"
    else
        printf '%s\n' other >"$prefix.kind" || die "could not record sentry kind"
        stat -c "$sentry_stat_format" -- "$sentry" >"$prefix.stat" || die "could not stat sentry object"
    fi
}

status=0
"$staged" --version >"$evidence/installer-version.txt" 2>&1
installer_version_rc=$?; printf '%s\n' "$installer_version_rc" >"$evidence/installer-version.status"
[ "$installer_version_rc" -eq 0 ] && record PASS "installer version recorded" || { record FAIL "installer --version"; status=1; }
case $lane in
lifecycle)
    endpoint=http://127.0.0.1:18080
    if [ "$phase" = initial ]; then
        capture_sentry_identity before-initial
        "$staged" --help >"$evidence/installer-help.txt" 2>&1 || { record FAIL "installer --help"; status=1; }
        capture_start 18080 diagnostic-initial-requests
        write_argv "$evidence/install.argv" "$staged" --diagnostic-endpoint "$endpoint" install --determinate --no-confirm --no-modify-profile
        "$staged" --diagnostic-endpoint "$endpoint" install --determinate --no-confirm --no-modify-profile >"$evidence/install.output" 2>&1
        rc=$?; printf '%s\n' "$rc" >"$evidence/install.status"
        sleep 2
        capture_stop
        requests=$(cat "$evidence/diagnostic-initial-requests")
        if [ "$rc" -eq 0 ] && [ "$requests" -gt 0 ]; then record PASS "initial install with captured diagnostics"; else record FAIL "initial install or diagnostic capture"; status=1; fi
        receipt=/nix/receipt.json
        installed=/nix/nix-installer
        if [ -L "$receipt" ]; then
            record FAIL "receipt is a symlink"
            status=1
        elif [ -f "$receipt" ] && [ -s "$receipt" ] && [ -x "$installed" ] && [ ! -L "$installed" ] && [ "$(stat -c %F -- "$receipt")" = 'regular file' ]; then
            if stat -c 'type=%F uid=%u gid=%g mode=0%a size=%s links=%h path=%n' -- "$receipt" >"$evidence/receipt.stat" &&
                receipt_hash=$(sha256 "$receipt") &&
                installed_hash=$(sha256 "$installed") &&
                [ "$installed_hash" = "$expected" ]; then
                printf '%s\n' "$receipt_hash" >"$evidence/receipt.sha256"
                printf '%s\n' "$installed_hash" >"$evidence/installed-installer.sha256"
                record PASS "opaque receipt metadata, private hash, and installed copy identity"
            else record FAIL "receipt metadata, private hash, or installed copy identity"; status=1; fi
        else record FAIL "receipt or installed copy missing or unsafe"; status=1; fi
        capture_sentry_identity after-initial
        snapshot after-initial-install.txt
        [ "$status" -eq 0 ] || exit 1
        before_boot=$(cat /proc/sys/kernel/random/boot_id)
        [ -n "$before_boot" ] || { record FAIL "initial boot ID is empty"; exit 1; }
        printf '%s\n' "$before_boot" >"$evidence/boot-id.before"
        record PASS "clean reboot requested after initial install"
        exit 194
    else
        before_boot=$(cat "$evidence/boot-id.before")
        after_boot=$(cat /proc/sys/kernel/random/boot_id)
        printf '%s\n' "$after_boot" >"$evidence/boot-id.after"
        if [ -n "$before_boot" ] && [ -n "$after_boot" ] && [ "$before_boot" != "$after_boot" ]; then record PASS "clean reboot changed boot ID"; else record FAIL "clean reboot did not change boot ID"; status=1; fi
    fi
    capture_start 18080 diagnostic-repeat-requests
    write_argv "$evidence/repeat-install.argv" "$staged" --diagnostic-endpoint "$endpoint" install --determinate --no-confirm --no-modify-profile
    "$staged" --diagnostic-endpoint "$endpoint" install --determinate --no-confirm --no-modify-profile >"$evidence/repeat-install.output" 2>&1
    repeat_rc=$?; printf '%s\n' "$repeat_rc" >"$evidence/repeat-install.status"
    sleep 2
    capture_stop
    repeat_requests=$(cat "$evidence/diagnostic-repeat-requests")
    repeat_counter_ok=1
    case $repeat_requests in ''|*[!0-9]*) repeat_counter_ok=0 ;; esac
    if [ -s /nix/receipt.json ] && [ -x /nix/nix-installer ] && [ "$repeat_counter_ok" -eq 1 ]; then record PASS "repeat install observed with status $repeat_rc, $repeat_requests diagnostic requests, and install intact"; else record FAIL "repeat install damaged installed state or diagnostic evidence"; status=1; fi
    write_argv "$evidence/repair.argv" /nix/nix-installer --diagnostic-endpoint '' repair --no-confirm
    /nix/nix-installer --diagnostic-endpoint '' repair --no-confirm >"$evidence/repair.output" 2>&1
    repair_rc=$?; printf '%s\n' "$repair_rc" >"$evidence/repair.status"
    [ "$repair_rc" -eq 0 ] && record PASS "default repair" || { record FAIL "default repair"; status=1; }
    write_argv "$evidence/repair-sequoia.argv" /nix/nix-installer --diagnostic-endpoint '' repair sequoia --no-confirm
    /nix/nix-installer --diagnostic-endpoint '' repair sequoia --no-confirm >"$evidence/repair-sequoia.output" 2>&1
    sequoia_rc=$?; printf '%s\n' "$sequoia_rc" >"$evidence/repair-sequoia.status"
    if [ "$sequoia_rc" -ne 0 ] && grep -F 'only available on macOS' "$evidence/repair-sequoia.output" >/dev/null; then record PASS "Linux sequoia refusal"; else record FAIL "Linux sequoia refusal was not observed"; status=1; fi
    absent=0
    for command_name in update upgrade self-update; do
        write_argv "$evidence/installer-$command_name.argv" "$staged" "$command_name" --help
        "$staged" "$command_name" --help >"$evidence/installer-$command_name.output" 2>&1
        command_rc=$?; printf '%s\n' "$command_rc" >"$evidence/installer-$command_name.status"
        if [ "$command_rc" -eq 0 ] || ! grep -Ei "unrecognized subcommand.*$command_name|invalid subcommand.*$command_name" "$evidence/installer-$command_name.output" >/dev/null; then absent=1; fi
    done
    [ "$absent" -eq 0 ] && record PASS "installer update, upgrade, and self-update commands rejected" || { record FAIL "installer exposes an update command"; status=1; }
    nixd=/usr/local/bin/determinate-nixd
    if [ -x "$nixd" ] && [ ! -L "$nixd" ]; then
        stat -c '%a %U:%G %n' "$nixd" >"$evidence/determinate-nixd.mode"
        nixd_mode=$(stat -c '%a %U:%G' "$nixd")
        "$nixd" version >"$evidence/determinate-nixd-version.output" 2>&1; version_rc=$?; printf '%s\n' "$version_rc" >"$evidence/determinate-nixd-version.status"
        "$nixd" upgrade --help >"$evidence/determinate-nixd-upgrade-help.output" 2>&1; help_rc=$?; printf '%s\n' "$help_rc" >"$evidence/determinate-nixd-upgrade-help.status"
        if [ "$nixd_mode" = '555 root:root' ] && [ "$version_rc" -eq 0 ] && [ "$help_rc" -eq 0 ]; then record PASS "determinate-nixd absolute path, mode, version, and upgrade help"; else record FAIL "determinate-nixd mode or CLI surface"; status=1; fi
        write_argv "$evidence/determinate-nixd-upgrade.argv" "$nixd" upgrade --version v3.22.1
        "$nixd" upgrade --version v3.22.1 >"$evidence/determinate-nixd-upgrade.output" 2>&1
        probe_rc=$?; printf '%s\n' "$probe_rc" >"$evidence/determinate-nixd-upgrade.status"
        [ "$probe_rc" -eq 0 ] && record PASS "required pinned same-version daemon upgrade" || { record FAIL "required pinned same-version daemon upgrade"; status=1; }
    else record FAIL "determinate-nixd missing or unsafe"; status=1; fi
    capture_sentry_identity after-determinate-nixd-upgrade
    snapshot after-install.txt
    write_argv "$evidence/uninstall.argv" /nix/nix-installer --diagnostic-endpoint '' uninstall --no-confirm /nix/receipt.json
    /nix/nix-installer --diagnostic-endpoint '' uninstall --no-confirm /nix/receipt.json >"$evidence/uninstall.output" 2>&1
    uninstall_rc=$?; printf '%s\n' "$uninstall_rc" >"$evidence/uninstall.status"
    capture_sentry_identity after-uninstall
    [ "$uninstall_rc" -eq 0 ] && record PASS "uninstall behavior observed" || { record FAIL "uninstall behavior"; status=1; }
    receipt_absent=0
    [ ! -e /nix/receipt.json ] && [ ! -L /nix/receipt.json ] && receipt_absent=1
    write_argv "$evidence/repeat-uninstall.argv" "$staged" --diagnostic-endpoint '' uninstall --no-confirm /nix/receipt.json
    "$staged" --diagnostic-endpoint '' uninstall --no-confirm /nix/receipt.json >"$evidence/repeat-uninstall.output" 2>&1
    repeat_uninstall_rc=$?; printf '%s\n' "$repeat_uninstall_rc" >"$evidence/repeat-uninstall.status"
    repeat_uninstall_ok=0
    if [ "$receipt_absent" -eq 1 ] && [ "$repeat_uninstall_rc" -eq 1 ] && grep -F 'Reading receipt' "$evidence/repeat-uninstall.output" >/dev/null && grep -F 'No such file or directory' "$evidence/repeat-uninstall.output" >/dev/null; then
        repeat_uninstall_ok=1
        record PASS "repeat uninstall observed pinned missing-receipt refusal"
    else record FAIL "repeat uninstall did not match pinned missing-receipt refusal"; status=1; fi
    snapshot after-uninstall.txt
    etc_nix_ok=1
    : >"$evidence/etc-nix.first-entry"
    if [ -L /etc/nix ]; then
        stat -c '%F %a %U:%G %n' /etc/nix >"$evidence/etc-nix.stat" 2>&1 || :
        etc_nix_ok=0
    elif [ -e /etc/nix ]; then
        stat -c '%F %a %U:%G %n' /etc/nix >"$evidence/etc-nix.stat" 2>&1 || :
        find /etc/nix -mindepth 1 -print -quit >"$evidence/etc-nix.first-entry"
        etc_nix_mode_owner=$(stat -c '%a %U:%G' /etc/nix)
        [ -d /etc/nix ] && [ "$etc_nix_mode_owner" = '755 root:root' ] && [ ! -s "$evidence/etc-nix.first-entry" ] || etc_nix_ok=0
    else
        printf '%s\n' absent >"$evidence/etc-nix.stat"
    fi
    [ "$etc_nix_ok" -eq 1 ] && record PASS "/etc/nix is absent or an empty root-owned 0755 directory" || { record FAIL "/etc/nix is unsafe or nonempty"; status=1; }
    {
        for path in /nix/receipt.json /nix /usr/local/bin/determinate-nixd; do [ ! -e "$path" ] && [ ! -L "$path" ] || printf '%s\n' "$path"; done
        find /etc/systemd/system /usr/lib/systemd/system /lib/systemd/system \( -type f -o -type l \) \( -iname '*nix*' -o -iname '*determinate*' \) -print 2>/dev/null
        find /usr/local/bin -maxdepth 1 \( -type f -o -type l \) \( -iname '*nix*' -o -iname '*determinate*' \) -print 2>/dev/null
        getent passwd | cut -d: -f1 | grep -E '^nixbld[0-9]+$' || :
    } >"$evidence/residue.txt"
    if [ "$uninstall_rc" -eq 0 ] && [ "$repeat_uninstall_ok" -eq 1 ] && [ "$etc_nix_ok" -eq 1 ] && [ ! -s "$evidence/residue.txt" ]; then record PASS "uninstall observations satisfy the pinned residue contract"; else record FAIL "uninstall observations violate the pinned residue contract"; status=1; fi
    ;;
diagnostics-disabled)
    [ "$phase" = initial ] || die "unexpected diagnostics continuation"
    capture_start 18081 diagnostic-disabled-requests
    transport=http://127.0.0.1:18081
    write_argv "$evidence/install.argv" "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile
    DETSYS_IDS_TRANSPORT=$transport "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile >"$evidence/install.output" 2>&1
    rc=$?; printf '%s\n' "$rc" >"$evidence/install.status"
    sleep 2
    capture_stop
    requests=$(cat "$evidence/diagnostic-disabled-requests")
    if [ "$rc" -eq 0 ] && [ "$requests" -eq 0 ]; then record PASS "empty diagnostic endpoint made zero requests"; else record FAIL "diagnostics-disabled install or zero-request proof"; status=1; fi
    ;;
crash-recovery)
    if [ "$phase" = initial ]; then
        write_argv "$evidence/install.argv" "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile
        "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile >"$evidence/install.output" 2>&1 &
        install_pid=$!
        progressed=0; i=0
        while [ "$i" -lt 1800 ]; do
            store_entry=$(find /nix/store -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null || true)
            if [ -x /usr/local/bin/determinate-nixd ] && [ -n "$store_entry" ]; then progressed=1; break; fi
            kill -0 "$install_pid" 2>/dev/null || break
            i=$((i + 1)); sleep 1
        done
        if [ "$progressed" -ne 1 ] || ! kill -0 "$install_pid" 2>/dev/null; then record UNPROVED "could not reach observable in-progress install"; snapshot crash-unproved.txt; exit 2; fi
        printf '%s\n' "determinate-nixd plus non-empty Nix store: $store_entry" >"$evidence/crash-marker"
        kill -KILL "$install_pid"
        wait "$install_pid" 2>/dev/null || :
        record PASS "killed active installer after observable progress"
        snapshot crash-immediate.txt
        exit 194
    fi
    snapshot crash-after-reboot.txt
    write_argv "$evidence/recovery-install.argv" "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile
    "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile >"$evidence/recovery-install.output" 2>&1
    rc=$?; printf '%s\n' "$rc" >"$evidence/recovery-install.status"
    if [ "$rc" -eq 0 ] && [ -s /nix/receipt.json ] && [ -x /nix/nix-installer ]; then record PASS "install recovered after crash and reboot"; else record FAIL "install did not recover after crash and reboot"; status=1; fi
    snapshot crash-recovered.txt
    ;;
foreign-nix)
    [ "$phase" = initial ] || die "unexpected foreign-nix continuation"
    mkdir -m 0755 /nix
    printf '%s' 'pkg-s6-foreign-nix-sentinel' >/nix/pkg-s6-sentinel
    sentinel_before=$(sha256 /nix/pkg-s6-sentinel)
    write_argv "$evidence/install.argv" "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile
    "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile >"$evidence/install.output" 2>&1
    rc=$?; printf '%s\n' "$rc" >"$evidence/install.status"
    if [ -f /nix/pkg-s6-sentinel ] && [ "$(sha256 /nix/pkg-s6-sentinel)" = "$sentinel_before" ]; then record PASS "foreign /nix sentinel preserved; vendor result $rc"; else record FAIL "foreign /nix sentinel changed or disappeared"; status=1; fi
    snapshot foreign-after.txt
    ;;
upstream-input)
    [ "$phase" = initial ] || die "unexpected upstream-input continuation"
    write_argv "$evidence/upstream-install.argv" "$staged" --diagnostic-endpoint '' install --prefer-upstream-nix --no-confirm --no-modify-profile
    "$staged" --diagnostic-endpoint '' install --prefer-upstream-nix --no-confirm --no-modify-profile >"$evidence/upstream-install.output" 2>&1
    upstream_rc=$?; printf '%s\n' "$upstream_rc" >"$evidence/upstream-install.status"
    upstream_nix=/nix/var/nix/profiles/default/bin/nix
    if [ "$upstream_rc" -eq 0 ] && [ -s /nix/receipt.json ] && [ -x "$upstream_nix" ]; then
        "$upstream_nix" --version >"$evidence/upstream-nix-version.output" 2>&1
        nix_rc=$?; printf '%s\n' "$nix_rc" >"$evidence/upstream-nix-version.status"
        [ "$nix_rc" -eq 0 ] && record PASS "real upstream Nix input created by pinned installer" || { record FAIL "upstream Nix executable failed"; status=1; }
    else record FAIL "upstream Nix input was not created"; status=1; fi
    write_argv "$evidence/determinate-after-upstream.argv" "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile
    "$staged" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile >"$evidence/determinate-after-upstream.output" 2>&1
    determinate_rc=$?; printf '%s\n' "$determinate_rc" >"$evidence/determinate-after-upstream.status"
    if [ -s /nix/receipt.json ]; then record PASS "Determinate-on-upstream refusal or result recorded with status $determinate_rc"; else record FAIL "Determinate-on-upstream run destroyed the upstream receipt"; status=1; fi
    snapshot upstream-after.txt
    ;;
esac

snapshot "after-$phase.txt"
find "$evidence" -type d -exec chmod 0700 {} \;
find "$evidence" -type f -exec chmod 0600 {} \;
[ "$status" -eq 0 ] || exit 1
exit 0
