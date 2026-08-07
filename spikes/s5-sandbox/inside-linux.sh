#!/bin/sh
# Runs as root inside the disposable privileged Linux evidence container.
set -eu

NIX_VERSION='2.34.8'
REPORT='/out/report.json'
DAEMON_LOG='/out/nix-daemon.log'
CONFIG='/tmp/nix.conf'

fail() {
    reason=$1
    printf '{"schemaVersion":1,"mode":"observed","platform":"linux-docker","complete":false,"failure":"%s"}\n' "$reason" > "$REPORT"
    chmod 600 "$REPORT"
    exit 69
}

version_line=$(nix --version)
actual_version=${version_line##* }
[ "$actual_version" = "$NIX_VERSION" ] || fail 'wrong_nix_version'
[ "$(uname -s)" = 'Linux' ] || fail 'not_linux'
[ "$(id -u)" = '0' ] || fail 'not_root'
grep -q '^nixbld:' /etc/group || fail 'build_group_missing'
grep -q '^nixbld1:' /etc/passwd || fail 'build_user_missing'
[ -f /sys/fs/cgroup/cgroup.controllers ] || fail 'cgroup_v2_missing'

NIX_CONFIG_VALUE=$(/bin/sh /harness/render-nix-conf.sh linux) || fail 'config_render_failed'
printf '%s\n' "$NIX_CONFIG_VALUE" > "$CONFIG"
chmod 600 "$CONFIG"
export NIX_CONFIG="$NIX_CONFIG_VALUE"

mkdir -p /nix/var/nix/daemon-socket
nix-daemon >"$DAEMON_LOG" 2>&1 &
daemon_pid=$!
cleanup() {
    kill "$daemon_pid" >/dev/null 2>&1 || true
    wait "$daemon_pid" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

ready=false
i=0
while [ "$i" -lt 200 ]; do
    if [ -S /nix/var/nix/daemon-socket/socket ]; then
        ready=true
        break
    fi
    i=$((i + 1))
    sleep 0.05
done
[ "$ready" = true ] || fail 'daemon_socket_unready'

export NIX_REMOTE=daemon
nix store ping >/dev/null 2>&1 || fail 'daemon_ping_failed'
[ "$(nix config show sandbox)" = true ] || fail 'sandbox_not_true'
[ "$(nix config show sandbox-fallback)" = false ] || fail 'sandbox_fallback_not_false'
[ "$(nix config show build-users-group)" = nixbld ] || fail 'wrong_build_group'
[ "$(nix config show use-cgroups)" = true ] || fail 'cgroups_not_enabled'

IFS= read -r controllers < /sys/fs/cgroup/cgroup.controllers
if [ "${S5_BUILD_APPROVAL:-not-approved}" != 'single-operation-approved' ]; then
    printf '{"schemaVersion":1,"mode":"observed","platform":"linux-docker","complete":false,"status":"readiness_only","nixVersion":"%s","sandbox":true,"sandboxFallback":false,"buildUsersGroup":"nixbld","useCgroups":true,"cgroupControllers":"%s","networkProbe":"pending_approval","fixedOutputProbe":"pending_approval","approvalProbe":"not_approved","resourceCapsClaimed":false}\n' \
        "$actual_version" "$controllers" > "$REPORT"
    chmod 600 "$REPORT" "$DAEMON_LOG"
    exit 0
fi
unset S5_BUILD_APPROVAL

case "$(uname -m)" in
    aarch64) system='aarch64-linux' ;;
    x86_64) system='x86_64-linux' ;;
    *) fail 'unsupported_linux_architecture' ;;
esac

regular_output=$(nix build \
    --file /harness/fixtures/regular-network.nix \
    --argstr system "$system" \
    --no-link \
    --print-out-paths 2>>"$DAEMON_LOG") || fail 'regular_network_build_failed'
regular_result=''
IFS= read -r regular_result < "$regular_output" || true
[ "$regular_result" = 'network-denied' ] || fail 'regular_network_was_not_denied'

fixed_output=$(nix build \
    --file /harness/fixtures/fixed-output-network.nix \
    --argstr system "$system" \
    --no-link \
    --print-out-paths 2>>"$DAEMON_LOG") || fail 'fixed_output_build_failed'
[ -s "$fixed_output" ] || fail 'fixed_output_was_empty'

cgroup_output='/tmp/cgroup-probe-output'
nix build \
    --file /harness/fixtures/cgroup-boundary.nix \
    --argstr system "$system" \
    --out-link "$cgroup_output" 2>>"$DAEMON_LOG" &
cgroup_build_pid=$!
cgroup_path=''
i=0
while [ "$i" -lt 200 ]; do
    for candidate in \
        /sys/fs/cgroup/* \
        /sys/fs/cgroup/*/* \
        /sys/fs/cgroup/*/*/* \
        /sys/fs/cgroup/*/*/*/*; do
        [ -f "$candidate/cgroup.procs" ] || continue
        while IFS= read -r candidate_pid; do
            if [ -r "/proc/$candidate_pid/status" ] \
                && grep -q '^Uid:[[:space:]]*300[0-9][0-9]' "/proc/$candidate_pid/status"; then
                cgroup_path=$candidate
                break
            fi
        done < "$candidate/cgroup.procs"
        [ -n "$cgroup_path" ] && break
    done
    [ -n "$cgroup_path" ] && break
    i=$((i + 1))
    sleep 0.05
done
[ -n "$cgroup_path" ] || fail 'per_build_cgroup_not_observed'

read_limit() {
    limit_file=$1
    limit_value=''
    if [ ! -f "$limit_file" ]; then
        limit_value='absent'
        return
    fi
    IFS= read -r limit_value < "$limit_file" || fail 'cgroup_limit_unreadable'
}

read_limit "$cgroup_path/memory.max"
memory_max=$limit_value
read_limit "$cgroup_path/pids.max"
pids_max=$limit_value
read_limit "$cgroup_path/cpu.max"
cpu_max=$limit_value
case "$memory_max" in max|absent) ;; *) fail 'unexpected_memory_cap' ;; esac
case "$pids_max" in max|absent) ;; *) fail 'unexpected_pids_cap' ;; esac
case "$cpu_max" in max|'max '*|absent) ;; *) fail 'unexpected_cpu_cap' ;; esac
wait "$cgroup_build_pid" || fail 'cgroup_probe_build_failed'
[ ! -d "$cgroup_path" ] || fail 'per_build_cgroup_not_cleaned'

printf '{"schemaVersion":1,"mode":"observed","platform":"linux-docker","complete":true,"nixVersion":"%s","sandbox":true,"sandboxFallback":false,"buildUsersGroup":"nixbld","buildUserProbe":"uid_30001_observed","useCgroups":true,"cgroupControllers":"%s","networkProbe":"denied_regular_derivation","fixedOutputProbe":"network_enabled_hash_verified","approvalProbe":"explicit_run_flag_consumed","cgroupProbe":"created_unlimited_then_cleaned","memoryMax":"%s","cpuMax":"%s","pidsMax":"%s","resourceCapsClaimed":false}\n' \
    "$actual_version" "$controllers" "$memory_max" "$cpu_max" "$pids_max" > "$REPORT"
chmod 600 "$REPORT" "$DAEMON_LOG"
