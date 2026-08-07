#!/bin/sh
# s3-probe wrapper (Spike S3 / PR-7 / DR-003).
#
# Closed grammar (no abbreviations, no --flag=value, no eval anywhere):
#   ./run.sh fake                       [OUT_DIR]
#   ./run.sh detect                     [OUT_DIR]
#   ./run.sh preflight ABSOLUTE_NIX_BIN [OUT_DIR]
#
# fake     — pure-harness lane; no network, no Nix, no keychain. Validates the
#            harness plumbing (report schema, atomic artifact writes, exit
#            codes) only. This is NOT evidence.
# detect   — read-only host Detect lane. WARNING: when explicitly invoked it
#            runs the fixed read-only `/usr/bin/security find-identity` probe,
#            so it reads default-keychain identity metadata/counts plus
#            `nixbld` build-group/`_nixbld*` member metadata. It NEVER accepts credentials,
#            NEVER unlocks/signs/notarizes, and NEVER writes keychain data.
# preflight — Preflight cache-coverage lane. WARNING: it EXECUTES the supplied
#            absolute Nix binary (its exact Nix 2.34.8 version verified at
#            runtime). It is build-free and activation-free but NOT read-only:
#            `nix flake prefetch` may fetch the pinned GitHub source and write
#            normal Nix store/fetch/eval state, and the `nix store info`/
#            `nix path-info` availability queries target cache.nixos.org. No
#            package build, profile activation, or signing; the s3-probe Preflight
#            probes use no shell/PATH lookup (this wrapper itself resolves
#            cargo/rustc on PATH).
#
# The `detect` wrapper intentionally does NOT accept a `--nix-bin`; it runs a
# pure keychain/toolchain Detect. To pass an optional `--nix-bin` (existence
# check only, never executed, never PATH-searched) run the binary directly:
#   s3-probe detect --nix-bin /absolute/nix [--out-dir PATH]
set -u

usage='Usage: ./run.sh fake [OUT_DIR] | ./run.sh detect [OUT_DIR] | ./run.sh preflight ABSOLUTE_NIX_BIN [OUT_DIR]'

# Safely resolve and cd to the script directory (handle relative $0).
case $0 in
  */*) cd "${0%/*}" || exit 70 ;;
esac

# Parse and validate mode + argument counts.
case ${1:-} in
  fake)
    if [ $# -gt 2 ]; then
      printf 's3-probe: %s\n' "$usage" >&2
      exit 64
    fi
    out_dir=${2:-target/s3-fake}
    set -- fake --out-dir "$out_dir"
    ;;
  detect)
    if [ $# -gt 2 ]; then
      printf 's3-probe: %s\n' "$usage" >&2
      exit 64
    fi
    # Effect warning (see header): reads default-keychain identity metadata/
    # counts plus nixbld build-group/_nixbld* member metadata; no credentials/writes/
    # signing.
    printf 's3-probe: detect reads default-keychain identity metadata/counts (read-only); no credentials, no keychain writes, no signing\n' >&2
    out_dir=${2:-target/s3-detect}
    set -- detect --out-dir "$out_dir"
    ;;
  preflight)
    if [ $# -lt 2 ] || [ $# -gt 3 ]; then
      printf 's3-probe: %s\n' "$usage" >&2
      exit 64
    fi
    nix_bin=$2
    case $nix_bin in
      /*) ;;
      *)
        printf 's3-probe: %s\n' "$usage" >&2
        exit 64
        ;;
    esac
    # Effect warning (see header): executes the supplied absolute Nix binary;
    # build-free/activation-free but NOT read-only (prefetch may write Nix
    # state; availability queries target cache.nixos.org).
    printf 's3-probe: preflight executes the supplied absolute Nix binary (exact 2.34.8 verified); NOT read-only: flake prefetch may write Nix store/fetch/eval state and availability queries target cache.nixos.org\n' >&2
    out_dir=${3:-target/s3-preflight}
    set -- preflight --nix-bin "$nix_bin" --out-dir "$out_dir"
    ;;
  *)
    printf 's3-probe: %s\n' "$usage" >&2
    exit 64
    ;;
esac

# Require cargo and rustc on PATH; the only toolchain gate is exact: rustc
# --version must print exactly "rustc 1.96.1 ..." or this wrapper exits 70.
# RUSTUP_TOOLCHAIN=1.96.1 and the repo-root rust-toolchain.toml (channel
# "1.96.1") steer RUSTUP-AWARE tooling only (e.g. rustup shims, or
# "rustup run 1.96.1 ..."); a standalone or Homebrew cargo/rustc on PATH
# ignores them and is caught by this exact-version check, not silently
# selected. This wrapper itself never invokes rustup or any installer and
# downloads nothing, but the PATH-resolved cargo/rustc MAY be rustup shims, in
# which case rust-toolchain.toml can cause rustup to obtain the toolchain if it
# is not already installed. Cargo --offline constrains crate/dependency access
# only; it does not stop rustup toolchain acquisition. Callers who require zero
# network must preinstall/select the 1.96.1 toolchain and configure their
# toolchain manager themselves (no install step is added here).
command -v cargo >/dev/null 2>&1 || {
  printf 's3-probe: cargo not found on PATH\n' >&2
  exit 70
}
command -v rustc >/dev/null 2>&1 || {
  printf 's3-probe: rustc not found on PATH\n' >&2
  exit 70
}
rustc_version=$(rustc --version 2>/dev/null) || {
  printf 's3-probe: rustc --version failed\n' >&2
  exit 70
}
case $rustc_version in
  'rustc 1.96.1 '*) ;;
  *)
    printf 's3-probe: rustc 1.96.1 required\n' >&2
    exit 70
    ;;
esac

# Run s3-probe with the argv built via set -- (no eval).
cargo run --locked --offline --release --bin s3-probe -- "$@"
status=$?

# Only echo a summary for the two binary outcomes that guarantee artifacts were
# written: 0 (Complete) and 69 (Incomplete still writes report.json/summary.md).
# For any other status (64/70, cargo failure) skip the summary entirely so a
# stale or pre-existing summary.md under --out-dir is never echoed. The runner's
# exit status is preserved unchanged either way.
case $status in
  0|69)
    if [ -f "$out_dir/summary.md" ]; then
      printf '%s\n' '--- summary.md ---'
      line=''
      while IFS= read -r line || [ -n "$line" ]; do
        printf '%s\n' "$line"
      done < "$out_dir/summary.md"
    fi
    ;;
esac

exit "$status"
