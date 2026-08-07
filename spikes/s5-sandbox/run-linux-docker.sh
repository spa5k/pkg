#!/bin/sh
# Spike S5 Linux evidence launcher. Runs only inside Docker Desktop's Linux VM.
set -eu

IMAGE='nixos/nix:2.34.8'
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ "${1:-}" = '--approve-build' ]; then
    OUT_DIR="$SCRIPT_DIR/out/linux-docker"
    APPROVAL='--approve-build'
else
    OUT_DIR=${1:-"$SCRIPT_DIR/out/linux-docker"}
    APPROVAL=${2:-}
fi

if ! command -v docker >/dev/null 2>&1; then
    printf '%s\n' 's5: docker is unavailable' >&2
    exit 69
fi
case "$OUT_DIR" in
    /*) ;;
    *) OUT_DIR="$SCRIPT_DIR/$OUT_DIR" ;;
esac
if [ -L "$OUT_DIR" ]; then
    printf '%s\n' 's5: output directory symlinks are refused' >&2
    exit 78
fi
mkdir -p "$OUT_DIR"
chmod 700 "$OUT_DIR"

case "$APPROVAL" in
    '') APPROVAL_VALUE='not-approved' ;;
    --approve-build) APPROVAL_VALUE='single-operation-approved' ;;
    *)
        printf '%s\n' 'usage: run-linux-docker.sh [OUT_DIR] [--approve-build]' >&2
        exit 64
        ;;
esac

exec docker run --rm \
    --privileged \
    --cgroupns=host \
    --network=bridge \
    --env "S5_BUILD_APPROVAL=$APPROVAL_VALUE" \
    --mount "type=bind,src=$SCRIPT_DIR,dst=/harness,readonly" \
    --mount "type=bind,src=$OUT_DIR,dst=/out" \
    "$IMAGE" \
    /bin/sh /harness/inside-linux.sh
