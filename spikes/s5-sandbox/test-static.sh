#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/s5-static.XXXXXX")
cleanup() {
    rm -rf "$SCRATCH"
}
trap cleanup EXIT INT TERM

/bin/sh "$SCRIPT_DIR/render-nix-conf.sh" linux > "$SCRATCH/linux.conf"
/bin/sh "$SCRIPT_DIR/render-nix-conf.sh" macos > "$SCRATCH/macos.conf"
diff -u "$SCRIPT_DIR/fixtures/nix-linux.conf" "$SCRATCH/linux.conf"
diff -u "$SCRIPT_DIR/fixtures/nix-macos.conf" "$SCRATCH/macos.conf"

grep -q '^use-cgroups = true$' "$SCRATCH/linux.conf"
if grep -q 'cgroup' "$SCRATCH/macos.conf"; then
    printf '%s\n' 's5: macOS config unexpectedly enables cgroups' >&2
    exit 1
fi
grep -q '^sandbox = true$' "$SCRATCH/linux.conf"
grep -q '^sandbox-fallback = false$' "$SCRATCH/linux.conf"
grep -q '^sandbox = true$' "$SCRATCH/macos.conf"
grep -q '^sandbox-fallback = false$' "$SCRATCH/macos.conf"

/bin/sh -n "$SCRIPT_DIR/run-native-macos.sh"
/bin/sh -n "$SCRIPT_DIR/inside-macos.sh"
grep -q '"resourceBoundary":"no_cgroups_on_macos"' "$SCRIPT_DIR/inside-macos.sh"
grep -q 'ordinaryUserProbe.*socket_traversal_denied' "$SCRIPT_DIR/inside-macos.sh"

printf '%s\n' 's5 static checks passed'
