#!/usr/bin/env bash
# Focused builder verification (E2B Linux, headless). Positive cases build
# and execute; expected-failure cases must fail with the recorded pattern.
# No vendor installer ever runs; no GUI claims.
set -uo pipefail
cd "$(dirname "$0")" || exit 1

# nix comes from the daemon profile on this runner. Source it only when
# nix is not already on PATH and the profile file exists; every other
# setup state is a failure and must stop the run with a nonzero exit.
if ! command -v nix-build >/dev/null 2>&1; then
  if [ -f /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]; then
    # shellcheck source=/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh disable=SC1091 # needed only where /nix is absent
    . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh || exit 1
  else
    echo "FATAL: nix-build not on PATH and no nix-daemon.sh profile found" >&2
    exit 1
  fi
fi

pass=0; fail=0
ok()  { echo "PASS: $1"; pass=$((pass+1)); }
bad() { echo "FAIL: $1"; fail=$((fail+1)); }

if nix eval --json --impure --expr '(import ./checks.nix {}).osBoundsIndex' \
  | python3 -c 'import json,sys; x=json.load(sys.stdin); assert x == {"aarch64-darwin": {"minMacos": "14.2", "maxMacos": "15.9"}, "x86_64-linux": {"minMacos": None, "maxMacos": None}}, x'; then
  ok "macOS bounds survive the client index projection"
else
  bad "macOS bounds index projection"
fi

# --- positive: real static ELF through the full builder
if op_out="$(nix-build checks.nix -A op --no-out-link 2>/dev/null)" \
  && [ "$("$op_out/bin/op" --version)" = "2.39.0" ]; then
  ok "static op builds and runs"
else
  bad "static op"
fi

# --- positive: app archive (rename, $APPDIR map, docs, shortcut omission)
if app_out="$(nix-build checks.nix -A app --no-out-link 2>/dev/null)" \
  && [ -d "$app_out/Applications/Renamed.app" ] \
  && [ "$("$app_out/bin/hicli")" = "hi-from-app" ] \
  && [ "$("$app_out/bin/hicur")" = "tool" ] \
  && { [ -f "$app_out/share/man/man1/hicli.1" ] || [ -f "$app_out/share/man/man1/hicli.1.gz" ]; } \
  && [ -f "$app_out/share/bash-completion/completions/hicli" ] \
  && [ -f "$app_out/share/zsh/site-functions/_hicli" ] \
  && [ -f "$app_out/share/fish/vendor_completions.d/hicli.fish" ]; then
  ok "app archive: rename, anchored CLIs, docs"
else
  bad "app archive"
fi

# --- positive: relocatable pkg payload
if pkg_out="$(nix-build checks.nix -A pkg --no-out-link 2>/dev/null)" \
  && ls "$pkg_out/Applications"/*.app >/dev/null 2>&1; then
  ok "relocatable pkg payload installs bundles"
else
  bad "relocatable pkg"
fi

# --- positive: dynamic ELF repaired generically (autoPatchelf).
# dynelf-verify depends on the dynelf package, so building it covers both.
if verify="$(nix-build checks.nix -A dynelf-verify --no-out-link 2>/dev/null)" \
  && grep -q "^interpreter=/nix/store/" "$verify" \
  && grep -q "run=ZLIB_OK" "$verify"; then
  ok "dynamic ELF: interpreter/rpath repaired to store, runs, no host libs"
else
  bad "dynamic elf"
fi

# --- expected failures: build must fail naming the cause.
# The producer (nix eval | python3) runs as a checked command-substitution
# assignment: with pipefail, any producer failure is caught here and
# aborts the run. Feeding mapfile from a process substitution would hide
# a producer failure and silently execute zero cases.
if ! case_list="$(nix eval --json --impure --expr '
  let c = import ./checks.nix {}; in
  map (e: e.name + "|" + e.pattern) c.expectFail' \
  | python3 -c 'import json,sys; [print(x) for x in json.load(sys.stdin)]')"; then
  echo "FATAL: expected-failure case list generation failed" >&2
  exit 1
fi
if [ -z "$case_list" ]; then
  echo "FATAL: expected-failure case list is empty" >&2
  exit 1
fi
mapfile -t cases <<<"$case_list"
for case in "${cases[@]}"; do
  name="${case%%|*}"
  pattern="${case#*|}"
  if [ -z "$name" ] || [ "$pattern" = "$case" ] || [ -z "$pattern" ]; then
    echo "FATAL: malformed case entry (need name|pattern): $case" >&2
    exit 1
  fi
  log=$(mktemp)
  if nix-build checks.nix -A "expectDrv.$name" --no-out-link >"$log" 2>&1; then
    bad "$name (build unexpectedly succeeded)"
  elif grep -q "$pattern" "$log"; then
    ok "$name rejected: $pattern"
  else
    bad "$name (no pattern '$pattern'): $(grep 'cask-build:' "$log" | tail -1)"
  fi
  rm -f "$log"
done

echo "----"
echo "pass=$pass fail=$fail"
[ "$fail" -eq 0 ]
