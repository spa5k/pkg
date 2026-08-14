#!/bin/sh
set -eu

case "$(uname -m)" in
    arm64|aarch64)
        docker_platform=linux/arm64
        pkg_system=aarch64-linux
        ;;
    x86_64|amd64)
        docker_platform=linux/amd64
        pkg_system=x86_64-linux
        ;;
    *)
        echo "This Linux proof does not support this host architecture." >&2
        exit 1
        ;;
esac

image=pkg-linux-clean-host:local
container="pkg-linux-clean-host-$$"
cleanup() {
    docker rm --force "$container" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

echo "+ docker build --platform $docker_platform --build-arg PKG_SYSTEM=$pkg_system"
docker build \
    --platform "$docker_platform" \
    --build-arg "PKG_SYSTEM=$pkg_system" \
    --file tests/linux-clean-host/Dockerfile \
    --tag "$image" \
    .

echo "+ docker run --privileged --cgroupns=private"
docker run \
    --detach \
    --privileged \
    --cgroupns=private \
    --name "$container" \
    --tmpfs /run \
    --tmpfs /run/lock \
    "$image" >/dev/null

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

echo "+ verify clean host"
docker exec "$container" sh -eu -c '
    ! command -v nix
    test ! -e /nix
    test ! -e /opt/pkg
'

echo "+ pkg-install"
docker exec "$container" /usr/local/sbin/pkg-install

echo "+ pkg-install retry"
docker exec "$container" /usr/local/sbin/pkg-install

echo "+ verify services and ordinary-user isolation"
docker exec "$container" sh -eu -c '
    systemctl is-active --quiet pkg-nix-daemon.socket
    systemctl is-active --quiet pkg-root-helper.socket
    systemctl is-active --quiet pkg-nix-broker.socket
    test "$(/usr/local/bin/pkg --version | cut -d" " -f1)" = pkg
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
    ! getent passwd pkg-nix-broker
    ! getent group pkg-nix-broker
    ! getent group nixbld
  ! systemctl list-unit-files --no-legend \
    | grep -Eq "^pkg-(nix-daemon|root-helper|nix-broker)\\.(service|socket)"
'

echo "Linux product install checkpoint passed."
