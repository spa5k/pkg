#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
OUTPUT_ROOT="$SCRIPT_DIR/out/native-macos"
PRIVATE_ROOT=$(mktemp -d /var/tmp/pkg-s5-native-evidence.XXXXXX)
BROKER_HOME=$(mktemp -d /var/tmp/pkg-s5-nix-broker.XXXXXX)
REPORT="$PRIVATE_ROOT/report.json"
PROBE_LOG="$PRIVATE_ROOT/probe.log"
FIXTURE_ROOT="$BROKER_HOME/fixtures"
NIX=/nix/var/nix/profiles/default/bin/nix
OWNER_UID=${S5_EVIDENCE_OWNER_UID:-0}
OWNER_GID=${S5_EVIDENCE_OWNER_GID:-0}

cleanup() {
    rm -rf "$PRIVATE_ROOT" "$BROKER_HOME"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

mkdir -p "$FIXTURE_ROOT"
chmod 700 "$PRIVATE_ROOT" "$BROKER_HOME" "$FIXTURE_ROOT"
touch "$PROBE_LOG"
chmod 600 "$PROBE_LOG"

finish_files() {
    if [ "$OWNER_UID" -eq 0 ]; then
        mkdir -p "$OUTPUT_ROOT"
        cp "$REPORT" "$OUTPUT_ROOT/report.json"
        cp "$PROBE_LOG" "$OUTPUT_ROOT/probe.log"
        chmod 700 "$OUTPUT_ROOT"
        chmod 600 "$OUTPUT_ROOT/report.json" "$OUTPUT_ROOT/probe.log"
        return
    fi

    owner_name=$(id -nu "$OWNER_UID") || return 1
    sudo -u "$owner_name" mkdir -p "$OUTPUT_ROOT"
    sudo -u "$owner_name" tee "$OUTPUT_ROOT/report.json" < "$REPORT" >/dev/null
    sudo -u "$owner_name" tee "$OUTPUT_ROOT/probe.log" < "$PROBE_LOG" >/dev/null
    sudo -u "$owner_name" chmod 700 "$OUTPUT_ROOT"
    sudo -u "$owner_name" chmod 600 "$OUTPUT_ROOT/report.json" "$OUTPUT_ROOT/probe.log"
}

fail() {
    reason=$1
    printf '{"schemaVersion":1,"mode":"observed","platform":"native-macos","complete":false,"failure":"%s"}\n' "$reason" > "$REPORT"
    finish_files
    exit 69
}

[ "$(id -u)" -eq 0 ] || fail not_root
[ "$(uname -s)" = Darwin ] || fail not_darwin
[ "$(uname -m)" = arm64 ] || fail unsupported_architecture
[ -x "$NIX" ] || fail nix_missing
[ "$($NIX --version)" = 'nix (Nix) 2.34.8' ] || fail wrong_nix_version
id pkg-nix-broker >/dev/null 2>&1 || fail broker_missing
dscl . -read /Groups/nixbld >/dev/null 2>&1 || fail build_group_missing
dscl . -read /Users/_nixbld1 >/dev/null 2>&1 || fail build_user_missing
launchctl print system/org.nixos.nix-daemon >/dev/null 2>&1 || fail daemon_not_running
[ "$(stat -f '%Sp' /nix/var/nix/daemon-socket)" = drwxr-x--- ] || fail wrong_socket_parent_mode
[ "$(stat -f '%Sg' /nix/var/nix/daemon-socket)" = pkg-nix-broker ] || fail wrong_socket_parent_group

store_encryption=$(diskutil info /nix | awk -F: '/FileVault/ {gsub(/^[[:space:]]+/, "", $2); print $2}')
[ "$store_encryption" = Yes ] || fail store_not_encrypted
xcode_path=$(xcode-select -p 2>/dev/null || true)
[ -n "$xcode_path" ] || fail xcode_tools_missing

cp "$SCRIPT_DIR/fixtures/regular-network.nix" "$FIXTURE_ROOT/"
cp "$SCRIPT_DIR/fixtures/fixed-output-network.nix" "$FIXTURE_ROOT/"
cp "$SCRIPT_DIR/fixtures/build-user-macos.nix" "$FIXTURE_ROOT/"
chown -R pkg-nix-broker:pkg-nix-broker "$BROKER_HOME"
chmod 700 "$BROKER_HOME" "$FIXTURE_ROOT"
chmod 600 "$FIXTURE_ROOT"/*.nix
cd "$BROKER_HOME"

run_broker() {
    sudo -u pkg-nix-broker env \
        HOME="$BROKER_HOME" \
        TMPDIR="$BROKER_HOME" \
        XDG_CACHE_HOME="$BROKER_HOME/cache" \
        NIX_REMOTE=daemon \
        "$@"
}

console_user=$(stat -f '%Su' /dev/console)
[ "$console_user" != root ] || fail console_user_unavailable
if sudo -u "$console_user" env NIX_REMOTE=daemon "$NIX" store info >>"$PROBE_LOG" 2>&1; then
    fail ordinary_user_reached_daemon
fi

run_broker "$NIX" store info >>"$PROBE_LOG" 2>&1 || fail broker_ping_failed
[ "$(run_broker "$NIX" config show sandbox)" = true ] || fail sandbox_not_true
[ "$(run_broker "$NIX" config show sandbox-fallback)" = false ] || fail sandbox_fallback_not_false
[ "$(run_broker "$NIX" config show build-users-group)" = nixbld ] || fail wrong_build_group

if [ "${S5_BUILD_APPROVAL:-not-approved}" != single-operation-approved ]; then
    printf '{"schemaVersion":1,"mode":"observed","platform":"native-macos","complete":false,"status":"readiness_only","nixVersion":"2.34.8","system":"aarch64-darwin","storeEncrypted":true,"sandbox":true,"sandboxFallback":false,"buildUsersGroup":"nixbld","ordinaryUserProbe":"socket_traversal_denied","brokerProbe":"daemon_reachable","networkProbe":"pending_approval","fixedOutputProbe":"pending_approval","buildUserProbe":"pending_approval","approvalProbe":"not_approved","resourceCapsClaimed":false,"resourceBoundary":"no_cgroups_on_macos"}\n' > "$REPORT"
    finish_files
    exit 0
fi
unset S5_BUILD_APPROVAL

regular_output=$(run_broker "$NIX" build \
    --file "$FIXTURE_ROOT/regular-network.nix" \
    --argstr system aarch64-darwin \
    --no-link --print-out-paths 2>>"$PROBE_LOG") || fail regular_network_build_failed
regular_result=''
IFS= read -r regular_result < "$regular_output" || true
[ "$regular_result" = network-denied ] || fail regular_network_was_not_denied

fixed_output=$(run_broker "$NIX" build \
    --file "$FIXTURE_ROOT/fixed-output-network.nix" \
    --argstr system aarch64-darwin \
    --no-link --print-out-paths 2>>"$PROBE_LOG") || fail fixed_output_build_failed
[ -s "$fixed_output" ] || fail fixed_output_was_empty

build_user_output=$(run_broker "$NIX" build \
    --file "$FIXTURE_ROOT/build-user-macos.nix" \
    --argstr system aarch64-darwin \
    --no-link --print-out-paths 2>>"$PROBE_LOG") || fail build_user_probe_failed
build_uid=''
IFS= read -r build_uid < "$build_user_output" || true
case "$build_uid" in
    35[1-9]|3[67][0-9]|38[0-2]) ;;
    *) fail unexpected_build_uid ;;
esac

printf '{"schemaVersion":1,"mode":"observed","platform":"native-macos","complete":true,"nixVersion":"2.34.8","system":"aarch64-darwin","storeEncrypted":true,"xcodeToolsReady":true,"sandbox":true,"sandboxFallback":false,"buildUsersGroup":"nixbld","buildUserUid":%s,"ordinaryUserProbe":"socket_traversal_denied","brokerProbe":"daemon_reachable","networkProbe":"denied_regular_derivation","fixedOutputProbe":"network_enabled_hash_verified","approvalProbe":"explicit_run_flag_consumed","resourceCapsClaimed":false,"resourceBoundary":"no_cgroups_on_macos"}\n' "$build_uid" > "$REPORT"
finish_files
