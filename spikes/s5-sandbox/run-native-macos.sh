#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
APPROVAL=not-approved

case "${1:-}" in
    '') ;;
    --approve-build) APPROVAL=single-operation-approved ;;
    *)
        printf '%s\n' 'usage: run-native-macos.sh [--approve-build]' >&2
        exit 64
        ;;
esac

[ "$(uname -s)" = Darwin ] || {
    printf '%s\n' 's5: native macOS evidence requires Darwin' >&2
    exit 69
}

if [ "$(id -u)" -ne 0 ]; then
    exec sudo env \
        S5_BUILD_APPROVAL="$APPROVAL" \
        S5_EVIDENCE_OWNER_UID="$(id -u)" \
        S5_EVIDENCE_OWNER_GID="$(id -g)" \
        /bin/sh "$SCRIPT_DIR/inside-macos.sh"
fi

S5_BUILD_APPROVAL="$APPROVAL" \
S5_EVIDENCE_OWNER_UID="$(stat -f '%u' "$SCRIPT_DIR")" \
S5_EVIDENCE_OWNER_GID="$(stat -f '%g' "$SCRIPT_DIR")" \
    /bin/sh "$SCRIPT_DIR/inside-macos.sh"
