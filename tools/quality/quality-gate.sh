#!/usr/bin/env bash
# G-QUALITY strict gate and debt ratchet (ADR 0005).
#
# The strict complexity budgets (50 lines, 5 arguments, 10 cognitive
# complexity, 150 type complexity) live here, not in clippy.toml, because a
# configured threshold fires at deny level inside the pedantic-deny crates and
# breaks every historical violation at once. This script swaps in a strict
# config for the measurement, restores the real one afterwards, and compares
# the measured debt against the checked-in baseline:
#
#   tools/quality/quality-gate.sh check   fail on any debt growth (global and per file)
#   tools/quality/quality-gate.sh rebase  record the current debt as the baseline
#
# `check` enforces two ratchets: no lint total may grow, and no file may grow
# beyond its per-lint baseline. With FULL_TOUCHED=1 (just lint-strict) it
# additionally requires every file changed against BASE_REF to be debt-free.
# Existing debt is tolerated exactly where it already lives; new debt is not.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

BASE_REF="${BASE_REF:-origin/main}"
if [[ -n "${GITHUB_BASE_REF:-}" ]]; then
    BASE_REF="origin/${GITHUB_BASE_REF}"
fi
# Never fail open: a missing BASE_REF must be loud, not an empty diff.
if ! git rev-parse --verify --quiet "${BASE_REF}^{commit}" >/dev/null 2>&1; then
    echo "::error::BASE_REF '$BASE_REF' does not resolve; export BASE_REF to a reachable ref" >&2
    exit 1
fi
STRICT_CONFIG="$ROOT/tools/quality/clippy-strict.toml"
BASELINE="$ROOT/tools/quality/baseline.json"

# Lints with historical debt: counted, ratcheted, and enforced on touched
# files. Production denies come from the workspace lints in Cargo.toml.
RATCHET_LINTS=(
    clippy::cast_possible_truncation
    clippy::cast_possible_wrap
    clippy::cognitive_complexity
    clippy::doc_markdown
    clippy::duration_suboptimal_units
    clippy::expect_used
    clippy::large_stack_arrays
    clippy::manual_let_else
    clippy::match_same_arms
    clippy::missing_const_for_fn
    clippy::missing_errors_doc
    clippy::needless_collect
    clippy::no_effect_underscore_binding
    clippy::option_if_let_else
    clippy::or_fun_call
    clippy::redundant_clone
    clippy::semicolon_if_nothing_returned
    clippy::significant_drop_tightening
    clippy::similar_names
    clippy::single_char_pattern
    clippy::single_match_else
    clippy::struct_excessive_bools
    clippy::too_many_arguments
    clippy::too_many_lines
    clippy::trivially_copy_pass_by_ref
    clippy::type_complexity
    clippy::unnecessary_literal_bound
    clippy::unnecessary_wraps
    clippy::use_self
    clippy::used_underscore_binding
    clippy::useless_let_if_seq
    clippy::zero_sized_map_values
    clippy::if_not_else
    clippy::panic
    clippy::todo
    clippy::unimplemented
    clippy::unwrap_used
    clippy::dbg_macro
    clippy::float_cmp
    clippy::missing_panics_doc
)

usage() {
    echo "usage: quality-gate.sh check|rebase" >&2
    exit 2
}

measure() {
    # Swap the strict config in, run one clippy pass over production targets,
    # and restore immediately, in this same shell. No traps: a trap-based
    # restore silently fails when measure() runs inside a command
    # substitution and left the strict config behind once already.
    local had_config=0
    if [[ -f clippy.toml ]]; then
        had_config=1
        mv clippy.toml clippy.toml.quality-bak
    fi
    cp "$STRICT_CONFIG" clippy.toml

    local flags=()
    for lint in "${RATCHET_LINTS[@]}"; do
        flags+=(-A "$lint" -W "$lint")
    done
    # Never leave the strict config behind: restore even when clippy itself
    # fails (compile error, deny, missing toolchain). `set -e` would abort
    # the script before the restore otherwise.
    local output status
    output="$(cargo clippy --locked --workspace --lib --bins --all-features \
        --message-format json -- "${flags[@]}" 2>&1)"
    status=$?

    rm -f clippy.toml
    if [[ $had_config -eq 1 ]]; then
        mv clippy.toml.quality-bak clippy.toml
    fi
    if [[ $status -ne 0 ]]; then
        echo "clippy failed (exit $status):" >&2
        printf '%s\n' "$output" | tail -20 >&2
        exit "$status"
    fi
    printf '%s\n' "$output" | python3 "$ROOT/tools/quality/report.py" "$ROOT"
}

rebase() {
    measure | python3 -c '
import json, sys
# {lint: {file: count}} so the per-file ratchet can detect new debt precisely.
counts = {}
for line in sys.stdin:
    item = json.loads(line)
    lint = counts.setdefault(item["lint"], {})
    lint[item["file"]] = lint.get(item["file"], 0) + 1
with open(sys.argv[1], "w") as f:
    json.dump(counts, f, indent=2, sort_keys=True)
    f.write("\n")
print(f"baseline written: {sys.argv[1]}")
' "$BASELINE"
}

check() {
    if [[ ! -f "$BASELINE" ]]; then
        echo "::error::baseline missing: run tools/quality/quality-gate.sh rebase" >&2
        exit 1
    fi
    local measurements
    measurements="$(measure)"
    local changed
    changed="$(git diff --name-only "$BASE_REF"...HEAD -- '*.rs')"

    CHANGED_FILES="$changed" python3 "$ROOT/tools/quality/compare.py" "$BASELINE" <<<"$measurements"
    echo "G-QUALITY: PASS"
}

case "${1:-}" in
    check) check ;;
    rebase) rebase ;;
    *) usage ;;
esac
