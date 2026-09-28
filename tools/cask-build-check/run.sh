#!/usr/bin/env bash
# Focused builder verification (E2B Linux, headless). Positive cases build
# and execute; expected-failure cases must fail with the recorded pattern.
# No vendor installer ever runs; no GUI claims.
set -uo pipefail
cd "$(dirname "$0")"
source /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh

pass=0; fail=0
ok()  { echo "PASS: $1"; pass=$((pass+1)); }
bad() { echo "FAIL: $1"; fail=$((fail+1)); }

# --- positive: real static ELF through the full builder
op_out="$(nix-build checks.nix -A op --no-out-link 2>/dev/null)" \
  && [ "$("$op_out/bin/op" --version)" = "2.39.0" ] \
  && ok "static op builds and runs" || bad "static op"

# --- positive: app archive (rename, $APPDIR map, docs, shortcut omission)
app_out="$(nix-build checks.nix -A app --no-out-link 2>/dev/null)" \
  && [ -d "$app_out/Applications/Renamed.app" ] \
  && [ "$("$app_out/bin/hicli")" = "hi-from-app" ] \
  && [ "$("$app_out/bin/hicur")" = "tool" ] \
  && [ -f "$app_out/share/man/man1/hicli.1" -o -f "$app_out/share/man/man1/hicli.1.gz" ] \
  && [ -f "$app_out/share/bash-completion/completions/hicli" ] \
  && [ -f "$app_out/share/zsh/site-functions/_hicli" ] \
  && [ -f "$app_out/share/fish/vendor_completions.d/hicli.fish" ] \
  && ok "app archive: rename, anchored CLIs, docs" || bad "app archive"

# --- positive: relocatable pkg payload
pkg_out="$(nix-build checks.nix -A pkg --no-out-link 2>/dev/null)" \
  && ls "$pkg_out/Applications"/*.app >/dev/null 2>&1 \
  && ok "relocatable pkg payload installs bundles" || bad "relocatable pkg"

# --- positive: dynamic ELF repaired generically (autoPatchelf)
dyn_out="$(nix-build checks.nix -A dynelf --no-out-link 2>/dev/null)" \
  && verify="$(nix-build checks.nix -A dynelf-verify --no-out-link 2>/dev/null)" \
  && grep -q "^interpreter=/nix/store/" "$verify" \
  && grep -q "run=ZLIB_OK" "$verify" \
  && ok "dynamic ELF: interpreter/rpath repaired to store, runs, no host libs" \
  || bad "dynamic elf"

# --- expected failures: build must fail naming the cause
mapfile -t cases < <(nix eval --json --impure --expr '
  let c = import ./checks.nix {}; in
  map (e: e.name + "|" + e.pattern) c.expectFail' \
  | python3 -c 'import json,sys; [print(x) for x in json.load(sys.stdin)]')
for case in "${cases[@]}"; do
  name="${case%%|*}"; pattern="${case#*|}"
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
