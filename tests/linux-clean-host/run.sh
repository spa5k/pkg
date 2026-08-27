#!/bin/sh
set -eu

case "${1-}" in
    '') artifact_output= ;;
    --keep-artifacts)
        [ "$#" -eq 2 ] || { echo "usage: $0 [--keep-artifacts DIR]" >&2; exit 2; }
        artifact_output=$2
        ;;
    *) echo "usage: $0 [--keep-artifacts DIR]" >&2; exit 2 ;;
esac

docker_arch=$(docker version --format '{{.Server.Arch}}')
case "$docker_arch" in
    amd64|x86_64) ;;
    *) echo "Linux clean-host proof requires a native x86_64/amd64 Docker server; found $docker_arch." >&2; exit 1 ;;
esac

repo=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
if [ -n "$(git -C "$repo" status --porcelain --untracked-files=all)" ]; then
    echo "Linux clean-host proof requires an exact clean commit." >&2
    exit 1
fi
python3 "$repo/tests/linux-clean-host/test_vendor_trace.py" >/dev/null
stage_root=$(mktemp -d "${TMPDIR:-/tmp}/pkg-linux-alpha.XXXXXXXX")
raw_stage="$stage_root/raw"
artifact_context="$stage_root/artifact"
evidence_root="$stage_root/evidence"
docker_platform=linux/amd64

if [ -n "$artifact_output" ]; then
    : "${PKG_CARGO_ABOUT:?set PKG_CARGO_ABOUT for a candidate archive}"
    if [ -e "$artifact_output" ] || [ -L "$artifact_output" ]; then
        echo "artifact output must not exist: $artifact_output" >&2
        exit 1
    fi
    mkdir -p -m 0700 "$artifact_output/evidence"
    evidence_root="$artifact_output/evidence"
fi

image=pkg-linux-clean-host:local
container="pkg-linux-clean-host-$$"
diagnostic_container="${container}-vendor-trace"
stop_container() {
    docker rm --force "$container" >/dev/null 2>&1 || true
    docker rm --force "$diagnostic_container" >/dev/null 2>&1 || true
}
handoff_is_started() {
    [ "$(docker inspect --format '{{.State.Running}}' "$container" 2>/dev/null)" = true ] \
        || return 1
    docker exec -i "$container" python3 - <<'PY'
import os
import stat
from pathlib import Path

try:
    descriptor = os.open(
        "/var/lib/pkg-install/determinate-handoff-v1.json",
        os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC,
    )
except OSError:
    raise SystemExit(1)
try:
    metadata = os.fstat(descriptor)
    chunks = []
    remaining = 47
    while remaining:
        chunk = os.read(descriptor, remaining)
        if not chunk:
            break
        chunks.append(chunk)
        remaining -= len(chunk)
    valid = (
        stat.S_ISREG(metadata.st_mode)
        and metadata.st_uid == 0
        and metadata.st_gid == 0
        and stat.S_IMODE(metadata.st_mode) == 0o600
        and metadata.st_nlink == 1
        and metadata.st_size == 47
        and not remaining
        and b"".join(chunks) == b'{"schema_version":1,"state":{"kind":"started"}}'
        and os.read(descriptor, 1) == b""
        and not Path("/nix").exists()
        and not Path("/nix").is_symlink()
    )
finally:
    os.close(descriptor)
raise SystemExit(not valid)
PY
}
copy_replay_file() {
    source=$1
    target=$2
    limit=$3
    umask 077
    docker exec "$diagnostic_container" python3 \
        /usr/local/libexec/pkg_bounded_capture.py copy "$source" "$limit" \
        > "$target" 2>/dev/null || : > "$target"
    chmod 0600 "$target"
    test "$(wc -c < "$target")" -le "$limit"
}
capture_vendor_replay() {
    failure=$1
    handoff_is_started || return 0
    replay=$failure/vendor-trace-replay
    umask 077
    mkdir -m 0700 "$replay" || return 0
    docker rm --force "$diagnostic_container" >/dev/null 2>&1 || true
    docker run \
        --detach \
        --privileged \
        --platform "$docker_platform" \
        --cgroupns=private \
        --name "$diagnostic_container" \
        --tmpfs /run \
        --tmpfs /run/lock \
        "$image" >/dev/null 2>&1 || return 0
    if ! wait_container_ready "$diagnostic_container" >/dev/null 2>&1; then
        printf 'setup=container-not-ready\n' > "$replay/setup-status.txt"
        docker rm --force "$diagnostic_container" >/dev/null 2>&1 || true
        return 0
    fi
    if ! docker exec "$diagnostic_container" sh -eu -c '
        groupadd --gid 30033 --system pkg-nix-broker
        useradd --uid 30033 --gid 30033 --system --no-create-home \
            --home-dir /var/lib/pkg/broker-home --shell /usr/sbin/nologin pkg-nix-broker
        install -d -m 0700 -o 30033 -g 30033 /var/lib/pkg/broker-home
        install -d -m 0700 -o root -g root /var/lib/pkg-install/tmp
        test "$(getent group pkg-nix-broker)" = "pkg-nix-broker:x:30033:"
        test "$(getent passwd pkg-nix-broker | cut -d: -f3-4,6-7)" = \
            "30033:30033:/var/lib/pkg/broker-home:/usr/sbin/nologin"
        test "$(stat -c %u:%g:%a /var/lib/pkg/broker-home)" = 30033:30033:700
        test "$(stat -c %u:%g:%a /var/lib/pkg-install/tmp)" = 0:0:700
        vendor=/srv/pkg-release/targets/9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c.determinate/3.22.1/nix-installer-x86_64-linux
        resolved=$(readlink -f "$vendor")
        test "$resolved" = /srv/pkg-releases/1/targets/9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c.determinate/3.22.1/nix-installer-x86_64-linux
        test ! -L "$vendor"
        test -f "$resolved"
        test ! -L "$resolved"
        test "$(stat -c %u:%g:%a:%s "$resolved")" = 0:0:644:74918096
        printf "%s  %s\n" 9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c "$resolved" \
            | sha256sum --check --strict --status
        install -m 0700 -o root -g root "$resolved" /var/lib/pkg-install/tmp/nix-installer
        staged=/var/lib/pkg-install/tmp/nix-installer
        test "$(stat -c %u:%g:%a:%s "$staged")" = 0:0:700:74918096
        printf "%s  %s\n" 9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c "$staged" \
            | sha256sum --check --strict --status
        install -d -m 0700 -o root -g root /run/pkg-vendor-trace
        printf "%s\n" "$resolved" > /run/pkg-vendor-trace/vendor-path.txt
        chmod 0600 /run/pkg-vendor-trace/vendor-path.txt
        printf "setup=passed\n" > /run/pkg-vendor-trace/setup-status.txt
        chmod 0600 /run/pkg-vendor-trace/setup-status.txt
    ' >/dev/null 2>&1; then
        printf 'setup=failed\n' > "$replay/setup-status.txt"
        docker rm --force "$diagnostic_container" >/dev/null 2>&1 || true
        return 0
    fi
    docker exec "$diagnostic_container" python3 \
        /usr/local/libexec/pkg_bounded_capture.py 1048576 \
        /run/pkg-vendor-trace/trace-status.txt \
        /run/pkg-vendor-trace/trace.stdout \
        /run/pkg-vendor-trace/trace.stderr -- \
        timeout --signal=TERM --kill-after=10s 1200s \
        env -i \
        HOME=/root \
        PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
        TMPDIR=/var/lib/pkg-install/tmp \
        DETSYS_IDS_TELEMETRY=disabled \
        /var/lib/pkg-install/tmp/nix-installer \
        --diagnostic-endpoint http://127.0.0.1:18080 \
        --logger pretty --log-directive nix_installer=trace -vv \
        install --determinate --no-confirm --no-modify-profile \
        >/dev/null 2>&1 || true
    docker exec "$diagnostic_container" python3 \
        /usr/local/libexec/pkg_bounded_capture.py 262144 \
        /run/pkg-vendor-trace/system-status.txt \
        /run/pkg-vendor-trace/system.stdout \
        /run/pkg-vendor-trace/system.stderr -- \
        timeout --signal=TERM --kill-after=5s 30s \
        sh -c 'systemctl --failed --no-pager; systemctl list-units --all --no-pager "nix-*" "determinate-*"; journalctl --no-pager -n 2000' \
        >/dev/null 2>&1 || true
    for name in setup-status.txt vendor-path.txt trace-status.txt system-status.txt; do
        copy_replay_file "/run/pkg-vendor-trace/$name" "$replay/$name" 4096 || true
    done
    copy_replay_file /run/pkg-vendor-trace/trace.stdout "$replay/trace.stdout" 1048576 || true
    remaining=$((1048576 - $(wc -c < "$replay/trace.stdout")))
    copy_replay_file /run/pkg-vendor-trace/trace.stderr "$replay/trace.stderr" "$remaining" || true
    copy_replay_file /run/pkg-vendor-trace/system.stdout "$replay/system.stdout" 262144 || true
    remaining=$((262144 - $(wc -c < "$replay/system.stdout")))
    copy_replay_file /run/pkg-vendor-trace/system.stderr "$replay/system.stderr" "$remaining" || true
    docker rm --force "$diagnostic_container" >/dev/null 2>&1 || true
    return 0
}
capture_failure() {
    [ -n "$artifact_output" ] || return 0
    failure="$evidence_root/failure"
    mkdir -p "$failure"
    printf 'exit_status=%s\n' "$1" > "$failure/status.txt"
    if ! docker inspect "$container" > "$failure/docker-inspect.json" 2> "$failure/docker-inspect.stderr"; then
        printf '[]\n' > "$failure/docker-inspect.json"
    fi
    docker logs "$container" > "$failure/docker.log" 2>&1 || true
    printf 'container was not running at failure capture\n' > "$failure/final-state.txt"
    printf 'container was not running at failure capture\n' > "$failure/residue.txt"
    if [ "$(docker inspect --format '{{.State.Running}}' "$container" 2>/dev/null)" = true ]; then
        docker exec "$container" sh -c '
            ps -ef
            systemctl --failed --no-pager || true
            systemctl list-units --all --no-pager \
                "nix-*" "determinate-*" "pkg-*" || true
        ' > "$failure/final-state.txt" 2>&1 || true
        docker exec "$container" sh -c '
            for path in /nix /etc/nix /opt/pkg /var/lib/pkg /var/lib/pkg-install \
                /run/pkg /run/pkg-helper /home/proof-user/.local/share/pkg; do
                if test -e "$path" || test -L "$path"; then
                    find "$path" -xdev -maxdepth 4 \
                        -printf "%M %u:%g %s %p -> %l\n" 2>&1 || true
                else
                    printf "absent %s\n" "$path"
                fi
            done
            getent passwd || true
            getent group || true
        ' > "$failure/residue.txt" 2>&1 || true
        umask 077
        docker exec "$container" python3 \
            /usr/local/libexec/pkg_bounded_capture.py copy \
            /var/lib/pkg-install/determinate-handoff-v1.json 4096 \
            > "$failure/handoff.json" 2>/dev/null || : > "$failure/handoff.json"
        chmod 0600 "$failure/handoff.json"
    fi
    capture_vendor_replay "$failure" || true
}
cleanup_after_signal() {
    signal_status=$1
    trap '' INT TERM
    set +e
    stop_container
    rm -rf "$stage_root"
    exit "$signal_status"
}
cleanup() {
    status=$1
    trap 'cleanup_after_signal 130' INT
    trap 'cleanup_after_signal 143' TERM
    set +e
    case "$status" in
        0|130|143) ;;
        *) capture_failure "$status" ;;
    esac
    stop_container
    rm -rf "$stage_root"
}
trap 'status=$?; trap - EXIT; cleanup "$status"; exit "$status"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "+ proof host"
uname -a
docker version --format 'Docker server {{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}'

echo "+ stage x86_64 Linux release inputs"
docker build \
    --platform "$docker_platform" \
    --file "$repo/tests/linux-clean-host/Dockerfile.stage" \
    --output "type=local,dest=$raw_stage" \
    "$repo"

python3 "$repo/tools/release/stage_linux_alpha.py" \
    "$raw_stage/binaries/pkg-install" \
    "$repo/docs/install.sh" \
    "$artifact_context" \
    https://127.0.0.1:8443

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$artifact_context" && sha256sum --check --strict SHA256SUMS)
else
    (cd "$artifact_context" && shasum -a 256 --check SHA256SUMS)
fi

if [ -n "$artifact_output" ]; then
    candidate="$artifact_output/pkg-v0.1.0-alpha.7-linux-x86_64.tar.gz"
    python3 "$repo/tools/release/package_alpha_candidate.py" \
        linux-x86_64 \
        "$artifact_context" \
        "$repo/LICENSE" \
        "$PKG_CARGO_ABOUT" \
        "$candidate"
    candidate_context="$stage_root/candidate"
    mkdir "$candidate_context"
    tar -xzf "$candidate" -C "$candidate_context"
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$candidate_context" && sha256sum --check --strict SHA256SUMS)
    else
        (cd "$candidate_context" && shasum -a 256 --check SHA256SUMS)
    fi
    if tar -tzf "$candidate" | grep -Eq '(^|/)test-binaries(/|$)'; then
        echo "proof-only test binary was included in the candidate" >&2
        exit 1
    fi
    artifact_context=$candidate_context
fi

cp -a "$raw_stage/publication-1" "$raw_stage/publication-2" "$artifact_context/"
cp -a "$raw_stage/test-binaries" "$artifact_context/"
cp "$repo/tests/linux-clean-host/pkg-proof-server.py" \
    "$repo/tests/linux-clean-host/pkg_bounded_capture.py" \
    "$repo/tests/linux-clean-host/pkg-proof-release.service" \
    "$artifact_context/"
cp -a "$artifact_context/v0.1.0-alpha.7" "$artifact_context/publication-1/"
cp -a "$artifact_context/v0.1.0-alpha.7" "$artifact_context/publication-2/"
cp "$raw_stage/binaries/pkg-install-n-plus-1" \
    "$artifact_context/publication-2/v0.1.0-alpha.7/pkg-installer-x86_64-linux"
mkdir -p "$evidence_root"
cp -a "$artifact_context/." "$evidence_root/"

test_binary="$artifact_context/test-binaries/pkg-installer-lib-tests"
test -f "$test_binary"
test -x "$test_binary"
cmp "$test_binary" "$evidence_root/test-binaries/pkg-installer-lib-tests"
if command -v sha256sum >/dev/null 2>&1; then
    test_binary_sha256=$(sha256sum "$test_binary" | awk '{print $1}')
else
    test_binary_sha256=$(shasum -a 256 "$test_binary" | awk '{print $1}')
fi
signed_commit=$(git -C "$repo" rev-parse HEAD)
results="$evidence_root/dn15-results.tsv"
filters="$evidence_root/dn15-filters.tsv"
printf 'record\tcase\trun\tdetail\n' > "$results"
printf 'meta\tsigned_commit\t-\t%s\n' "$signed_commit" >> "$results"
printf 'meta\tdocker_server_arch\t-\t%s\n' "$docker_arch" >> "$results"
printf 'meta\ttest_binary_sha256\t-\t%s\n' "$test_binary_sha256" >> "$results"
cat > "$filters" <<'EOF'
persisted-started-refusal	linux_backend::tests::production_preflight_refuses_persisted_started_without_later_mutation
persisted-started-refusal	bootstrap::tests::started_handoff_preflight_prevents_product_mutation_and_vendor_start
persisted-started-refusal	determinate_handoff::tests::handoff_record_is_atomic_private_strict_and_contains_no_receipt_data
sync-exec-restore	determinate_handoff::tests::synchronous_exec_error_restores_exact_accepted_handoff
sync-exec-restore-failure	determinate_handoff::tests::synchronous_exec_and_restore_failure_is_fail_closed
post-unlink-clear-restore	determinate_handoff::tests::every_post_unlink_clear_failure_restores_exact_accepted_handoff
real-sigkill-unmarked	determinate_handoff::tests::sigkill_after_consume_leaves_unmarked_determinate_state_for_install_refusal
later-outcome-unknown	determinate_handoff::tests::sigkill_after_vendor_exec_keeps_later_outcome_unknown_and_refuses_retry
vendor-action-last	determinate_handoff::tests::terminal_uninstall_consumes_handoff_only_after_identity_revalidation
vendor-action-last	uninstall::tests::linux_vendor_uninstall_is_the_terminal_action
vendor-action-last	uninstall::tests::service_stop_is_a_cleanup_barrier
vendor-action-last	uninstall::tests::cleanup_failures_do_not_skip_residue_verification
vendor-action-last	uninstall::tests::linux_product_cleanup_failure_never_dispatches_terminal_vendor
vendor-action-last	uninstall::tests::residue_failure_has_priority_and_success_is_total
install-process-controls	determinate::tests::operations_use_exact_argv_and_cleared_environment
install-process-controls	determinate::tests::terminal_uninstall_uses_exact_fixed_argv_and_environment
install-process-controls	determinate::tests::executable_authentication_rejects_every_invalid_shape
install-process-controls	determinate::tests::both_large_streams_are_drained_and_capped
install-process-controls	determinate::tests::exit_nonzero_and_signal_are_distinct
install-process-controls	determinate::tests::late_success_is_not_reclassified_as_failure
install-process-controls	determinate::tests::synchronous_supervisor_reaps_child_before_return
install-process-controls	determinate::tests::diagnostics_never_expose_captured_bytes_or_paths
install-process-controls	bootstrap::tests::only_exit_zero_is_vendor_success
install-process-controls	determinate::tests::spawn_failure_is_reported_without_terminal_outcome
install-process-controls	determinate::tests::wait_failure_is_reported_after_one_vendor_start
install-process-controls	bootstrap::tests::nonzero_exit_preserves_started_and_refuses_retry
install-process-controls	bootstrap::tests::signal_preserves_started_and_refuses_retry
install-process-controls	bootstrap::tests::real_supervisor_loss_preserves_started_and_refuses_second_start
install-process-controls	bootstrap::tests::crash_before_vendor_start_preserves_started_and_refuses_retry
install-process-controls	bootstrap::tests::crash_after_exit_zero_before_acceptance_preserves_started
install-process-controls	bootstrap::tests::failed_installed_state_validation_preserves_started
install-process-controls	bootstrap::tests::exit_zero_plus_installed_state_validation_accepts_handoff_exactly_once
install-process-controls	bootstrap::tests::spawn_and_wait_uncertainty_preserves_started_and_refuses_retry
install-process-controls	bootstrap::tests::failed_product_receipt_publication_keeps_accepted_handoff
product-upgrade	bootstrap::tests::journaled_existing_product_update_stays_offline_and_never_starts_determinate
product-upgrade	bootstrap::tests::offline_state_change_blocks_the_next_file_mutation_and_rollback
product-upgrade	bootstrap::tests::failed_existing_product_update_restores_files_and_stays_offline
product-upgrade	linux_platform_assets::tests::ordinary_upgrade_requires_different_release_and_prior_content_identity
product-upgrade	linux_filesystem::tests::upgrade_replaces_only_exact_prior_owned_bytes_and_rolls_back
product-asset-repair	bootstrap::tests::journaled_offline_repair_changes_product_files_without_service_mutation
product-asset-repair	bootstrap::tests::journaled_repair_refuses_non_offline_service_state_before_mutation
product-asset-repair	bootstrap::tests::failed_offline_repair_rolls_forward_files_without_service_mutation
product-asset-repair	linux_systemd::tests::offline_preflight_is_query_only_and_refuses_every_non_offline_state
product-asset-repair	linux_platform_assets::tests::repair_requires_same_release_and_created_product_ownership
product-asset-repair	linux_platform_assets::tests::repair_requires_a_receipt_and_non_files_never_gain_implicit_ownership
product-asset-repair	linux_filesystem::tests::repair_roll_forward_replaces_unknown_binaries_and_changed_or_missing_units
EOF

echo "+ build clean host from staged artifacts only"
docker build \
    --platform "$docker_platform" \
    --file "$repo/tests/linux-clean-host/Dockerfile" \
    --tag "$image" \
    "$artifact_context"

wait_container_ready() {
    target_container=${1:-$container}
    ready=0
    attempt=0
    while [ "$attempt" -lt 60 ]; do
        if docker exec "$target_container" curl --fail --silent https://127.0.0.1:8443/root.json >/dev/null; then
            ready=1
            break
        fi
        attempt=$((attempt + 1))
        sleep 1
    done
    if [ "$ready" -ne 1 ]; then
        docker logs "$target_container" || true
        return 1
    fi
}

start_container() {
    echo "+ docker run --privileged --cgroupns=private"
    docker run \
        --detach \
        --privileged \
        --platform "$docker_platform" \
        --cgroupns=private \
        --name "$container" \
        --tmpfs /run \
        --tmpfs /run/lock \
        "$image" >/dev/null
    wait_container_ready
}

record_pass() {
    printf 'pass\t%s\t%s\t%s\n' "$1" "$2" "$3" >> "$results"
}

run_filter_group() {
    case_name=$1
    lifecycle_run=$2
    case_detail=${3-}
    log_directory="$evidence_root/test-logs/run-$lifecycle_run"
    mkdir -p "$log_directory"
    filter_index=0
    found=0
    tab=$(printf '\t')
    while IFS="$tab" read -r listed_case filter; do
        [ "$listed_case" = "$case_name" ] || continue
        found=1
        filter_index=$((filter_index + 1))
        log="$log_directory/$case_name-$filter_index.log"
        echo "+ exact test $filter"
        if docker exec "$container" /usr/local/libexec/pkg-installer-lib-tests \
            --exact "$filter" --nocapture > "$log" 2>&1; then
            cat "$log"
        else
            status=$?
            cat "$log" >&2
            return "$status"
        fi
        grep -F "test $filter ... ok" "$log" >/dev/null
        grep -F "test result: ok. 1 passed; 0 failed;" "$log" >/dev/null
    done < "$filters"
    test "$found" -eq 1
    filter_detail="$filter_index exact filters"
    if [ -n "$case_detail" ]; then
        filter_detail="$filter_detail; $case_detail"
    fi
    record_pass "$case_name" "$lifecycle_run" "$filter_detail"
}

inspect_test_binary() {
    lifecycle_run=$1
    inspection="$evidence_root/test-binary-run-$lifecycle_run.txt"
    docker exec "$container" sh -eu -c '
        actual=$(sha256sum /usr/local/libexec/pkg-installer-lib-tests)
        actual=${actual%% *}
        test "$actual" = "$1"
        printf "sha256: %s\n" "$actual"
        file_output=$(file /usr/local/libexec/pkg-installer-lib-tests)
        printf "%s\n" "$file_output"
        printf "%s\n" "$file_output" | grep -F "ELF 64-bit" >/dev/null
        printf "%s\n" "$file_output" | grep -F "x86-64" >/dev/null
        readelf_output=$(readelf --file-header /usr/local/libexec/pkg-installer-lib-tests)
        printf "%s\n" "$readelf_output"
        printf "%s\n" "$readelf_output" \
            | grep -Eq "^[[:space:]]*Machine:[[:space:]]+Advanced Micro Devices X86-64$"
        ldd_output=$(ldd /usr/local/libexec/pkg-installer-lib-tests)
        printf "%s\n" "$ldd_output"
        ! printf "%s\n" "$ldd_output" | grep -F "not found" >/dev/null
    ' sh "$test_binary_sha256" > "$inspection" 2>&1
    cat "$inspection"
}

snapshot_uninstall_state() {
    snapshot=$1
    docker exec -i "$container" python3 - > "$snapshot" <<'PY'
import hashlib
import json
import os
import stat
import subprocess
import sys

paths = [
    "/var/lib/pkg-install/determinate-handoff-v1.json",
    "/run/pkg-install-handoff.lock",
    "/nix/nix-installer",
    "/nix/receipt.json",
    "/opt/pkg/uninstall/manifest.json",
    "/opt/pkg/bin/pkg-root-helper",
    "/opt/pkg/bin/pkg-nix-broker",
    "/usr/local/bin/pkg",
    "/nix/var/nix/daemon-socket/socket",
    "/run/pkg-helper/root-helper.sock",
    "/run/pkg/broker.sock",
]
units = [
    "nix-daemon.service",
    "nix-daemon.socket",
    "pkg-root-helper.service",
    "pkg-root-helper.socket",
    "pkg-nix-broker.service",
    "pkg-nix-broker.socket",
]


def path_state(path):
    metadata = os.lstat(path)
    value = {
        "device": metadata.st_dev,
        "gid": metadata.st_gid,
        "inode": metadata.st_ino,
        "mode": format(stat.S_IMODE(metadata.st_mode), "04o"),
        "mtime_ns": metadata.st_mtime_ns,
        "nlink": metadata.st_nlink,
        "size": metadata.st_size,
        "type": stat.S_IFMT(metadata.st_mode),
        "uid": metadata.st_uid,
    }
    if stat.S_ISREG(metadata.st_mode):
        digest = hashlib.sha256()
        with open(path, "rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        value["sha256"] = digest.hexdigest()
    elif stat.S_ISLNK(metadata.st_mode):
        value["target"] = os.readlink(path)
    return value


state = {"paths": {path: path_state(path) for path in paths}, "units": {}}
for unit in units:
    result = subprocess.run(
        [
            "systemctl",
            "show",
            "--no-pager",
            "--property=ActiveState,SubState,UnitFileState,MainPID",
            unit,
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    state["units"][unit] = dict(
        line.split("=", 1) for line in result.stdout.splitlines() if "=" in line
    )
json.dump(state, sys.stdout, indent=2, sort_keys=True)
print()
PY
}

prove_structured_uninstall_refusal() {
    mode=$1
    lifecycle_run=$2
    directory="$evidence_root/structured-uninstall/run-$lifecycle_run"
    mkdir -p "$directory"
    before="$directory/$mode-before.json"
    after="$directory/$mode-after.json"
    stdout="$directory/$mode.stdout"
    stderr="$directory/$mode.stderr"
    expected="$directory/$mode.expected"
    snapshot_uninstall_state "$before"
    case "$mode" in
        json)
            flag=--json
            printf '%s\n' '{"schemaVersion":1,"ok":false,"command":"uninstall","error":{"symbol":"CONFIG","code":78,"message":"live uninstall requires plain output","hint":"remove --json or --jsonl, or use --dry-run"}}' > "$expected"
            ;;
        jsonl)
            flag=--jsonl
            printf '%s\n' '{"schemaVersion":1,"type":"result","ok":false,"command":"uninstall","error":{"symbol":"CONFIG","code":78,"message":"live uninstall requires plain output","hint":"remove --json or --jsonl, or use --dry-run"}}' > "$expected"
            ;;
        *) return 2 ;;
    esac
    set +e
    docker exec "$container" /usr/local/bin/pkg "$flag" --yes uninstall \
        > "$stdout" 2> "$stderr"
    status=$?
    set -e
    test "$status" -eq 78
    test ! -s "$stderr"
    cmp "$expected" "$stdout"
    snapshot_uninstall_state "$after"
    cmp "$before" "$after"
    record_pass "structured-$mode" "$lifecycle_run" "exit 78; exact CONFIG; zero mutation"
}

product_units='pkg-root-helper.socket pkg-nix-broker.socket pkg-root-helper.service pkg-nix-broker.service'

snapshot_product_boundary() {
    snapshot=$1
    docker exec -i "$container" python3 - > "$snapshot" <<'PY'
import hashlib
import json
import os
import stat
import subprocess
import sys

paths = [
    "/var/lib/pkg-install/determinate-handoff-v1.json",
    "/run/pkg-install/transaction-v1.json",
    "/nix/nix-installer",
    "/nix/receipt.json",
    "/opt/pkg/uninstall/manifest.json",
    "/opt/pkg/bin/pkg-root-helper",
    "/opt/pkg/bin/pkg-nix-broker",
    "/opt/pkg/etc/pkg/nix.conf",
    "/usr/lib/systemd/system/pkg-root-helper.socket",
    "/usr/lib/systemd/system/pkg-nix-broker.socket",
    "/usr/lib/systemd/system/pkg-root-helper.service",
    "/usr/lib/systemd/system/pkg-nix-broker.service",
    "/usr/local/bin/pkg",
]
units = [
    "pkg-root-helper.socket",
    "pkg-nix-broker.socket",
    "pkg-root-helper.service",
    "pkg-nix-broker.service",
]


def path_state(path):
    try:
        metadata = os.lstat(path)
    except FileNotFoundError:
        return None
    value = {
        "gid": metadata.st_gid,
        "mode": format(stat.S_IMODE(metadata.st_mode), "04o"),
        "size": metadata.st_size,
        "type": stat.S_IFMT(metadata.st_mode),
        "uid": metadata.st_uid,
    }
    if stat.S_ISREG(metadata.st_mode):
        value["sha256"] = hashlib.sha256(open(path, "rb").read()).hexdigest()
    elif stat.S_ISLNK(metadata.st_mode):
        value["target"] = os.readlink(path)
    return value


state = {"paths": {path: path_state(path) for path in paths}, "units": {}}
for unit in units:
    result = subprocess.run(
        ["systemctl", "show", "--no-pager", "--property=ActiveState,SubState,UnitFileState,MainPID", unit],
        check=True,
        capture_output=True,
        text=True,
    )
    state["units"][unit] = dict(
        line.split("=", 1) for line in result.stdout.splitlines() if "=" in line
    )
json.dump(state, sys.stdout, indent=2, sort_keys=True)
print()
PY
}

assert_product_units_offline() {
    for unit in $product_units; do
        docker exec "$container" systemctl is-active --quiet "$unit" && return 1
        test "$(docker exec "$container" systemctl is-enabled "$unit" 2>/dev/null || true)" = disabled
        test "$(docker exec "$container" systemctl show --property=MainPID --value "$unit")" = 0
    done
}

stop_disable_product_units() {
    docker exec "$container" systemctl stop $product_units
    docker exec "$container" systemctl disable $product_units
    assert_product_units_offline
}

activate_product_units() {
    assert_publication_product "$1"
    docker exec "$container" systemctl daemon-reload
    docker exec "$container" systemctl enable $product_units
    docker exec "$container" systemctl start $product_units
    for unit in $product_units; do
        docker exec "$container" systemctl is-active --quiet "$unit"
        test "$(docker exec "$container" systemctl is-enabled "$unit")" = enabled
    done
}

assert_publication_product() {
    publication=$1
    docker exec "$container" python3 - "$publication" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
manifest = json.loads((root / "release-manifest.json").read_text())
installed = {
    "installer/x86_64-linux/pkg-root-helper": pathlib.Path("/opt/pkg/bin/pkg-root-helper"),
    "installer/x86_64-linux/pkg-nix-broker": pathlib.Path("/opt/pkg/bin/pkg-nix-broker"),
    "installer/x86_64-linux/pkg": pathlib.Path("/usr/local/bin/pkg"),
}
expected = {
    item["target"]: item["sha256"]
    for item in manifest["artifacts"]
    if item["target"] in installed
}
if expected.keys() != installed.keys():
    raise SystemExit("publication product set is incomplete")
for target, path in installed.items():
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    if actual != expected[target]:
        raise SystemExit(f"installed product digest mismatch: {target}")

receipt = json.loads(pathlib.Path("/opt/pkg/uninstall/manifest.json").read_text())
descriptor = next(item for item in manifest["artifacts"] if item["kind"] == "descriptor")
if receipt["ownershipManifestDigest"] != descriptor["sha256"]:
    raise SystemExit("receipt release identity does not match the publication")
records = {item["id"]: item for item in receipt["assets"]}
expected_records = {
    "broker-group", "broker-user", "nix-root", "nix-gcroots", "product-root",
    "product-config-root", "product-config-dir", "uninstall-root", "service-bin-dir",
    "service-root", "helper-socket-dir", "broker-socket-dir", "log-root",
    "broker-log-dir", "helper-log-dir", "broker-home", "broker-channel-state",
    "helper-home", "helper-tmp", "broker-tmp", "root-helper-binary", "broker-binary",
    "nix-config", "helper-socket-unit", "helper-service-unit", "broker-socket-unit",
    "broker-service-unit", "runtime-tmpfiles", "profile-snippet", "product-cli",
    "uninstall-manifest",
}
if len(records) != len(receipt["assets"]) or records.keys() != expected_records:
    raise SystemExit("receipt product record set is not exact")
for asset, target in {
    "root-helper-binary": "installer/x86_64-linux/pkg-root-helper",
    "broker-binary": "installer/x86_64-linux/pkg-nix-broker",
    "product-cli": "installer/x86_64-linux/pkg",
}.items():
    if records[asset]["state"] != "created" or records[asset]["contentDigest"] != expected[target]:
        raise SystemExit(f"receipt product digest mismatch: {asset}")
file_paths = {
    "root-helper-binary": pathlib.Path("/opt/pkg/bin/pkg-root-helper"),
    "broker-binary": pathlib.Path("/opt/pkg/bin/pkg-nix-broker"),
    "nix-config": pathlib.Path("/opt/pkg/etc/pkg/nix.conf"),
    "helper-socket-unit": pathlib.Path("/usr/lib/systemd/system/pkg-root-helper.socket"),
    "helper-service-unit": pathlib.Path("/usr/lib/systemd/system/pkg-root-helper.service"),
    "broker-socket-unit": pathlib.Path("/usr/lib/systemd/system/pkg-nix-broker.socket"),
    "broker-service-unit": pathlib.Path("/usr/lib/systemd/system/pkg-nix-broker.service"),
    "runtime-tmpfiles": pathlib.Path("/usr/lib/tmpfiles.d/pkg.conf"),
    "profile-snippet": pathlib.Path("/etc/profile.d/pkg.sh"),
    "product-cli": pathlib.Path("/usr/local/bin/pkg"),
}
for asset, path in file_paths.items():
    actual = hashlib.sha256(path.read_bytes()).hexdigest()
    if records[asset].get("contentDigest") != actual:
        raise SystemExit(f"receipt file digest mismatch: {asset}")
for asset in expected_records - file_paths.keys() - {"uninstall-manifest"}:
    if records[asset].get("contentDigest") is not None:
        raise SystemExit(f"non-file receipt digest is present: {asset}")
if records["uninstall-manifest"]["state"] != "created" or records["uninstall-manifest"].get("contentDigest") is not None:
    raise SystemExit("receipt ownership record is invalid")
PY
}

snapshot_package_state() {
    snapshot=$1
    docker exec -i "$container" python3 - > "$snapshot" <<'PY'
import hashlib
import os
import pathlib
import stat

state_root = pathlib.Path("/home/proof-user/.local/share/pkg")
gc_root = pathlib.Path("/nix/var/nix/gcroots/pkg")


def entry(path, relative):
    metadata = path.lstat()
    fields = [relative, format(stat.S_IMODE(metadata.st_mode), "04o"), str(metadata.st_uid), str(metadata.st_gid)]
    if stat.S_ISDIR(metadata.st_mode):
        fields.insert(0, "directory")
    elif stat.S_ISLNK(metadata.st_mode):
        fields[:0] = ["symlink"]
        fields.append(os.readlink(path))
    elif stat.S_ISREG(metadata.st_mode):
        fields[:0] = ["file"]
        fields.extend([str(metadata.st_size), hashlib.sha256(path.read_bytes()).hexdigest()])
    else:
        raise SystemExit(f"unexpected package-state object: {path}")
    return "\t".join(fields)


if not state_root.is_dir() or not gc_root.is_dir():
    raise SystemExit("package state or GC-root directory is absent")
print(entry(state_root, "."))
for path in sorted(state_root.rglob("*"), key=lambda item: os.fsencode(str(item.relative_to(state_root)))):
    print(entry(path, str(path.relative_to(state_root))))
roots = []
for path in sorted(gc_root.rglob("*"), key=lambda item: os.fsencode(str(item.relative_to(gc_root)))):
    if path.is_symlink():
        roots.append((str(path.relative_to(gc_root)), os.readlink(path), str(path.resolve(strict=True))))
if not roots:
    raise SystemExit("package GC roots are absent")
for name, target, resolved in roots:
    print("gc-root\t" + "\t".join((name, target, resolved)))
PY
}

publication_installer() {
    docker exec "$container" python3 - "$1" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
path = root / "v0.1.0-alpha.7/pkg-installer-x86_64-linux"
manifest = json.loads((root / "release-manifest.json").read_text())
record = next(
    item for item in manifest["cliArtifacts"]
    if item["kind"] == "pkg-install" and item["system"] == "x86_64-linux"
)
if hashlib.sha256(path.read_bytes()).hexdigest() != record["sha256"]:
    raise SystemExit("publication installer digest mismatch")
print(path)
PY
}

shipping_installer=/srv/pkg-release/v0.1.0-alpha.7/pkg-installer-x86_64-linux

echo "+ foreign Nix refusal before mutation"
start_container
docker exec "$container" sh -eu -c 'mkdir /nix; printf "foreign\n" > /nix/foreign'
if foreign_output=$(docker exec "$container" "$shipping_installer" 2>&1); then
    echo "Foreign Nix was accepted." >&2
    exit 1
fi
test "$foreign_output" = "pkg installation failed."
docker exec "$container" sh -eu -c '
    grep -Fx foreign /nix/foreign
    test ! -e /opt/pkg
    test ! -e /var/lib/pkg
    test ! -e /var/lib/pkg-install
    ! getent passwd pkg-nix-broker
    ! getent group pkg-nix-broker
    ! getent group nixbld
'
stop_container

echo "+ authenticated ownership drift refusal"
start_container
docker exec "$container" /usr/local/sbin/pkg-bootstrap
docker exec "$container" chmod 0777 /opt/pkg/bin/pkg-nix-broker
if drift_output=$(docker exec "$container" "$shipping_installer" 2>&1); then
    echo "Ownership drift was accepted." >&2
    exit 1
fi
test "$drift_output" = "pkg installation failed."
test "$(docker exec "$container" stat -c %a /opt/pkg/bin/pkg-nix-broker)" = 777
stop_container

prove_lifecycle() {
echo "+ lifecycle run $1 of 2"
start_container

echo "+ verify clean host"
docker exec "$container" sh -eu -c '
    ! command -v nix
    test ! -e /nix
    test ! -e /opt/pkg
'

echo "+ bootstrap verify-only"
docker exec "$container" /usr/local/sbin/pkg-bootstrap --verify-only

echo "+ bootstrap install"
docker exec "$container" /usr/local/sbin/pkg-bootstrap

echo "+ bootstrap retry"
docker exec "$container" /usr/local/sbin/pkg-bootstrap

echo "+ verify vendor Nix, product services, and ordinary-user isolation"
docker exec "$container" sh -eu -c '
    python3 -c '\''
import json, sys
record = json.load(open("/var/lib/pkg-install/determinate-handoff-v1.json"))
sys.exit(record.get("schema_version") != 1 or record.get("state", {}).get("kind") != "accepted")
'\''
    test -f /nix/nix-installer
    test ! -L /nix/nix-installer
    test "$(stat -c %u:%g:%a /nix/nix-installer)" = 0:0:755
    test "$(stat -c %s /nix/nix-installer)" = 74918096
    printf "%s  %s\n" \
        9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c \
        /nix/nix-installer \
        | sha256sum --check --strict
    test -f /nix/receipt.json
    test ! -L /nix/receipt.json
    test "$(stat -c %u:%g:%a /nix/receipt.json)" = 0:0:600
    receipt_size=$(stat -c %s /nix/receipt.json)
    test "$receipt_size" -gt 0
    test "$receipt_size" -le 1048576
    stat -c "vendor receipt: owner=%u:%g mode=%a bytes=%s" /nix/receipt.json
    systemctl is-active --quiet nix-daemon.service
    systemctl is-active --quiet nix-daemon.socket
    /nix/var/nix/profiles/default/bin/nix --version \
        | grep -F "nix (Determinate Nix 3.22.1) 2.35.2"
    /nix/var/nix/profiles/default/bin/nix store ping --store daemon
    systemctl is-active --quiet pkg-root-helper.socket
    systemctl is-active --quiet pkg-nix-broker.socket
    test "$(/usr/local/bin/pkg --version)" = "pkg 0.1.0-alpha.7"
    test ! -e /opt/pkg/nix
    test ! -L /opt/pkg/nix
    ! grep -R -F /opt/pkg/nix /etc/systemd/system/pkg-* >/dev/null 2>&1
    ! su -s /bin/sh proof-user -c "command -v nix"
    ! su -s /bin/sh proof-user -c "/opt/pkg/bin/pkg-root-helper"
    ! su -s /bin/sh proof-user -c "/opt/pkg/bin/pkg-nix-broker"
    su -s /bin/sh proof-user -c "test -w /run/pkg/broker.sock"
    ! su -s /bin/sh proof-user -c "test -w /run/pkg-helper/root-helper.sock"
    ! su -s /bin/sh proof-user -c "test -r /opt/pkg/etc/pkg/nix.conf"
'
record_pass old-runtime-absent "$1" "no /opt/pkg/nix tree or service reference"

echo "+ inspect the proof-only test executable"
inspect_test_binary "$1"

for blocking_case in \
    persisted-started-refusal \
    sync-exec-restore \
    sync-exec-restore-failure \
    post-unlink-clear-restore \
    real-sigkill-unmarked \
    later-outcome-unknown \
    vendor-action-last \
    install-process-controls
do
    run_filter_group "$blocking_case" "$1"
done

echo "+ pkg install hello"
docker exec "$container" su - proof-user -c "/usr/local/bin/pkg --yes install hello"
docker exec "$container" su - proof-user -c "/usr/local/bin/pkg --json list" \
    | grep -F '"name":"hello"' >/dev/null
docker exec "$container" su - proof-user -c \
    "/home/proof-user/.local/share/pkg/current/bin/hello" \
    | grep -F "Hello, world!" >/dev/null

echo "+ pkg install ripgrep"
docker exec "$container" su - proof-user -c "/usr/local/bin/pkg --yes install ripgrep"
docker exec "$container" su - proof-user -c \
    "/home/proof-user/.local/share/pkg/current/bin/rg --version" \
    | grep -F "ripgrep 13.0.0" >/dev/null

for package in fd bat tree wget git tmux zoxide fzf; do
    echo "+ pkg install $package"
    docker exec "$container" su - proof-user -c \
        "/usr/local/bin/pkg --yes install $package"
done
package_list=$(docker exec "$container" su - proof-user -c "/usr/local/bin/pkg --json list")
for package in hello ripgrep fd bat tree wget git tmux zoxide fzf; do
    printf '%s\n' "$package_list" | grep -F "\"name\":\"$package\"" >/dev/null
done

echo "+ pkg install cxx-prettyprint with approved local build"
if ! local_build_output=$(docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --yes --jsonl install cxx-prettyprint"); then
    printf '%s\n' "$local_build_output" >&2
    exit 1
fi
printf '%s\n' "$local_build_output" | grep -F '"type":"build_started"' >/dev/null
printf '%s\n' "$local_build_output" | grep -F '"selector":"cxx-prettyprint"' >/dev/null

echo "+ publish channel sequence 2"
product_evidence="$evidence_root/product-lifecycle/run-$1"
mkdir -p "$product_evidence"
assert_publication_product /srv/pkg-releases/1
snapshot_product_boundary "$product_evidence/n-before.json"
snapshot_package_state "$product_evidence/package-state-before.txt"
product_n_broker=$(docker exec "$container" sha256sum /opt/pkg/bin/pkg-nix-broker | awk '{print $1}')
product_n_receipt=$(docker exec "$container" sha256sum /opt/pkg/uninstall/manifest.json | awk '{print $1}')
base_installer=$(docker exec "$container" sha256sum /nix/nix-installer | awk '{print $1}')
base_receipt=$(docker exec "$container" sha256sum /nix/receipt.json | awk '{print $1}')
base_handoff=$(docker exec "$container" sha256sum /var/lib/pkg-install/determinate-handoff-v1.json | awk '{print $1}')
old_broker_pid=$(docker exec "$container" systemctl show --property=MainPID --value pkg-nix-broker.service)
test "$old_broker_pid" -gt 0
stop_disable_product_units
docker exec "$container" sh -eu -c '
    ln -s /srv/pkg-releases/2 /srv/pkg-release.next
    mv -Tf /srv/pkg-release.next /srv/pkg-release
'

echo "+ authenticated offline product upgrade"
n_plus_1_installer=$(publication_installer /srv/pkg-releases/2)
upgrade_output=$(docker exec "$container" "$n_plus_1_installer")
test "$upgrade_output" = "pkg product files are upgraded. Product services remain offline."
assert_product_units_offline
assert_publication_product /srv/pkg-releases/2
snapshot_product_boundary "$product_evidence/n-plus-1-offline.json"
snapshot_package_state "$product_evidence/package-state-after-upgrade.txt"
cmp "$product_evidence/package-state-before.txt" \
    "$product_evidence/package-state-after-upgrade.txt"
product_n_plus_1_broker=$(docker exec "$container" sha256sum /opt/pkg/bin/pkg-nix-broker | awk '{print $1}')
product_n_plus_1_receipt=$(docker exec "$container" sha256sum /opt/pkg/uninstall/manifest.json | awk '{print $1}')
test "$product_n_broker" != "$product_n_plus_1_broker"
test "$product_n_receipt" != "$product_n_plus_1_receipt"
test "$(docker exec "$container" sha256sum /nix/nix-installer | awk '{print $1}')" = "$base_installer"
test "$(docker exec "$container" sha256sum /nix/receipt.json | awk '{print $1}')" = "$base_receipt"
test "$(docker exec "$container" sha256sum /var/lib/pkg-install/determinate-handoff-v1.json | awk '{print $1}')" = "$base_handoff"
docker exec "$container" sh -eu -c '
    test ! -e /run/pkg-install/transaction-v1.json
    python3 -c '\''
import json, sys
record = json.load(open("/var/lib/pkg-install/determinate-handoff-v1.json"))
sys.exit(record.get("state", {}).get("kind") != "accepted")
'\''
    test -d /nix/var/nix/gcroots/pkg
    test "$(find /nix/var/nix/gcroots/pkg -type l | wc -l)" -gt 0
'
echo "+ activate verified N+1 product services"
activate_product_units /srv/pkg-releases/2
docker exec "$container" su - proof-user -c "/usr/local/bin/pkg --json list" \
    | grep -F '"name":"hello"' >/dev/null
docker exec "$container" su - proof-user -c \
    "/home/proof-user/.local/share/pkg/current/bin/hello" \
    | grep -F "Hello, world!" >/dev/null
new_broker_pid=$(docker exec "$container" systemctl show --property=MainPID --value pkg-nix-broker.service)
test "$new_broker_pid" -gt 0
test "$new_broker_pid" != "$old_broker_pid"
test "$(docker exec "$container" /usr/local/bin/pkg --version)" = "pkg 0.1.0-alpha.7"
run_filter_group product-upgrade "$1" "native N to N+1; Base Nix unchanged; verified activation"

echo "+ active product repair refusal without mutation"
snapshot_product_boundary "$product_evidence/repair-active-before.json"
set +e
docker exec "$container" "$n_plus_1_installer" --repair-product-assets \
    > "$product_evidence/repair-active.stdout" \
    2> "$product_evidence/repair-active.stderr"
repair_active_status=$?
set -e
test "$repair_active_status" -eq 1
test ! -s "$product_evidence/repair-active.stdout"
grep -Fx "Stop and disable all pkg product services. Remove all product unit drop-ins. Then run pkg-install again." \
    "$product_evidence/repair-active.stderr" >/dev/null
snapshot_product_boundary "$product_evidence/repair-active-after.json"
cmp "$product_evidence/repair-active-before.json" "$product_evidence/repair-active-after.json"
snapshot_package_state "$product_evidence/package-state-after-active-repair-refusal.txt"
cmp "$product_evidence/package-state-before.txt" \
    "$product_evidence/package-state-after-active-repair-refusal.txt"

echo "+ authenticated offline product asset repair"
stop_disable_product_units
repair_receipt=$(docker exec "$container" sha256sum /opt/pkg/uninstall/manifest.json | awk '{print $1}')
repair_pkg=$(docker exec "$container" sha256sum /usr/local/bin/pkg | awk '{print $1}')
repair_service=$(docker exec "$container" sha256sum /usr/lib/systemd/system/pkg-nix-broker.service | awk '{print $1}')
repair_base_installer=$(docker exec "$container" sha256sum /nix/nix-installer | awk '{print $1}')
repair_base_receipt=$(docker exec "$container" sha256sum /nix/receipt.json | awk '{print $1}')
repair_handoff=$(docker exec "$container" sha256sum /var/lib/pkg-install/determinate-handoff-v1.json | awk '{print $1}')
docker exec "$container" sh -eu -c '
    printf "damaged product cli\n" > /usr/local/bin/pkg
    chmod 0755 /usr/local/bin/pkg
    printf "damaged broker service\n" > /usr/lib/systemd/system/pkg-nix-broker.service
    chmod 0644 /usr/lib/systemd/system/pkg-nix-broker.service
'
test "$(docker exec "$container" sha256sum /usr/local/bin/pkg | awk '{print $1}')" != "$repair_pkg"
test "$(docker exec "$container" sha256sum /usr/lib/systemd/system/pkg-nix-broker.service | awk '{print $1}')" != "$repair_service"
repair_output=$(docker exec "$container" "$n_plus_1_installer" --repair-product-assets)
test "$repair_output" = "pkg product files are repaired. Product services remain offline."
assert_product_units_offline
assert_publication_product /srv/pkg-releases/2
test "$(docker exec "$container" sha256sum /usr/local/bin/pkg | awk '{print $1}')" = "$repair_pkg"
test "$(docker exec "$container" sha256sum /usr/lib/systemd/system/pkg-nix-broker.service | awk '{print $1}')" = "$repair_service"
test "$(docker exec "$container" sha256sum /opt/pkg/uninstall/manifest.json | awk '{print $1}')" = "$repair_receipt"
test "$(docker exec "$container" sha256sum /nix/nix-installer | awk '{print $1}')" = "$repair_base_installer"
test "$(docker exec "$container" sha256sum /nix/receipt.json | awk '{print $1}')" = "$repair_base_receipt"
test "$(docker exec "$container" sha256sum /var/lib/pkg-install/determinate-handoff-v1.json | awk '{print $1}')" = "$repair_handoff"
docker exec "$container" sh -eu -c '
    test ! -e /run/pkg-install/transaction-v1.json
    test -d /nix/var/nix/gcroots/pkg
    test "$(find /nix/var/nix/gcroots/pkg -type l | wc -l)" -gt 0
'
snapshot_product_boundary "$product_evidence/repair-offline-after.json"
snapshot_package_state "$product_evidence/package-state-after-repair.txt"
cmp "$product_evidence/package-state-before.txt" \
    "$product_evidence/package-state-after-repair.txt"

echo "+ activate verified repaired N+1 product services"
activate_product_units /srv/pkg-releases/2
docker exec "$container" su - proof-user -c "/usr/local/bin/pkg --json list" \
    | grep -F '"name":"hello"' >/dev/null
docker exec "$container" su - proof-user -c \
    "/home/proof-user/.local/share/pkg/current/bin/hello" \
    | grep -F "Hello, world!" >/dev/null
run_filter_group product-asset-repair "$1" "native pkg-nix-broker.service repair; exact snapshots; verified activation"

echo "+ pkg update"
channel_output=$(docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --json update")
printf '%s\n' "$channel_output" | grep -F '"channelSequence":2' >/dev/null
printf '%s\n' "$channel_output" | grep -F '"updated":true' >/dev/null
printf '%s\n' "$channel_output" | grep -F '"stateUpdated":true' >/dev/null

echo "+ pkg upgrade ripgrep"
if ! upgrade_output=$(docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --yes --json upgrade ripgrep --no-build" 2>&1); then
    printf '%s\n' "$upgrade_output" >&2
    exit 1
fi
printf '%s\n' "$upgrade_output" | grep -F '"upgraded":["ripgrep"]' >/dev/null
docker exec "$container" su - proof-user -c \
    "/home/proof-user/.local/share/pkg/current/bin/rg --version" \
    | grep -F "ripgrep 15.1.0" >/dev/null

echo "+ pkg rollback"
rollback_output=$(docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --json rollback")
printf '%s\n' "$rollback_output" | grep -F '"sourceGeneration"' >/dev/null
printf '%s\n' "$rollback_output" | grep -F '"targetGeneration"' >/dev/null
docker exec "$container" su - proof-user -c \
    "/home/proof-user/.local/share/pkg/current/bin/rg --version" \
    | grep -F "ripgrep 13.0.0" >/dev/null

echo "+ damage and repair the cached hello package"
docker exec "$container" sh -eu -c '
    hello_path=$(readlink -f /home/proof-user/.local/share/pkg/current/bin/hello)
    case "$hello_path" in
        /nix/store/*/bin/hello) ;;
        *) exit 1 ;;
    esac
    chmod u+w "$hello_path"
    printf "damaged\n" > "$hello_path"
    chmod a-w "$hello_path"
'
if verify_output=$(docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --json repair --verify-only" 2>&1); then
    echo "Repair verification did not detect the damaged package." >&2
    exit 1
fi
printf '%s\n' "$verify_output" | grep -F '"symbol":"VERIFY_FAIL"' >/dev/null
printf '%s\n' "$verify_output" | grep -F '"code":70' >/dev/null
repair_output=$(docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --yes --json repair")
printf '%s\n' "$repair_output" | grep -F '"status":"repaired-from-cache"' >/dev/null
docker exec "$container" su - proof-user -c \
    "/home/proof-user/.local/share/pkg/current/bin/hello" \
    | grep -F "Hello, world!" >/dev/null
record_pass package-repair "$1" "live cache repair"

echo "+ prove package roots and explicit GC"
current_before_gc=$(docker exec "$container" readlink -f \
    /home/proof-user/.local/share/pkg/current)
roots_evidence="$evidence_root/package-roots-gc-run-$1.txt"
docker exec "$container" sh -eu -c '
    test -d /nix/var/nix/gcroots/pkg
    roots=$(find /nix/var/nix/gcroots/pkg -type l -print | sort)
    test -n "$roots"
    printf "before GC roots:\n%s\n" "$roots"
' > "$roots_evidence"
test -s "$roots_evidence"
gc_output=$(docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --yes --json gc --keep-generations 1 --max-age-days 0")
printf 'gc: %s\n' "$gc_output" >> "$roots_evidence"
printf '%s\n' "$gc_output" | python3 -c '
import json
import sys

result = json.load(sys.stdin)
assert isinstance(result.get("prunedGenerations"), list)
assert result["prunedGenerations"]
assert isinstance(result.get("collectedPathCount"), int)
'
docker exec "$container" sh -eu -c '
    test -d /nix/var/nix/gcroots/pkg
    roots=$(find /nix/var/nix/gcroots/pkg -type l -print)
    test -n "$roots"
    printf "after GC roots:\n%s\n" "$roots"
    for root in $roots; do
        target=$(readlink -f "$root")
        test -e "$target"
        case "$target" in /nix/store/*) ;; *) exit 1 ;; esac
    done
' >> "$roots_evidence"
test "$(docker exec "$container" readlink -f \
    /home/proof-user/.local/share/pkg/current)" = "$current_before_gc"
docker exec "$container" su - proof-user -c \
    "/home/proof-user/.local/share/pkg/current/bin/hello" \
    | grep -F "Hello, world!" >/dev/null
cat "$roots_evidence"
record_pass package-roots-gc "$1" "owned roots survive explicit generation prune and store GC"

echo "+ pkg remove all installed packages"
docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --yes remove hello ripgrep fd bat tree wget git tmux zoxide fzf cxx-prettyprint"
docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --json list" \
    | grep -F '"entries":[]' >/dev/null
record_pass package-operations "$1" "live install, update, upgrade, rollback, remove"

echo "+ refuse live JSON uninstall without mutation"
prove_structured_uninstall_refusal json "$1"

echo "+ refuse live JSONL uninstall without mutation"
prove_structured_uninstall_refusal jsonl "$1"

echo "+ verify terminal-exec uninstall inputs"
docker exec "$container" sh -eu -c '
    python3 -c '\''
import json, sys
record = json.load(open("/var/lib/pkg-install/determinate-handoff-v1.json"))
sys.exit(record.get("schema_version") != 1 or record.get("state", {}).get("kind") != "accepted")
'\''
    test -f /nix/nix-installer
    test ! -L /nix/nix-installer
    test "$(stat -c %u:%g:%a /nix/nix-installer)" = 0:0:755
    test "$(stat -c %s /nix/nix-installer)" = 74918096
    printf "%s  %s\n" \
        9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c \
        /nix/nix-installer \
        | sha256sum --check --strict
    test -f /nix/receipt.json
    test ! -L /nix/receipt.json
    test "$(stat -c %u:%g:%a /nix/receipt.json)" = 0:0:600
    receipt_size=$(stat -c %s /nix/receipt.json)
    test "$receipt_size" -gt 0
    test "$receipt_size" -le 1048576
'

echo "+ pkg terminal-exec uninstall"
set +e
docker exec "$container" /usr/local/bin/pkg --yes uninstall
uninstall_status=$?
set -e
if [ "$uninstall_status" -ne 0 ]; then
    echo "Vendor uninstall failed with status $uninstall_status." >&2
    exit "$uninstall_status"
fi

echo "+ verify final product absence and vendor success postconditions"
docker exec "$container" sh -eu -c '
    ! systemctl is-active --quiet nix-daemon.service
    ! systemctl is-active --quiet nix-daemon.socket
    ! /nix/var/nix/profiles/default/bin/nix store ping --store daemon
    test ! -e /usr/local/bin/pkg
    test ! -e /opt/pkg
    test ! -e /var/lib/pkg
    test ! -e /var/lib/pkg-install
    test ! -e /run/pkg
    test ! -e /run/pkg-helper
    test ! -e /home/proof-user/.local/share/pkg
    test ! -e /nix/var/nix/gcroots/pkg
    test ! -L /nix/var/nix/gcroots/pkg
    ! getent passwd pkg-nix-broker
    ! getent group pkg-nix-broker
    ! systemctl list-unit-files --no-legend \
        | grep -Eq "^pkg-(root-helper|nix-broker)\\.(service|socket)"
'
record_pass terminal-uninstall "$1" "plain exec status and postconditions"

echo "+ record vendor-owned uninstall residue"
residue_report="$evidence_root/vendor-residue-run-$1.txt"
docker exec "$container" sh -eu -c '
    if test -e /nix; then
        find /nix -xdev -maxdepth 3 -printf "vendor residue: %M %u:%g %p\n" | sort
    fi
    if test -e /etc/nix; then
        find /etc/nix -xdev -printf "vendor residue: %M %u:%g %p\n" | sort
    fi
    getent passwd | awk -F: '\''$1 ~ /^nixbld/ { print "vendor residue: user=" $1 }'\''
    getent group | awk -F: '\''$1 == "nixbld" { print "vendor residue: group=" $1 }'\''
    systemctl list-unit-files --no-legend \
        | awk '\''$1 ~ /(nix|determinate)/ { print "vendor residue: unit=" $1 " state=" $2 }'\''
' > "$residue_report"
cat "$residue_report"
stop_container
}

for lifecycle_run in 1 2; do
    prove_lifecycle "$lifecycle_run"
done

for blocking_case in \
    persisted-started-refusal \
    structured-json \
    structured-jsonl \
    sync-exec-restore \
    sync-exec-restore-failure \
    post-unlink-clear-restore \
    real-sigkill-unmarked \
    later-outcome-unknown \
    vendor-action-last \
    install-process-controls \
    product-upgrade \
    product-asset-repair \
    package-operations \
    package-repair \
    package-roots-gc \
    old-runtime-absent \
    terminal-uninstall
do
    test "$(awk -F '\t' -v case_name="$blocking_case" \
        '$1 == "pass" && $2 == case_name { count += 1 } END { print count + 0 }' \
        "$results")" -eq 2
    for lifecycle_run in 1 2; do
        test "$(awk -F '\t' -v case_name="$blocking_case" -v run="$lifecycle_run" \
            '$1 == "pass" && $2 == case_name && $3 == run { count += 1 } END { print count + 0 }' \
            "$results")" -eq 1
    done
done
test "$(awk -F '\t' '$1 == "pass" { count += 1 } END { print count + 0 }' "$results")" -eq 34

echo "Linux vendor install/uninstall and product package lifecycle proof passed."
echo "Docker limits: no host boot or reboot, SELinux, foreign-host coexistence, or full distribution matrix."
