#!/bin/sh
set -u

usage='Usage: ./run.sh fake [OUT_DIR] | ./run.sh real ABSOLUTE_NIX_BIN [OUT_DIR] [ABSOLUTE_STORE_ROOT]'

# Safely resolve and cd to the script directory (handle relative $0).
case $0 in
  */*) cd "${0%/*}" || exit 70 ;;
esac

# Parse and validate mode + argument counts.
case ${1:-} in
  fake)
    if [ $# -gt 2 ]; then
      printf 's4-runner: %s\n' "$usage" >&2
      exit 64
    fi
    out_dir=${2:-target/s4-fake}
    set -- fake --out-dir "$out_dir"
    ;;
  real)
    if [ $# -lt 2 ] || [ $# -gt 4 ]; then
      printf 's4-runner: %s\n' "$usage" >&2
      exit 64
    fi
    nix_bin=$2
    case $nix_bin in
      /*) ;;
      *)
        printf 's4-runner: %s\n' "$usage" >&2
        exit 64
        ;;
    esac
    out_dir=${3:-target/s4-real}
    store_root=${4:-}
    if [ -n "$store_root" ]; then
      case $store_root in
        /*) ;;
        *)
          printf 's4-runner: %s\n' "$usage" >&2
          exit 64
          ;;
      esac
      set -- real --nix-bin "$nix_bin" --store-root "$store_root" --out-dir "$out_dir"
    else
      set -- real --nix-bin "$nix_bin" --out-dir "$out_dir"
    fi
    ;;
  *)
    printf 's4-runner: %s\n' "$usage" >&2
    exit 64
    ;;
esac

# Select an already-installed exact toolchain. Prefer rustup when available so
# an unrelated system rustc earlier on PATH cannot defeat the repository pin.
# `rustup run` never installs or downloads a missing toolchain.
command -v cargo >/dev/null 2>&1 || {
  printf 's4-runner: cargo not found on PATH\n' >&2
  exit 70
}
command -v rustc >/dev/null 2>&1 || {
  printf 's4-runner: rustc not found on PATH\n' >&2
  exit 70
}
use_rustup=false
if command -v rustup >/dev/null 2>&1; then
  rustc_version=$(rustup run 1.96.1 rustc --version 2>/dev/null) || rustc_version=''
  case $rustc_version in
    'rustc 1.96.1 '*) use_rustup=true ;;
  esac
fi
if [ "$use_rustup" = false ]; then
  rustc_version=$(rustc --version 2>/dev/null) || {
    printf 's4-runner: rustc --version failed\n' >&2
    exit 70
  }
  case $rustc_version in
    'rustc 1.96.1 '*) ;;
    *)
      printf 's4-runner: rustc 1.96.1 required\n' >&2
      exit 70
      ;;
  esac
fi

# Run s4-runner with the argv built via set -- (no eval).
if [ "$use_rustup" = true ]; then
  rustup run 1.96.1 cargo run --locked --offline --release --bin s4-runner -- "$@"
else
  cargo run --locked --offline --release --bin s4-runner -- "$@"
fi
status=$?

# If a summary was produced, echo it under a fixed header, preserving all lines.
if [ -f "$out_dir/summary.md" ]; then
  printf '%s\n' '--- summary.md ---'
  line=''
  while IFS= read -r line || [ -n "$line" ]; do
    printf '%s\n' "$line"
  done < "$out_dir/summary.md"
fi

exit "$status"
