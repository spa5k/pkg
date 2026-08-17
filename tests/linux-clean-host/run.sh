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

repo=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
stage_root=$(mktemp -d "${TMPDIR:-/tmp}/pkg-linux-alpha.XXXXXXXX")
raw_stage="$stage_root/raw"
artifact_context="$stage_root/artifact"
docker_platform=linux/amd64

image=pkg-linux-clean-host:local
container="pkg-linux-clean-host-$$"
stop_container() {
    docker rm --force "$container" >/dev/null 2>&1 || true
}
cleanup() {
    stop_container
    rm -rf "$stage_root"
}
trap cleanup EXIT INT TERM

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
cp -a "$raw_stage/publication-1" "$raw_stage/publication-2" "$artifact_context/"
cp "$repo/tests/linux-clean-host/pkg-proof-server.py" \
    "$repo/tests/linux-clean-host/pkg-proof-release.service" \
    "$artifact_context/"
cp -a "$artifact_context/v0.1.0-alpha.1" "$artifact_context/publication-1/"
cp -a "$artifact_context/v0.1.0-alpha.1" "$artifact_context/publication-2/"

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$artifact_context" && sha256sum --check --strict SHA256SUMS)
else
    (cd "$artifact_context" && shasum -a 256 --check SHA256SUMS)
fi

if [ -n "$artifact_output" ]; then
    mkdir -p "$artifact_output"
    tar -C "$artifact_context" -czf \
        "$artifact_output/pkg-v0.1.0-alpha.1-x86_64-linux-proof.tar.gz" \
        SHA256SUMS install.sh v0.1.0-alpha.1
fi

echo "+ build clean host from staged artifacts only"
docker build \
    --platform "$docker_platform" \
    --file "$repo/tests/linux-clean-host/Dockerfile" \
    --tag "$image" \
    "$artifact_context"

wait_container_ready() {
    ready=0
    attempt=0
    while [ "$attempt" -lt 60 ]; do
        if docker exec "$container" curl --fail --silent https://127.0.0.1:8443/root.json >/dev/null; then
            ready=1
            break
        fi
        attempt=$((attempt + 1))
        sleep 1
    done
    if [ "$ready" -ne 1 ]; then
        docker logs "$container"
        exit 1
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

shipping_installer=/srv/pkg-release/v0.1.0-alpha.1/pkg-installer-x86_64-linux

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

echo "+ interrupted install recovery"
start_container
docker exec --detach "$container" sh -c \
    "echo \$\$ > /tmp/pkg-install.pid; exec $shipping_installer > /tmp/pkg-install-interrupted.log 2>&1"
journal_ready=0
attempt=0
while [ "$attempt" -lt 600 ]; do
    if docker exec "$container" sh -c 'test -s /var/lib/pkg-install/transaction-v1.json' \
        && docker exec "$container" sh -c 'kill -STOP "$(cat /tmp/pkg-install.pid)"'; then
        if docker exec "$container" python3 -c \
            'import json,sys; entries=json.load(open("/var/lib/pkg-install/transaction-v1.json"))["entries"]; sys.exit(not entries or entries[-1].get("state") != "created")'; then
            journal_ready=1
            break
        fi
        docker exec "$container" sh -c 'kill -CONT "$(cat /tmp/pkg-install.pid)"'
    fi
    attempt=$((attempt + 1))
    sleep 0.05
done
if [ "$journal_ready" -ne 1 ]; then
    docker exec "$container" cat /tmp/pkg-install-interrupted.log >&2 || true
    echo "The install journal did not become durable before the installer exited." >&2
    exit 1
fi
docker kill "$container" >/dev/null
docker start "$container" >/dev/null
wait_container_ready
docker exec "$container" "$shipping_installer"
docker exec "$container" sh -eu -c '
    test ! -e /var/lib/pkg-install/transaction-v1.json
    test "$(/usr/local/bin/pkg --version)" = "pkg 0.1.0-alpha.1"
    systemctl is-active --quiet pkg-nix-broker.socket
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

echo "+ verify services and ordinary-user isolation"
docker exec "$container" sh -eu -c '
    systemctl is-active --quiet pkg-nix-daemon.socket
    systemctl is-active --quiet pkg-root-helper.socket
    systemctl is-active --quiet pkg-nix-broker.socket
    test "$(/usr/local/bin/pkg --version)" = "pkg 0.1.0-alpha.1"
    ! su -s /bin/sh proof-user -c "command -v nix"
    ! su -s /bin/sh proof-user -c "/opt/pkg/bin/pkg-root-helper"
    ! su -s /bin/sh proof-user -c "/opt/pkg/bin/pkg-nix-broker"
    ! su -s /bin/sh proof-user -c "/opt/pkg/nix/current/bin/nix --version"
    su -s /bin/sh proof-user -c "test -w /run/pkg/broker.sock"
    ! su -s /bin/sh proof-user -c "test -w /run/pkg-helper/root-helper.sock"
    ! su -s /bin/sh proof-user -c "test -w /nix/var/nix/daemon-socket/socket"
    ! su -s /bin/sh proof-user -c "test -r /opt/pkg/etc/pkg/nix.conf"
    cp /usr/local/bin/pkg /tmp/pkg-after-uninstall
    chmod 0755 /tmp/pkg-after-uninstall
'

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

echo "+ pkg install cxx-prettyprint with approved local build"
if ! local_build_output=$(docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --yes --jsonl install cxx-prettyprint"); then
    printf '%s\n' "$local_build_output" >&2
    exit 1
fi
printf '%s\n' "$local_build_output" | grep -F '"type":"build_started"' >/dev/null
printf '%s\n' "$local_build_output" | grep -F '"selector":"cxx-prettyprint"' >/dev/null

echo "+ publish channel sequence 2"
docker exec "$container" sh -eu -c '
    ln -s /srv/pkg-releases/2 /srv/pkg-release.next
    mv -Tf /srv/pkg-release.next /srv/pkg-release
'

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

echo "+ pkg --yes uninstall"
docker exec "$container" /usr/local/bin/pkg --yes uninstall

echo "+ idempotent uninstall and final absence"
docker exec "$container" sh -eu -c '
    /tmp/pkg-after-uninstall --yes uninstall
    test ! -e /opt/pkg
    test ! -e /var/lib/pkg
    test ! -e /run/pkg
    test ! -e /nix
    test ! -e /home/proof-user/.local/share/pkg
    ! getent passwd pkg-nix-broker
    ! getent group pkg-nix-broker
    ! getent group nixbld
  ! systemctl list-unit-files --no-legend \
    | grep -Eq "^pkg-(nix-daemon|root-helper|nix-broker)\\.(service|socket)"
'

echo "Linux product install checkpoint passed."
