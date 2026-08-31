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
#   tools/quality/quality-gate.sh check   fail on any growth or touched-file debt
#   tools/quality/quality-gate.sh rebase  record the current debt as the baseline
#
# The touched-files rule: every Rust file changed against BASE_REF (default
# origin/main) must carry zero diagnostics from the strict set. Existing debt
# in untouched files is tolerated; touched files must improve.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

BASE_REF="${BASE_REF:-origin/main}"
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
    # and restore. Output: one JSON object per diagnostic on stdout.
    if [[ -f clippy.toml ]]; then mv clippy.toml clippy.toml.quality-bak; fi
    trap 'if [[ -f clippy.toml.quality-bak ]]; then mv clippy.toml.quality-bak clippy.toml; fi' EXIT
    cp "$STRICT_CONFIG" clippy.toml

    local flags=()
    for lint in "${RATCHET_LINTS[@]}"; do
        flags+=(-A "$lint" -W "$lint")
    done
    cargo clippy --locked --workspace --lib --bins --all-features \
        --message-format json -- "${flags[@]}" 2>/dev/null \
        | python3 "$ROOT/tools/quality/report.py" "$ROOT"
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
    changed="$(git diff --name-only "$BASE_REF"...HEAD -- '*.rs' 2>/dev/null || true)"

    CHANGED_FILES="$changed" python3 "$ROOT/tools/quality/compare.py" "$BASELINE" <<<"$measurements"
    echo "G-QUALITY: PASS"
}

case "${1:-}" in
    check) check ;;
    rebase) rebase ;;
    *) usage ;;
esac
