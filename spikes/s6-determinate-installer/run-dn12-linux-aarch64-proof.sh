#!/usr/bin/env bash
set -euo pipefail

readonly LANE='--disposable-linux-aarch64-container'
readonly BASE_IMAGE='ubuntu@sha256:33ceb71981b602c1a7443a53469e4dba065f7503eab3078a2d7a57a2ab987517'
readonly INSTALLER_SHA256='9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179'
readonly DAEMON_321_SHA256='a808c0cb3a6216ba167c873c8866806114253bcaa90a5cd52eef4b384c27febc'
readonly DAEMON_322_SHA256='39876af59651a7c3ec3037c8ef796f3bbbe4855d418b9ef5f98202244620428f'
readonly DAEMON_321_URL='https://install.determinate.systems/determinate-nixd/tag/v3.22.1/aarch64-linux'
readonly DAEMON_322_URL='https://install.determinate.systems/determinate-nixd/tag/v3.22.2/aarch64-linux'
readonly SYSTEMD_VERSION='255.4-1ubuntu8.17'
readonly PREP_CONTAINER_PREFIX='pkg-dn12-systemd-prep'
readonly PROOF_CONTAINER_PREFIX='pkg-dn12-upgrade-proof'
readonly PROOF_IMAGE_PREFIX='pkg-dn12-systemd-proof'

usage() {
  printf '%s\n' \
    'Refusing host execution.' \
    "Usage: $0 $LANE /absolute/pinned-installer /absolute/evidence.txt /absolute/cleanup.txt"
  exit 64
}

[[ $# -eq 4 && $1 == "$LANE" ]] || usage
readonly INSTALLER=$2
readonly EVIDENCE=$3
readonly CLEANUP=$4

[[ $INSTALLER == /* && -f $INSTALLER && ! -L $INSTALLER ]] || usage
[[ $EVIDENCE == /* && $CLEANUP == /* ]] || usage
[[ $EVIDENCE != "$CLEANUP" ]] || usage
for host_path in "$INSTALLER" "$EVIDENCE" "$CLEANUP"; do
  [[ $host_path != /nix && $host_path != /nix/* ]] || usage
done
[[ ! -e $EVIDENCE && ! -L $EVIDENCE && ! -e $CLEANUP && ! -L $CLEANUP ]] || {
  printf '%s\n' 'Refusing to overwrite proof output.' >&2
  exit 65
}

WORK=$(mktemp -d /private/tmp/pkg-dn12-proof.XXXXXX)
readonly WORK
[[ -d $WORK && $WORK == /private/tmp/pkg-dn12-proof.* ]] || exit 72
RUN_TOKEN=$(printf '%s' "$WORK" | shasum -a 256 | awk '{print substr($1, 1, 12)}')
readonly RUN_TOKEN
[[ $RUN_TOKEN =~ ^[0-9a-f]{12}$ ]] || exit 72
readonly PREP_CONTAINER="$PREP_CONTAINER_PREFIX-$RUN_TOKEN"
readonly PROOF_CONTAINER="$PROOF_CONTAINER_PREFIX-$RUN_TOKEN"
readonly PROOF_IMAGE="$PROOF_IMAGE_PREFIX-$RUN_TOKEN:temp"

PREP_CONTAINER_ID=''
PROOF_CONTAINER_ID=''
PROOF_IMAGE_ID=''

cleanup_temp() {
  find "$WORK" -depth -delete 2>/dev/null || true
}

container_name_exists() {
  docker container inspect "$1" >/dev/null 2>&1
}

container_id_exists() {
  docker container inspect "$1" >/dev/null 2>&1
}

image_name_exists() {
  docker image inspect "$1" >/dev/null 2>&1
}

image_id_exists() {
  docker image inspect "$1" >/dev/null 2>&1
}

cleanup_owned() {
  set +e
  for owned_container_id in "$PROOF_CONTAINER_ID" "$PREP_CONTAINER_ID"; do
    if [[ -n $owned_container_id ]] && container_id_exists "$owned_container_id"; then
      docker stop --time 10 "$owned_container_id" >/dev/null 2>&1
      docker rm "$owned_container_id" >/dev/null 2>&1
    fi
  done
  if [[ -n $PROOF_IMAGE_ID ]] && image_id_exists "$PROOF_IMAGE_ID"; then
    docker image rm "$PROOF_IMAGE_ID" >/dev/null 2>&1
  fi
  set -e
}

write_cleanup() {
  local harness_status=$1
  local evidence_hash='absent'
  if [[ -f $EVIDENCE ]]; then
    evidence_hash=$(shasum -a 256 "$EVIDENCE" | awk '{print $1}')
  fi
  {
    printf '%s\n' \
      'dn12_cleanup_version=1' \
      "harness_exit_status=$harness_status" \
      "container_id=$PROOF_CONTAINER_ID absent=$(container_id_exists "$PROOF_CONTAINER_ID" && printf false || printf true)" \
      "container_id=$PREP_CONTAINER_ID absent=$(container_id_exists "$PREP_CONTAINER_ID" && printf false || printf true)" \
      "image_id=$PROOF_IMAGE_ID absent=$(image_id_exists "$PROOF_IMAGE_ID" && printf false || printf true)" \
      'host_mounts_created=false' \
      'host_nix_used=false' \
      "harness_sha256=$(shasum -a 256 "$0" | awk '{print $1}')" \
      "evidence_sha256=$evidence_hash"
  } >"$CLEANUP"
}

cleanup_on_exit() {
  local harness_status=$?
  set +e
  cleanup_owned
  cleanup_temp
  if [[ ! -e $CLEANUP ]]; then
    write_cleanup "$harness_status"
  fi
  return "$harness_status"
}
trap cleanup_temp EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

container_name_exists "$PREP_CONTAINER" && {
  printf '%s\n' "Refusing existing container: $PREP_CONTAINER" >&2
  exit 66
}
container_name_exists "$PROOF_CONTAINER" && {
  printf '%s\n' "Refusing existing container: $PROOF_CONTAINER" >&2
  exit 66
}
image_name_exists "$PROOF_IMAGE" && {
  printf '%s\n' "Refusing existing image: $PROOF_IMAGE" >&2
  exit 66
}

[[ $(docker info --format '{{.OSType}}/{{.Architecture}}') == 'linux/aarch64' ]] || {
  printf '%s\n' 'This proof requires a native Linux aarch64 Docker server.' >&2
  exit 67
}

actual_installer=$(shasum -a 256 "$INSTALLER" | awk '{print $1}')
[[ $actual_installer == "$INSTALLER_SHA256" ]] || {
  printf '%s\n' 'The installer digest does not match the fixed proof input.' >&2
  exit 68
}

curl --proto '=https' --tlsv1.2 -fsSL -o "$WORK/determinate-nixd-3.22.1" "$DAEMON_321_URL"
curl --proto '=https' --tlsv1.2 -fsSL -o "$WORK/determinate-nixd-3.22.2" "$DAEMON_322_URL"
[[ $(shasum -a 256 "$WORK/determinate-nixd-3.22.1" | awk '{print $1}') == "$DAEMON_321_SHA256" ]]
[[ $(shasum -a 256 "$WORK/determinate-nixd-3.22.2" | awk '{print $1}') == "$DAEMON_322_SHA256" ]]

created_prep_id=$(docker create --name "$PREP_CONTAINER" --platform linux/arm64 "$BASE_IMAGE" sleep infinity)
[[ $created_prep_id =~ ^[0-9a-f]{64}$ ]]
[[ $(docker container inspect --format '{{.Id}}' "$created_prep_id") == "$created_prep_id" ]]
PREP_CONTAINER_ID=$created_prep_id
trap cleanup_on_exit EXIT
docker start "$PREP_CONTAINER_ID" >/dev/null
docker exec "$PREP_CONTAINER_ID" sh -c \
  "apt-get update >/tmp/apt-update.log && DEBIAN_FRONTEND=noninteractive apt-get install -y systemd=$SYSTEMD_VERSION systemd-sysv=$SYSTEMD_VERSION >/tmp/apt-install.log"
created_image_id=$(docker commit "$PREP_CONTAINER_ID" "$PROOF_IMAGE")
[[ $created_image_id =~ ^sha256:[0-9a-f]{64}$ ]]
[[ $(docker image inspect --format '{{.Id}}' "$created_image_id") == "$created_image_id" ]]
PROOF_IMAGE_ID=$created_image_id
docker stop --time 10 "$PREP_CONTAINER_ID" >/dev/null
docker rm "$PREP_CONTAINER_ID" >/dev/null

created_proof_id=$(docker create \
  --name "$PROOF_CONTAINER" \
  --platform linux/arm64 \
  --privileged \
  --cgroupns=private \
  --tmpfs /run \
  --tmpfs /run/lock \
  "$PROOF_IMAGE_ID" /sbin/init)
[[ $created_proof_id =~ ^[0-9a-f]{64}$ ]]
[[ $(docker container inspect --format '{{.Id}}' "$created_proof_id") == "$created_proof_id" ]]
PROOF_CONTAINER_ID=$created_proof_id
docker start "$PROOF_CONTAINER_ID" >/dev/null

for _ in $(seq 1 30); do
  docker exec "$PROOF_CONTAINER_ID" systemctl is-system-running --wait >/dev/null 2>&1 && break
  sleep 1
done
docker exec "$PROOF_CONTAINER_ID" systemctl is-system-running >/dev/null
docker exec "$PROOF_CONTAINER_ID" install -d -m 0700 /proof
docker cp "$INSTALLER" "$PROOF_CONTAINER_ID:/proof/nix-installer"
docker exec "$PROOF_CONTAINER_ID" chmod 0555 /proof/nix-installer
[[ $(docker exec "$PROOF_CONTAINER_ID" sha256sum /proof/nix-installer | awk '{print $1}') == "$INSTALLER_SHA256" ]]

{
  printf '%s\n' \
    'dn12_evidence_version=1' \
    'target=linux/aarch64' \
    "base_image=$BASE_IMAGE" \
    "installer_sha256=$INSTALLER_SHA256" \
    "daemon_v3.22.1_sha256=$DAEMON_321_SHA256" \
    "daemon_v3.22.2_sha256=$DAEMON_322_SHA256" \
    "systemd_package_version=$SYSTEMD_VERSION" \
    'host_mounts=none' \
    'host_nix_used=false' \
    'container_cgroup_namespace=private' \
    'container_run_tmpfs=/run,/run/lock' \
    "harness_sha256=$(shasum -a 256 "$0" | awk '{print $1}')"
} >"$EVIDENCE"

run_command() {
  local phase=$1
  shift
  set +e
  docker exec "$PROOF_CONTAINER_ID" "$@" >"$WORK/$phase.stdout" 2>"$WORK/$phase.stderr"
  local result=$?
  set -e
  printf 'phase=%s exit_status=%s\n' "$phase" "$result" >>"$EVIDENCE"
  return "$result"
}

snapshot() {
  local phase=$1
  local store="$WORK/store-$phase.txt"
  {
    printf 'snapshot=%s\n' "$phase"
    docker exec "$PROOF_CONTAINER_ID" /usr/local/bin/determinate-nixd version 2>&1 \
      | grep -E 'Determinate Nixd (daemon|client) version:' || true
    docker exec "$PROOF_CONTAINER_ID" /nix/var/nix/profiles/default/bin/nix --version
    docker exec "$PROOF_CONTAINER_ID" sh -c '
      for path in \
        /usr/local/bin/determinate-nixd \
        /nix/nix-installer \
        /nix/receipt.json \
        /etc/nix/nix.conf \
        /etc/nix/nix.custom.conf \
        /etc/nix/sentry-endpoint \
        /nix/var/determinate/identity.json \
        /nix/var/determinate/netrc \
        /etc/systemd/system/nix-daemon.service \
        /etc/systemd/system/nix-daemon.socket \
        /etc/systemd/system/determinate-nixd.socket
      do
        if [ -f "$path" ] && [ ! -L "$path" ]; then
          stat -c "file path=%n inode=%i mtime=%Y size=%s uid=%u gid=%g mode=%a" "$path"
          sha256sum "$path"
        else
          printf "file path=%s absent_or_invalid=true\\n" "$path"
        fi
      done
      printf "active_nix path=%s\\n" "$(readlink -f /nix/var/nix/profiles/default/bin/nix)"
      printf "profile default=%s\\n" "$(readlink /nix/var/nix/profiles/default)"
      find -P /nix/var/nix/profiles/per-user/root -mindepth 1 -maxdepth 1 -type l \
        -printf "profile %f=%l\\n" | sort
      systemctl show nix-daemon.service \
        -p ActiveState -p SubState -p Result -p ExecMainStatus \
        -p KillMode -p TimeoutStopUSec -p SendSIGKILL
    '
  } >>"$EVIDENCE"
  docker exec "$PROOF_CONTAINER_ID" \
    find /nix/store -mindepth 1 -maxdepth 1 -printf '%f\n' | sort >"$store"
  printf 'store_inventory phase=%s count=%s\n' "$phase" "$(wc -l <"$store" | tr -d ' ')" >>"$EVIDENCE"
}

store_diff() {
  local before=$1
  local after=$2
  local removed="$WORK/store-removed.txt"
  local added="$WORK/store-added.txt"
  comm -23 "$WORK/store-$before.txt" "$WORK/store-$after.txt" >"$removed"
  comm -13 "$WORK/store-$before.txt" "$WORK/store-$after.txt" >"$added"
  printf 'store_diff from=%s to=%s removed=%s added=%s\n' \
    "$before" "$after" \
    "$(wc -l <"$removed" | tr -d ' ')" \
    "$(wc -l <"$added" | tr -d ' ')" >>"$EVIDENCE"
  sed 's/^/store_removed /' "$removed" >>"$EVIDENCE"
  sed 's/^/store_added /' "$added" >>"$EVIDENCE"
}

run_command install env DETSYS_IDS_TELEMETRY=disabled \
  /proof/nix-installer \
  --diagnostic-endpoint http://127.0.0.1:18080 \
  install linux --determinate --no-confirm --no-modify-profile
snapshot installed

run_command same_version /usr/local/bin/determinate-nixd upgrade --version v3.22.1
snapshot same_version
store_diff installed same_version

run_command next_version /usr/local/bin/determinate-nixd upgrade --version v3.22.2
snapshot next_version
store_diff same_version next_version

run_command downgrade /usr/local/bin/determinate-nixd upgrade --version v3.22.1
snapshot downgrade
store_diff next_version downgrade

run_command invalid_version /usr/local/bin/determinate-nixd upgrade --version v0.0.0-pkg-proof && {
  printf '%s\n' 'The invalid-version proof unexpectedly succeeded.' >&2
  exit 69
}
grep -q '404' "$WORK/invalid_version.stderr"
grep -q 'v0.0.0-pkg-proof' "$WORK/invalid_version.stderr"
printf '%s\n' 'invalid_target http_status=404 target=v0.0.0-pkg-proof' >>"$EVIDENCE"
snapshot invalid_version
store_diff downgrade invalid_version

set +e
docker exec "$PROOF_CONTAINER_ID" sh -c \
  'printf "%s\n" "$$" >/proof/disconnect-cli.pid; exec /usr/local/bin/determinate-nixd upgrade --version v3.22.2' \
  >"$WORK/disconnect.stdout" 2>"$WORK/disconnect.stderr" &
readonly DISCONNECT_HOST_PID=$!
set -e

for _ in $(seq 1 120); do
  if grep -q '^Upgrading Determinate Nixd\.\.\.$' "$WORK/disconnect.stdout" 2>/dev/null; then
    break
  fi
  sleep 0.25
done
grep -q '^Upgrading Determinate Nixd\.\.\.$' "$WORK/disconnect.stdout"
readonly DISCONNECT_CLI_PID=$(docker exec "$PROOF_CONTAINER_ID" cat /proof/disconnect-cli.pid)
[[ $DISCONNECT_CLI_PID =~ ^[0-9]+$ ]]
docker exec "$PROOF_CONTAINER_ID" kill -TERM "$DISCONNECT_CLI_PID"
set +e
wait "$DISCONNECT_HOST_PID"
readonly DISCONNECT_STATUS=$?
set -e
printf 'phase=disconnect exit_status=%s cli_pid=<redacted-volatile> signal=TERM\n' "$DISCONNECT_STATUS" >>"$EVIDENCE"

disconnect_settled=false
readonly DISCONNECT_WAIT_SECONDS=180
for _ in $(seq 1 "$DISCONNECT_WAIT_SECONDS"); do
  current_daemon=$(docker exec "$PROOF_CONTAINER_ID" sha256sum /usr/local/bin/determinate-nixd 2>/dev/null | awk '{print $1}')
  current_nix=$(docker exec "$PROOF_CONTAINER_ID" /nix/var/nix/profiles/default/bin/nix --version 2>/dev/null || true)
  current_service=$(docker exec "$PROOF_CONTAINER_ID" systemctl is-active nix-daemon.service 2>/dev/null || true)
  if [[ $current_daemon == "$DAEMON_322_SHA256" && $current_nix == *'Determinate Nix 3.22.2'* && $current_service == active ]]; then
    disconnect_settled=true
    break
  fi
  sleep 1
done
printf 'disconnect_reconciliation wait_seconds=%s target_reached=%s\n' \
  "$DISCONNECT_WAIT_SECONDS" "$disconnect_settled" >>"$EVIDENCE"
snapshot disconnect
store_diff invalid_version disconnect

docker exec "$PROOF_CONTAINER_ID" journalctl -u nix-daemon.service --no-pager \
  | grep -E "State 'stop-sigterm' timed out\. Killing\.|with signal SIGKILL" \
  | sed -E 's/^[A-Z][a-z]{2} [ 0-9]{2} [0-9:]{8} [^ ]+ /journal /; s/process [0-9]+/process <redacted-pid>/' \
  >>"$EVIDENCE"

disconnect_daemon_hash=$(docker exec "$PROOF_CONTAINER_ID" sha256sum /usr/local/bin/determinate-nixd | awk '{print $1}')
disconnect_service=$(docker exec "$PROOF_CONTAINER_ID" systemctl is-active nix-daemon.service)
printf '%s\n' \
  "disconnect_final target_reached=$disconnect_settled daemon_sha256=$disconnect_daemon_hash service=$disconnect_service" \
  'receipt_contents_retained=false' \
  'full_logs_retained=false' \
  'binaries_retained=false' \
  'secrets_retained=false' >>"$EVIDENCE"

cleanup_owned
write_cleanup 0

grep -q 'absent=false' "$CLEANUP" && exit 70
cleanup_temp
trap - EXIT HUP INT TERM
