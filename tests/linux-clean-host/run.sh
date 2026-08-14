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

echo "+ docker run --privileged --cgroupns=host"
docker run \
    --detach \
    --privileged \
    --cgroupns=host \
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
