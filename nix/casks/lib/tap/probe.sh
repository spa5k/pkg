#!/usr/bin/env bash
# pkg sandbox behavior probe: runs INSIDE the ordinary (non-FOD) export
# derivation BEFORE any tap Ruby executes.
#
# SECURITY INVARIANT: every host access below MUST be denied. Any
# success is a sandbox hole and fails the whole export.
#
# Host-side prerequisites, verified by the trusted parent (this probe
# cannot see host configuration and must never weaken them):
#   * macOS Nix daemon: sandbox=true, fallback=false, minimal read-only
#     paths, daemon TMPDIR outside any public tmp; this build uses
#     __darwinAllowLocalNetworking=false. Background: IMPLEMENTATION.md.
#   * Positive control, FRESH per import (no cached probe):
#     CANARY_PATH is a nonce canary file, mode 0644, inside a FRESH
#     random public directory /tmp/<random> (that directory has mode
#     0777 and its parent IS the public tmp root — the canary parent
#     must NOT be /tmp or /private/tmp itself). The host listener
#     process holds the canary directory alive for the whole build.
#     CANARY_SHA256 is that nonce's sha256. A LIVE loopback TCP
#     listener on 127.0.0.1:CANARY_PORT stays up for the whole build.
#     The parent verified canary readability and listener acceptance
#     on the host immediately before this build.
#
# Contract (environment, all required):
#   CANARY_PATH    host canary file /tmp/<random>/<name>, file 0644,
#                  inside a fresh 0777 random public-tmp subdirectory
#   CANARY_SHA256  sha256 hex (64 chars) of the fresh canary nonce
#   CANARY_PORT    live host loopback TCP listener port (1-65535)
set -u

fail() {
  echo "pkg-probe: FAIL: $1" >&2
  exit 1
}

: "${CANARY_PATH:?pkg-probe: CANARY_PATH is not set}"
: "${CANARY_SHA256:?pkg-probe: CANARY_SHA256 is not set}"
: "${CANARY_PORT:?pkg-probe: CANARY_PORT is not set}"

# The canary must live in a fresh random subdirectory of the public
# tmp, never directly in the tmp root: a directly-in-/tmp canary makes
# the write probe below ambiguous (the build's own /tmp entries would
# collide with host paths). No chroot-path heuristics are used.
canary_dir="$(dirname "$CANARY_PATH")"
case "$canary_dir" in
  /tmp|/private/tmp)
    fail "canary parent ${canary_dir} is the tmp root: the canary must be inside a fresh random subdirectory" ;;
  /tmp/*|/private/tmp/*) ;;
  *)
    fail "canary parent ${canary_dir} is not a subdirectory of the public tmp" ;;
esac

case "$CANARY_SHA256" in
  *[!0-9a-f]*|'') fail "CANARY_SHA256 is not 64 hex chars" ;;
  *) [ "${#CANARY_SHA256}" -eq 64 ] || fail "CANARY_SHA256 is not 64 hex chars" ;;
esac

case "$CANARY_PORT" in
  ''|*[!0-9]*) fail "CANARY_PORT is not numeric (got $CANARY_PORT)" ;;
  *) { [ "$CANARY_PORT" -ge 1 ] && [ "$CANARY_PORT" -le 65535 ]; } ||
       fail "CANARY_PORT out of range 1..65535 (got $CANARY_PORT)" ;;
esac

command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is missing from the build environment"

# 1. host read denied. Any readable result is a hole; if the readable
#    bytes match the fresh nonce, the host tmp is fully exposed.
if content=$(cat "$CANARY_PATH" 2>/dev/null); then
  actual=$(printf '%s' "$content" | sha256sum | cut -d' ' -f1)
  if [ "$actual" = "$CANARY_SHA256" ]; then
    fail "host read ALLOWED: canary $CANARY_PATH readable and fresh nonce sha256 matches"
  fi
  fail "host read ALLOWED: canary $CANARY_PATH is readable (sha256 $actual, expected $CANARY_SHA256)"
fi
if [ -e "$CANARY_PATH" ]; then
  fail "host read ALLOWED: canary $CANARY_PATH is stat-able"
fi

# 2. host write denied: attempt to create a SIBLING of the canary
#    inside the parent-created fresh random host directory, WITHOUT
#    creating any directory first.
#      * Correct Linux sandbox: the build runs in a private chroot
#        whose /tmp is the throwaway chroot filesystem; the host's
#        /tmp/<random> does not exist there, so the create fails.
#      * Correct macOS sandbox: seatbelt denies the write.
#      * Hole (host public tmp or canary dir mounted in, or sandbox
#        off): the random directory exists here (check 1 proved the
#        path resolves when readable), the sibling create succeeds,
#        and the probe fails the build.
probe_target="${canary_dir}/pkg-probe-write.$$"
if ( : > "$probe_target" ) 2>/dev/null && [ -e "$probe_target" ]; then
  rm -f "$probe_target" 2>/dev/null || true
  fail "host write ALLOWED: created $probe_target"
fi

# 3. loopback connect denied. Live attempt each time (no cached
#    result): the parent keeps a listener accepting on the host, so a
#    successful connect proves host loopback reachability.
#    FAIL-CLOSED: a ruby load error, an unexpected exception, or a
#    timeout is an abort, never a silent DENIED. Only the narrow
#    SystemCallError network-denial path inside the probe prints the
#    exact DENIED marker.
command -v ruby >/dev/null 2>&1 || fail "ruby is missing from the build environment: the loopback probe cannot run"
probe_err="$(mktemp pkg-probe-ruby.XXXXXX)"
result=$(timeout 10 ruby -rsocket -e '
  begin
    TCPSocket.new("127.0.0.1", ARGV.fetch(0)).close
    puts "CONNECTED"
  rescue SystemCallError
    # Narrow: kernel-level network denial (ECONNREFUSED/EPERM/ENETUNREACH
    # ...). Anything else (bad argument, load error, unexpected raise)
    # escapes and fails the probe.
    puts "DENIED"
  end
' "$CANARY_PORT" 2>"$probe_err")
status=$?
# Read the bounded stderr BEFORE unlinking the file so real Ruby
# errors (load failures, argument errors) are reported, not lost.
err_head="$(head -c 300 "$probe_err" 2>/dev/null || true)"
rm -f "$probe_err"
case "$status" in
  0) ;;
  124) fail "loopback probe timed out after 10s: cannot prove denial, aborting" ;;
  *) fail "loopback probe did not run (exit ${status}): ${err_head:-<no stderr>}" ;;
esac
[ "$result" = "DENIED" ] || fail "loopback connect ALLOWED: 127.0.0.1:$CANARY_PORT returned ${result:-<no marker>} (expected exact DENIED)"

echo "pkg-probe: denied: host-read, host-write, loopback" >&2
