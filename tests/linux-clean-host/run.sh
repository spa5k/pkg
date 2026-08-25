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

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$artifact_context" && sha256sum --check --strict SHA256SUMS)
else
    (cd "$artifact_context" && shasum -a 256 --check SHA256SUMS)
fi

if [ -n "$artifact_output" ]; then
    : "${PKG_CARGO_ABOUT:?set PKG_CARGO_ABOUT for a candidate archive}"
    if [ -e "$artifact_output" ] || [ -L "$artifact_output" ]; then
        echo "artifact output must not exist: $artifact_output" >&2
        exit 1
    fi
    mkdir -p -m 0700 "$artifact_output"
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
    artifact_context=$candidate_context
fi

cp -a "$raw_stage/publication-1" "$raw_stage/publication-2" "$artifact_context/"
cp "$repo/tests/linux-clean-host/pkg-proof-server.py" \
    "$repo/tests/linux-clean-host/pkg-proof-release.service" \
    "$artifact_context/"
cp -a "$artifact_context/v0.1.0-alpha.7" "$artifact_context/publication-1/"
cp -a "$artifact_context/v0.1.0-alpha.7" "$artifact_context/publication-2/"
if [ -n "$artifact_output" ]; then
    mkdir "$artifact_output/evidence"
    cp -a "$artifact_context/." "$artifact_output/evidence/"
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
    ! su -s /bin/sh proof-user -c "command -v nix"
    ! su -s /bin/sh proof-user -c "/opt/pkg/bin/pkg-root-helper"
    ! su -s /bin/sh proof-user -c "/opt/pkg/bin/pkg-nix-broker"
    su -s /bin/sh proof-user -c "test -w /run/pkg/broker.sock"
    ! su -s /bin/sh proof-user -c "test -w /run/pkg-helper/root-helper.sock"
    ! su -s /bin/sh proof-user -c "test -r /opt/pkg/etc/pkg/nix.conf"
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

echo "+ pkg remove all installed packages"
docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --yes remove hello ripgrep fd bat tree wget git tmux zoxide fzf cxx-prettyprint"
docker exec "$container" su - proof-user -c \
    "/usr/local/bin/pkg --json list" \
    | grep -F '"entries":[]' >/dev/null

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

echo "+ record vendor-owned uninstall residue"
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
'
stop_container
}

for lifecycle_run in 1 2; do
    prove_lifecycle "$lifecycle_run"
done

echo "Linux vendor install/uninstall and product package lifecycle proof passed."
echo "Docker limits: no host boot or reboot, SELinux, foreign-host coexistence, or full distribution matrix."
