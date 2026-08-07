#!/bin/sh
# Negative control: the same build request must fail closed without privilege.
set -eu

IMAGE='nixos/nix:2.34.8'
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
OUT_DIR="$SCRIPT_DIR/out/linux-unprivileged"

if ! command -v docker >/dev/null 2>&1; then
    printf '%s\n' 's5: docker is unavailable' >&2
    exit 69
fi
if [ -L "$OUT_DIR" ]; then
    printf '%s\n' 's5: output directory symlinks are refused' >&2
    exit 78
fi
mkdir -p "$OUT_DIR"
chmod 700 "$OUT_DIR"

set +e
docker run --rm \
    --network=bridge \
    --env 'S5_BUILD_APPROVAL=single-operation-approved' \
    --mount "type=bind,src=$SCRIPT_DIR,dst=/harness,readonly" \
    --mount "type=bind,src=$OUT_DIR,dst=/out" \
    "$IMAGE" \
    /bin/sh /harness/inside-linux.sh
status=$?
set -e

[ "$status" -eq 69 ] || {
    printf '%s\n' 's5: unprivileged lane did not fail with exit 69' >&2
    exit 1
}
grep -q '"complete":false' "$OUT_DIR/report.json"
grep -q '"failure":"daemon_socket_unready"' "$OUT_DIR/report.json"
printf '%s\n' 's5 unprivileged negative control passed'
