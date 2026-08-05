#!/bin/sh
# run-tests.sh — fixture-driven tests for detect-unmanaged-nix.sh (Spike S1).
#
# The detector is READ-ONLY. THIS harness intentionally creates/mutates/deletes,
# but ONLY inside its own verified mktemp scratch tree. It:
#   * creates the suite with an explicit portable template under TMPDIR:
#       mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX"
#     then marks it via fx_init_suite (which verifies the name, the empty dir, the
#     canonical parent == TMPDIR, an ALLOWLISTED canonical TMPDIR parent
#     (/tmp, /private/tmp, /var/tmp, /private/var/tmp, or macOS
#     /private/var/folders/*/*/T), and refuses protected/system roots BEFORE
#     writing a per-run capability token into the sentinel);
#   * runs the detector against each fixture and asserts exit code + output;
#   * exports NIX_*/IN_NIX_SHELL only for the dedicated env cases;
#   * on cleanup, calls fx_cleanup_suite (the ONLY sanctioned recursive-cleanup
#     path), which re-verifies canonical SCRATCH == FX_SUITE_ROOT AND sentinel
#     content == the per-run token before any chmod/rm, so cleanup can never touch
#     an unrelated path; every primitive also runs fx_guard_chain so a fixture can
#     never write THROUGH a symlink planted inside a case dir;
#   * never invokes sudo and never writes outside the scratch dir.
#
# Run:   sh spikes/s1-store-prefix/run-tests.sh
# Exit:  0 if all cases pass, 1 otherwise.

set -eu

# When invoked under zsh, behave POSIX-sh-like so unmatched globs are left
# literal (not errors) and word splitting matches dash/bash. The detector itself
# always runs under /bin/sh via its shebang, so it needs no shim.
if [ -n "${ZSH_VERSION:-}" ]; then
    emulate -L sh
fi

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DETECT="$SCRIPT_DIR/detect-unmanaged-nix.sh"
# shellcheck source=build-fixtures.sh
. "$SCRIPT_DIR/build-fixtures.sh"

PASS=0
FAIL=0
SKIP=0
FAILED_CASES=""

# mktemp -d with the explicit pkg-s1. template directly under TMPDIR (default /tmp).
# No predictable mkdir fallback. Fail hard if mktemp is missing.
SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX" 2>/dev/null) || { echo 'run-tests: mktemp -d failed; refusing to run' >&2; exit 1; }
fx_init_suite "$SCRATCH"

cleanup() {
    # Delegate to the ONLY sanctioned recursive-cleanup path, which re-verifies
    # canonical SCRATCH == FX_SUITE_ROOT AND exact sentinel-token match before any
    # chmod/rm. Do not duplicate the guard here.
    [ -n "${SCRATCH:-}" ] || return 0
    fx_cleanup_suite "$SCRATCH" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# A minimal, hermetic, Nix-free environment for non-env test runs (so a dev's
# stray NIX_*/IN_NIX_SHELL never turns a clean case into a refusal).
clean_env() {
    env -i PATH="$PATH" HOME="${HOME:-}" USER="${USER:-}" SHELL="${SHELL:-}" TZ="${TZ:-}" "$@"
}

# run_case NAME EXPECT_RC EXPECT_SUBSTR
# Builds fixture NAME into a fresh case dir, runs detector, checks rc + substr.
run_case() {
    rc_name=$1; rc_expect=$2; rc_substr=$3
    rc_dir="$SCRATCH/$rc_name"
    mkdir -p "$rc_dir"
    "make_$rc_name" "$rc_dir"
    set +e
    rc_out=$(clean_env "$DETECT" --root "$rc_dir" 2>&1)
    rc=$?
    set -e
    rc_ok=1
    [ "$rc" -eq "$rc_expect" ] || rc_ok=0
    if [ -n "$rc_substr" ]; then
        case "$rc_out" in
            *"$rc_substr"*) ;;
            *) rc_ok=0 ;;
        esac
    fi
    _report "$rc_name" "$rc" "$rc_expect" "$rc_substr" "$rc_ok" "$rc_out"
}

# run_case_env NAME ENV_ASSIGNMENTS EXPECT_RC EXPECT_SUBSTR SECRET_MUST_BE_ABSENT
#   ENV_ASSIGNMENTS is passed to `env` (word-split intentionally); SECRET is a
#   token that must NOT appear anywhere in the output (proves values are redacted).
#   Uses a real fixture dir from make_NAME.
# shellcheck disable=SC2086
run_case_env() {
    re_name=$1; re_env=$2; re_expect=$3; re_substr=$4; re_secret=$5
    re_dir="$SCRATCH/$re_name"
    mkdir -p "$re_dir"
    "make_$re_name" "$re_dir"
    set +e
    re_out=$(env $re_env "$DETECT" --root "$re_dir" 2>&1)
    re=$?
    set -e
    re_ok=1
    [ "$re" -eq "$re_expect" ] || re_ok=0
    if [ -n "$re_substr" ]; then
        case "$re_out" in *"$re_substr"*) ;; *) re_ok=0 ;; esac
    fi
    if [ -n "$re_secret" ]; then
        case "$re_out" in *"$re_secret"*) re_ok=0 ;; esac
    fi
    _report "$re_name" "$re" "$re_expect" "$re_substr" "$re_ok" "$re_out"
}

_report() {
    _name=$1; _rc=$2; _expect=$3; _substr=$4; _ok=$5; _out=$6
    if [ "$_ok" -eq 1 ]; then
        PASS=$((PASS + 1))
        printf '  PASS  %-38s rc=%d (expected %d)\n' "$_name" "$_rc" "$_expect"
    else
        FAIL=$((FAIL + 1))
        FAILED_CASES="$FAILED_CASES $_name"
        printf '  FAIL  %-38s rc=%d (expected %d)\n' "$_name" "$_rc" "$_expect"
        [ -n "$_substr" ] && printf '        expected substring: %s\n' "$_substr"
        printf '%s\n' "$_out" | sed 's/^/        | /' | head -20
    fi
}

# assert_ok NAME OK DETAIL — record pass/fail for a boolean content/behavior check.
assert_ok() {
    ao_name=$1; ao_ok=$2; ao_detail=$3
    if [ "$ao_ok" -eq 1 ]; then
        PASS=$((PASS + 1))
        printf '  PASS  %-38s %s\n' "$ao_name" "$ao_detail"
    else
        FAIL=$((FAIL + 1))
        FAILED_CASES="$FAILED_CASES $ao_name"
        printf '  FAIL  %-38s %s\n' "$ao_name" "$ao_detail"
    fi
}

# cap_rc NAME RC — record a capability refusal (RC must be nonzero).
cap_rc() {
    cr_name=$1; cr_rc=$2
    assert_ok "$cr_name" "$([ "$cr_rc" -ne 0 ] && echo 1 || echo 0)" "rc=$cr_rc (refused before init/mutation)"
}

# expect_rc NAME CMD...  — run an arbitrary command, assert exit code.
expect_rc() {
    er_name=$1; er_expect=$2; shift 2
    set +e
    er_out=$("$@" 2>&1); er=$?; set -e
    er_ok=1
    [ "$er" -eq "$er_expect" ] || er_ok=0
    if [ "$er_ok" -eq 1 ]; then
        PASS=$((PASS + 1))
        printf '  PASS  %-38s rc=%d (expected %d)\n' "$er_name" "$er" "$er_expect"
    else
        FAIL=$((FAIL + 1))
        FAILED_CASES="$FAILED_CASES $er_name"
        printf '  FAIL  %-38s rc=%d (expected %d)\n' "$er_name" "$er" "$er_expect"
        printf '%s\n' "$er_out" | sed 's/^/        | /' | head -12
    fi
}

# json_parse OK|SKIP — read stdin; validate with jq or python3 if present.
#   Returns 0 if valid, 77 if no parser (caller treats as SKIP).
json_parse() {
    jp_in=$(cat)
    if command -v jq >/dev/null 2>&1; then
        printf '%s' "$jp_in" | jq -e '.' >/dev/null 2>&1 && return 0 || return 1
    elif command -v python3 >/dev/null 2>&1; then
        printf '%s' "$jp_in" | python3 -c 'import json,sys; json.load(sys.stdin)' >/dev/null 2>&1 && return 0 || return 1
    else
        return 77
    fi
}

echo "=== Spike S1 detector fixture tests (scratch=$SCRATCH) ==="

# Required acceptance cases + extras. Install/preflight only (no runtime mode).
# profile_only now has ONLY a spaced user dir (no ordinary path can satisfy it).
run_case clean                         0 ""
run_case existing_install_linux        2 "NIX_STORE_POPULATED"
run_case existing_install_macos        2 "LAUNCHD_PLIST"
run_case linux_service                 2 "SYSTEMD_UNIT"
run_case macos_launchd                 2 "LAUNCHD_PLIST"
run_case macos_apfs_synthetic_fstab    2 "SYNTHETIC_CONF_NIX"
run_case symlink_mount                 2 "NIX_ROOT_SYMLINK"
run_case nix_on_path                   2 "ROOT_BINARY"
run_case db_and_socket                 2 "NIX_DAEMON_SOCKET"
run_case profile_only                  2 "NIX_PROFILE_FILES"
run_case product_marker_only           2 "PKG_MARKER"

# Unreadable-state handling depends on not running as root.
if [ "$(id -u)" -eq 0 ]; then
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (running as root: unreadable is not meaningful)\n' ambiguous_unreadable
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (running as root: unreadable is not meaningful)\n' marker_unreadable
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (running as root: unreadable is not meaningful)\n' group_unreadable
else
    run_case ambiguous_unreadable       2 "NIX_ROOT_UNREADABLE"
    run_case marker_unreadable          2 "MARKER_UNREADABLE"
    run_case group_unreadable           2 "GROUP_UNREADABLE"
fi

echo
echo "=== Uninspectable scan root (mode 000 root -> ROOT_UNINSPECTABLE) ==="
# A root that EXISTS but cannot be entered/read/searched must NOT silently scan
# CLEAN: validate_root sets ROOT_INSPECTABLE=0, main() records ROOT_UNINSPECTABLE
# AFTER emit_open (JSON schema preserved) and skips all scans. Skipped as root
# (mode 000 is readable to root). JSON variant parsed where a parser exists.
if [ "$(id -u)" -eq 0 ]; then
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (running as root: unreadable not meaningful)\n' root_uninspectable
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (running as root: unreadable not meaningful)\n' root_uninspectable_json
else
    ru_dir="$SCRATCH/root_uninspectable"
    mkdir -p "$ru_dir"
    chmod 0000 "$ru_dir"
    set +e
    ru_out=$(clean_env "$DETECT" --root "$ru_dir" 2>&1); ru_rc=$?
    set -e
    chmod 0700 "$ru_dir" 2>/dev/null || true   # restore so cleanup can recurse
    ru_ok=1
    [ "$ru_rc" -eq 2 ] || ru_ok=0
    case "$ru_out" in *"ROOT_UNINSPECTABLE"*) ;; *) ru_ok=0 ;; esac
    assert_ok root_uninspectable "$ru_ok" "rc=$ru_rc (mode 000 root -> ROOT_UNINSPECTABLE)"
    # JSON variant: valid schema, result=refuse, parses where a parser exists.
    chmod 0000 "$ru_dir"
    set +e
    ru_jout=$(clean_env "$DETECT" --root "$ru_dir" --json 2>/dev/null); ru_jrc=$?
    set -e
    chmod 0700 "$ru_dir" 2>/dev/null || true
    ru_jok=1
    [ "$ru_jrc" -eq 2 ] || ru_jok=0
    case "$ru_jout" in *"ROOT_UNINSPECTABLE"*) ;; *) ru_jok=0 ;; esac
    case "$ru_jout" in *'"result":"refuse"'*) ;; *) ru_jok=0 ;; esac
    ru_jp=$(printf '%s' "$ru_jout" | json_parse; echo "rc=$?")
    case "$ru_jp" in
        *rc=77) SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (no JSON parser available)\n' root_uninspectable_json ;;
        *rc=0)  assert_ok root_uninspectable_json "$ru_jok" "rc=$ru_jrc (json=refuse ROOT_UNINSPECTABLE)" ;;
        *)      FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES root_uninspectable_json"
                printf '  FAIL  %-38s json parse failed\n' root_uninspectable_json ;;
    esac
fi

echo
echo "=== Spaced CASE ROOT (path itself contains a space) ==="
# A case whose ROOT PATH contains a space (not only the user name), proving the
# detector and the whitespace-safe home enumeration handle it end to end.
sp_name=profile_spaced_root
sp_dir="$SCRATCH/has space in name"
mkdir -p "$sp_dir"
make_profile_only "$sp_dir"
set +e
sp_out=$(clean_env "$DETECT" --root "$sp_dir" 2>&1); sp_rc=$?; set -e
sp_ok=1
[ "$sp_rc" -eq 2 ] || sp_ok=0
case "$sp_out" in *"NIX_PROFILE_FILES"*) ;; *) sp_ok=0 ;; esac
assert_ok "$sp_name" "$sp_ok" "rc=$sp_rc (spaced root + spaced user dir -> NIX_PROFILE_FILES)"

echo
echo "=== Environment detection (presence-only; names, counts, and values redacted) ==="
# NIX_CONFIG, NIX_REMOTE_SYSTEMS, an arbitrary NIX_FUTURE_VARIABLE, an EMPTY
# NIX_FUTURE_VARIABLE, and IN_NIX_SHELL each cause refusal; the signal ID is
# emitted but NO name/count/value text. Secret values must be absent.
run_case_env clean "NIX_CONFIG=build-users-group=nixbld"                  2 ENV_NIX_VAR        "build-users-group"
run_case_env clean "NIX_REMOTE_SYSTEMS=ssh://example/builder"            2 ENV_NIX_VAR        "ssh://example"
run_case_env clean "NIX_FUTURE_VARIABLE=arbitrary-future-value"          2 ENV_NIX_VAR        "arbitrary-future-value"
run_case_env clean "NIX_FUTURE_VARIABLE="                                2 ENV_NIX_VAR        ""
run_case_env clean "IN_NIX_SHELL=pure"                                   2 ENV_IN_NIX_SHELL   "pure"
# Multiple NIX_* vars exported together are still refused; NO name or value leaks.
run_case_env clean "NIX_PATH=/nix/var/nix NIX_PROFILES=/nix/var/nix/profiles/default NIX_REMOTE=ZZSECRET-DAEMON-VAL" 2 ENV_NIX_VAR "ZZSECRET-DAEMON-VAL"

# Regression (overwatch finding): a NON-NIX variable whose multiline VALUE
# contains a newline + a fake `NIX_SECRET_FROM_VALUE=...` line. The line-oriented
# POSIX `env` serialization cannot distinguish this from a real assignment, so the
# detector CONSERVATIVELY refuses (an honest, stated false-positive). What it must
# NOT do is echo the injected fake NAME or the secret value text — in text OR JSON.
ml_name=env_multiline_no_name_leak
ml_dir="$SCRATCH/$ml_name"
mkdir -p "$ml_dir"
make_clean "$ml_dir"
ml_secret_val='ZZSECRET-VALUE-TEXT'
ml_fake_name='NIX_SECRET_FROM_VALUE'
# NON_NIX_VAR's value embeds a line that looks exactly like a NIX_ assignment.
ml_assign="NON_NIX_VAR=benign-first-line
${ml_fake_name}=${ml_secret_val}"
set +e
ml_out=$(env "$ml_assign" "$DETECT" --root "$ml_dir" 2>&1); ml_rc=$?
set -e
ml_ok=1
# Conservative refusal (rc=2) is the expected/accepted behavior here.
[ "$ml_rc" -eq 2 ] || ml_ok=0
case "$ml_out" in *"$ml_fake_name"*) ml_ok=0 ;; esac     # injected fake NAME must not appear
case "$ml_out" in *"$ml_secret_val"*) ml_ok=0 ;; esac   # secret VALUE text must not appear
case "$ml_out" in *"names:"*) ml_ok=0 ;; esac           # no name-list echo at all
assert_ok "$ml_name" "$ml_ok" "rc=$ml_rc (refused; no injected name/secret in text)"
# JSON variant: same guarantees, plus parse-valid where a parser exists.
set +e
ml_jout=$(env "$ml_assign" "$DETECT" --root "$ml_dir" --json 2>/dev/null); ml_jrc=$?
set -e
ml_jok=1
[ "$ml_jrc" -eq 2 ] || ml_jok=0
case "$ml_jout" in *"$ml_fake_name"*) ml_jok=0 ;; esac
case "$ml_jout" in *"$ml_secret_val"*) ml_jok=0 ;; esac
ml_jp=$(printf '%s' "$ml_jout" | json_parse; echo "rc=$?")
case "$ml_jp" in
    *rc=77) SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (no JSON parser available)\n' "${ml_name}_json" ;;
    *rc=0)  assert_ok "${ml_name}_json" "$ml_jok" "rc=$ml_jrc (no injected name/secret in JSON; parses)" ;;
    *)      FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES ${ml_name}_json"
            printf '  FAIL  %-38s json parse failed\n' "${ml_name}_json" ;;
esac

echo
echo "=== JSON shape + hostile-input robustness (parser-backed) ==="
jdir="$SCRATCH/json_smoke"
mkdir -p "$jdir"
make_existing_install_linux "$jdir"
set +e
jout=$(clean_env "$DETECT" --root "$jdir" --json 2>/dev/null); jrc=$?; set -e
if [ "$jrc" -eq 2 ] && printf '%s' "$jout" | grep -q '"result":"refuse"' && printf '%s' "$jout" | grep -q '"findings":\['; then
    PASS=$((PASS + 1)); printf '  PASS  %-38s rc=%d json=refuse\n' "json_smoke" "$jrc"
else
    FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES json_smoke"
    printf '  FAIL  %-38s rc=%d\n' "json_smoke" "$jrc"
    printf '%s\n' "$jout" | sed 's/^/        | /' | head -10
fi

# Hostile environment values must (a) cause refusal, (b) never leak into output,
# and (c) not break JSON. Detection is presence-only: no env names, counts, or
# values (nor value-derived text) are echoed.
jh_name=json_hostile_env
jh_dir="$SCRATCH/$jh_name"
mkdir -p "$jh_dir"
make_clean "$jh_dir"
# A literal tab and newline inside the hostile value, plus quotes/backslash/control.
jh_secret='ZZLEAKZZ'
set +e
jh_out=$(env \
    "NIX_HOSTILE=pre	$(printf 'x\ny')\"z\\w	" \
    NIX_PATH='$('"$jh_secret"'rm -rf /)' \
    NIX_NEWLINE="$(printf 'a\nb')" \
    IN_NIX_SHELL=1 \
    "$DETECT" --root "$jh_dir" --json 2>/dev/null)
jh_rc=$?
set -e
jh_ok=1
[ "$jh_rc" -eq 2 ] || jh_ok=0
case "$jh_out" in *"$jh_secret"*) jh_ok=0 ;; esac           # value leaked -> fail
case "$jh_out" in *"rm -rf"*) jh_ok=0 ;; esac              # hostile payload leaked
# Validate the JSON parses (parser-backed; SKIP only if no parser available).
jp=$(printf '%s' "$jh_out" | json_parse; echo "rc=$?")
case "$jp" in
    *rc=77) SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (no JSON parser available)\n' "$jh_name (parse)";;
    *rc=0)  ;;
    *)      jh_ok=0 ;;
esac
if [ "$jh_ok" -eq 1 ]; then
    PASS=$((PASS + 1)); printf '  PASS  %-38s rc=%d json=valid values=redacted\n' "$jh_name" "$jh_rc"
else
    FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES $jh_name"
    printf '  FAIL  %-38s rc=%d parse=%s\n' "$jh_name" "$jh_rc" "$jp"
    printf '%s\n' "$jh_out" | sed 's/^/        | /' | head -12
fi

# A symlink whose TARGET contains quotes/backslash, plus a profile file under a
# spaced home — all get scanned but only IDs/redacted text enter JSON. (No hostile
# FILENAME is created here; only a hostile symlink TARGET string.)
jh2_name=json_hostile_fs
jh2_dir="$SCRATCH/$jh2_name"
mkdir -p "$jh2_dir"
make_existing_install_macos "$jh2_dir"
fx_dir  "$jh2_dir" "home/Spaced User"
fx_symlink "$jh2_dir" "home/Spaced User/.nix-profile" '/nix/var/nix/"evil"'
set +e
jh2_out=$(clean_env "$DETECT" --root "$jh2_dir" --json 2>/dev/null); jh2_rc=$?; set -e
jp2=$(printf '%s' "$jh2_out" | json_parse; echo "rc=$?")
jh2_ok=1
[ "$jh2_rc" -eq 2 ] || jh2_ok=0
case "$jh2_out" in *'"evil"'*) jh2_ok=0 ;; esac
case "$jp2" in *rc=77) SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (no JSON parser available)\n' "$jh2_name (parse)";; *rc=0) ;; *) jh2_ok=0 ;; esac
if [ "$jh2_ok" -eq 1 ]; then
    PASS=$((PASS + 1)); printf '  PASS  %-38s rc=%d json=valid\n' "$jh2_name" "$jh2_rc"
else
    FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES $jh2_name"
    printf '  FAIL  %-38s rc=%d parse=%s\n' "$jh2_name" "$jh2_rc" "$jp2"
    printf '%s\n' "$jh2_out" | sed 's/^/        | /' | head -12
fi

echo
echo "=== Usage / safety guards ==="
# Unsafe roots rejected.
expect_rc reject_root_nix          64 "$DETECT" --root /nix
expect_rc reject_root_etc_nix      64 "$DETECT" --root /etc/nix
expect_rc reject_relative          64 "$DETECT" --root relative/dir
expect_rc reject_nonexistent       64 "$DETECT" --root "$SCRATCH/does-not-exist"
# Bare --root (no value) => 64 with a clear error.
expect_rc reject_bare_root         64 "$DETECT" --root
# Removed --mode (any value) and bare --mode => 64.
expect_rc reject_mode_install      64 "$DETECT" --mode install --root "$SCRATCH/clean"
expect_rc reject_mode_runtime      64 "$DETECT" --mode runtime --root "$SCRATCH/clean"
expect_rc reject_mode_bare         64 "$DETECT" --mode
# Unknown argument => 64.
expect_rc reject_unknown_arg       64 "$DETECT" --bogus --root "$SCRATCH/clean"
# A non-"/" symlink passed as --root is rejected (do not follow into real fs).
sym="$SCRATCH/symroot_to_clean"
ln -s "$SCRATCH/clean" "$sym"
expect_rc reject_symlink_root      64 "$DETECT" --root "$sym"
# Traversal alias: a root containing ".." is rejected before any scan (no real
# /nix is touched). A macOS /var/folders temp root must still be ACCEPTED (the
# clean case above already proves a temp-rooted dir scans as CLEAN).
expect_rc reject_traversal_dotdot  64 "$DETECT" --root "$SCRATCH/clean/../clean"

echo
echo "=== Execution guard (symlink/renamed binary still runs main) ==="
# main is unconditional: a differently-named symlink to the detector must STILL
# run main (the basename == "detect-unmanaged-nix.sh" guard was removed after
# overwatch proved a renamed copy silently exited 0). The symlink is created SAFELY
# inside the verified scratch tree, and both `sh <symlink>` and exec'ing the
# symlink directly must still reject --root /nix with 64 (executable security
# behavior must not depend on basename).
sg_link="$SCRATCH/not-the-detector-name"
ln -s "$DETECT" "$sg_link"
expect_rc symlink_invocation_runs_main_sh   64 sh "$sg_link" --root /nix
expect_rc symlink_invocation_runs_main_exec 64 "$sg_link" --root /nix

echo
echo "=== Fixture-library guard ==="
# make_<case> / must fail BEFORE any mutation (no matching capability / protected
# root). Run the make_* calls in SUBSHELLS: fx_guard_root uses `exit 64` (correct
# standalone behavior) which would otherwise exit the whole harness.
set +e
( make_existing_install_linux / ) >/dev/null 2>&1; g1=$?
( make_clean ./relative ) >/dev/null 2>&1; g2=$?
( make_clean "$SCRATCH/no-such-case-xyz" ) >/dev/null 2>&1; g3=$?
set -e
if [ "$g1" -ne 0 ]; then
    PASS=$((PASS + 1)); printf '  PASS  %-38s rc=%d (refused before mutation)\n' fixture_guard_root_slash "$g1"
    # Confirm "/" was not mutated: /nix/store from the fixture must NOT exist.
    if [ -e /nix/store/0c2a7m9x4y3b2c1d0e9f8a7b6c5d4e3f-hello-2.12.1 ]; then
        FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES fixture_guard_root_slash(MUTATED!)"
        printf '  FAIL  fixture guard: /nix/store was MUTATED by make_existing_install_linux /\n'
    fi
else
    FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES fixture_guard_root_slash"
    printf '  FAIL  fixture_guard_root_slash rc=%d (expected non-zero)\n' "$g1"
fi
if [ "$g2" -ne 0 ] && [ "$g3" -ne 0 ]; then
    PASS=$((PASS + 1)); printf '  PASS  %-38s relative+missing refused\n' fixture_guard_relmiss
else
    FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES fixture_guard_relmiss"
    printf '  FAIL  fixture_guard_relmiss relative=%d missing=%d (expected both non-zero)\n' "$g2" "$g3"
fi

echo
echo "=== Fixture-suite capability regressions (fresh subshells) ==="
# Each case runs in a fresh subshell that sources build-fixtures and calls
# fx_init_suite / a primitive with a bad setup; it MUST exit nonzero BEFORE
# writing/creating anything.

# / : if a sentinel somehow pre-exists at /, refuse the test WITHOUT deleting it.
if [ -e "/$FX_SENTINEL" ] || [ -L "/$FX_SENTINEL" ]; then
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (pre-existing sentinel at /; not touching it)\n' cap_root_sentinel_preexists
else
    set +e
    ( . "$SCRIPT_DIR/build-fixtures.sh" && fx_init_suite / ) >/dev/null 2>&1; cr1=$?
    set -e
    cap_rc cap_init_refuses_slash "$cr1"
    # Confirm NO sentinel was created at /.
    if [ -e "/$FX_SENTINEL" ] || [ -L "/$FX_SENTINEL" ]; then
        FAIL=$((FAIL + 1)); FAILED_CASES="$FAILED_CASES cap_created_sentinel_at_root"
        printf '  FAIL  cap_created_sentinel_at_root: a sentinel was created at /\n'
    else
        PASS=$((PASS + 1)); printf '  PASS  %-38s no sentinel at / after attempt\n' cap_no_sentinel_at_root
    fi
fi

# /etc (protected root) and a normal repository-like directory (not a direct
# child of TMPDIR) must be refused before init.
mkdir -p "$SCRATCH/repolike"; : > "$SCRATCH/repolike/README.md"
set +e
( . "$SCRIPT_DIR/build-fixtures.sh" && fx_init_suite /etc ) >/dev/null 2>&1; cr2=$?
( . "$SCRIPT_DIR/build-fixtures.sh" && fx_init_suite "$SCRATCH/repolike" ) >/dev/null 2>&1; cr3=$?
set -e
cap_rc cap_init_refuses_etc "$cr2"
cap_rc cap_init_refuses_repo_dir "$cr3"

# A wrong-prefix temp dir directly under TMPDIR must be refused (basename check),
# and must NOT gain a (real) sentinel.
cr_wrong=$(mktemp -d "${TMPDIR:-/tmp}/wrong.XXXXXXXX" 2>/dev/null) || cr_wrong=
if [ -n "$cr_wrong" ]; then
    set +e
    ( . "$SCRIPT_DIR/build-fixtures.sh" && fx_init_suite "$cr_wrong" ) >/dev/null 2>&1; cr4=$?
    set -e
    cap_rc cap_init_refuses_wrong_prefix "$cr4"
    # The dir stays empty (init refused before writing a sentinel). Confirm no
    # sentinel was created, then remove it with an EXACT rmdir (no recursive rm).
    if [ ! -e "$cr_wrong/$FX_SENTINEL" ] && [ ! -L "$cr_wrong/$FX_SENTINEL" ]; then
        rmdir "$cr_wrong" 2>/dev/null || true
    fi
else
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (mktemp failed)\n' cap_init_refuses_wrong_prefix
fi

# A hand-planted CONSTANT sentinel must NOT authorize mutation.
# (a) No capability initialized at all -> a primitive refuses even though a
#     sentinel file is present (sentinel existence alone is insufficient). Planted
#     UNDER the main verified suite (SCRATCH) so its cleanup is the suite cleanup
#     (no separate rm). A nested subshell unsets the capability before the call.
hp_a="$SCRATCH/hp_no_cap"
mkdir -p "$hp_a/c"
printf '%s\n' "PKG_S1_FIXTURE_SUITE_HANDPLANTED_CONSTANT" > "$hp_a/$FX_SENTINEL"
set +e
( unset FX_SUITE_ROOT FX_TOKEN; . "$SCRIPT_DIR/build-fixtures.sh"; fx_dir "$hp_a/c" "x" ) >/dev/null 2>&1; hp1=$?
set -e
cap_rc cap_no_capability_refuses_planted "$hp1"

# (b) Capability initialized on a valid suite, then that suite's sentinel is
#     overwritten with a CONSTANT -> a primitive on a valid case under it refuses
#     (per-run token mismatch). This needs a real fx_init_suite, so it stays a
#     direct child of TMPDIR; the subshell restores the original token and calls
#     fx_cleanup_suite so no temp dir is left.
hp_b=$(mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX" 2>/dev/null) || hp_b=
if [ -n "$hp_b" ]; then
    set +e
    (
        . "$SCRIPT_DIR/build-fixtures.sh"
        fx_init_suite "$hp_b"
        mkdir -p "$hp_b/c"
        printf '%s\n' "HANDPLANTED_CONSTANT" > "$hp_b/$FX_SENTINEL"
        # Run the refusing primitive in a NESTED subshell so its exit 64 does not
        # abort the token restore + sanctioned cleanup below.
        ( fx_dir "$hp_b/c" "x" ) >/dev/null 2>&1; hp2_rc=$?
        # Restore the original per-run token, then remove via fx_cleanup_suite.
        printf '%s\n' "$FX_TOKEN" > "$hp_b/$FX_SENTINEL"
        fx_cleanup_suite "$hp_b"
        exit "$hp2_rc"
    ) >/dev/null 2>&1; hp2=$?
    set -e
    cap_rc cap_tampered_sentinel_refused "$hp2"
else
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (mktemp failed)\n' cap_tampered_sentinel_refused
fi

# (c) A NESTED directory under the valid suite, carrying a hand-planted constant
#     sentinel: a primitive on a case under the nested dir refuses because the
#     case's canonical parent is the nested dir, not the suite root.
mkdir -p "$SCRATCH/nested/c"
printf '%s\n' "NESTED_CONSTANT" > "$SCRATCH/nested/$FX_SENTINEL"
set +e
( fx_dir "$SCRATCH/nested/c" "x" ) >/dev/null 2>&1; hp3=$?
set -e
cap_rc cap_nested_planted_sentinel_refused "$hp3"

echo
echo "=== Missing-argument regressions (set -u safe) ==="
# fx_init_suite / fx_cleanup_suite must handle a MISSING argument via a safe
# default-empty expansion: a documented nonzero status + message, NOT an
# "unbound variable" crash under `set -u` (run-tests runs under `set -eu`).

# fx_init_suite with NO argument -> exit 64, documented message, no unbound crash.
set +e
mi_out=$( ( . "$SCRIPT_DIR/build-fixtures.sh"; fx_init_suite ) 2>&1 ); mi_rc=$?
set -e
mi_ok=1
[ "$mi_rc" -eq 64 ] || mi_ok=0
case "$mi_out" in *unbound*) mi_ok=0 ;; esac
case "$mi_out" in *"not set"*) mi_ok=0 ;; esac
case "$mi_out" in *"empty argument"*) ;; *) mi_ok=0 ;; esac
assert_ok cap_init_missing_arg "$mi_ok" "rc=$mi_rc (exit 64, no unbound crash, documented msg)"

# fx_cleanup_suite with NO argument (capability initialized) -> nonzero return,
# documented message, no unbound crash. The still-present suite is then removed via
# the sanctioned fx_cleanup_suite path so no temp dir is leaked.
mc_suite=$(mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX" 2>/dev/null) || mc_suite=
if [ -n "$mc_suite" ]; then
    set +e
    mc_out=$( (
        set +e
        . "$SCRIPT_DIR/build-fixtures.sh"
        fx_init_suite "$mc_suite"
        fx_cleanup_suite                # missing arg -> nonzero return, NO removal
        mc_inner=$?
        fx_cleanup_suite "$mc_suite"    # sanctioned cleanup of the still-present suite
        exit "$mc_inner"
    ) 2>&1 ); mc_rc=$?
    set -e
    mc_ok=1
    [ "$mc_rc" -ne 0 ] || mc_ok=0
    case "$mc_out" in *unbound*) mc_ok=0 ;; esac
    case "$mc_out" in *"not set"*) mc_ok=0 ;; esac
    case "$mc_out" in *"empty argument"*) ;; *) mc_ok=0 ;; esac
    assert_ok cap_cleanup_missing_arg "$mc_ok" "rc=$mc_rc (nonzero return, no unbound crash, suite cleaned)"
else
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (mktemp failed)\n' cap_cleanup_missing_arg
fi

echo
echo "=== Missing HOME does not crash (env -i PATH-only) ==="
# The detector runs under `set -eu`; an environment with NO HOME must not crash on
# an unbound HOME. A clean fake root scanned under `env -i PATH=...` (no HOME)
# must yield CLEAN (exit 0) and emit no "unbound"/"not set" error.
nh_dir="$SCRATCH/no_home_clean"
mkdir -p "$nh_dir"
make_clean "$nh_dir"
set +e
nh_out=$(env -i PATH="$PATH" "$DETECT" --root "$nh_dir" 2>&1); nh_rc=$?; set -e
nh_ok=1
[ "$nh_rc" -eq 0 ] || nh_ok=0
case "$nh_out" in *unbound*) nh_ok=0 ;; esac
case "$nh_out" in *"not set"*) nh_ok=0 ;; esac
case "$nh_out" in *"CLEAN"*) ;; *) nh_ok=0 ;; esac
assert_ok no_home_clean "$nh_ok" "rc=$nh_rc (env -i PATH-only; CLEAN, no unbound HOME)"

echo
echo "=== Fixture-relative symlink escape (destination-chain guard) ==="
# Plant a symlink INSIDE a case dir pointing at a sibling also inside the verified
# suite (NOT a real system path). A subsequent fx_file THROUGH that symlink must be
# refused before any mkdir/write, and the sibling target must NOT receive the file.
esc_case="$SCRATCH/escape_case"
esc_target="$SCRATCH/escape_target"
mkdir -p "$esc_case" "$esc_target"
# fx_symlink creates the symlink leaf (capability OK; "escape" does not exist yet).
fx_symlink "$esc_case" "escape" "$esc_target"
# Attempt to write THROUGH the symlink: must be refused by fx_guard_chain.
set +e
( fx_file "$esc_case" "escape/pwn" "pwned" ) >/dev/null 2>&1; esc_rc=$?
set -e
esc_ok=1
[ "$esc_rc" -ne 0 ] || esc_ok=0
# The sibling target must NOT have been written through the symlink.
if [ -e "$esc_target/pwn" ] || [ -L "$esc_target/pwn" ]; then esc_ok=0; fi
assert_ok fixture_symlink_escape_refused "$esc_ok" "rc=$esc_rc (refused; sibling target untouched)"

echo
echo "=== TMPDIR allowlist (user-controlled TMPDIR refused) ==="
# A repo-like TMPDIR under the main scratch with a correctly-named EMPTY pkg-s1
# child must be REFUSED (TMPDIR is not an allowed canonical temp parent) and receive
# NO sentinel. The child stays empty -> removed with an exact rmdir (no recursive rm).
mkdir -p "$SCRATCH/fake_repo"; : > "$SCRATCH/fake_repo/README.md"
td_suite=$(mktemp -d "$SCRATCH/fake_repo/pkg-s1.XXXXXXXX" 2>/dev/null) || td_suite=
if [ -n "$td_suite" ]; then
    set +e
    ( TMPDIR="$SCRATCH/fake_repo" . "$SCRIPT_DIR/build-fixtures.sh" && fx_init_suite "$td_suite" ) >/dev/null 2>&1; td_rc=$?
    set -e
    td_ok=1
    [ "$td_rc" -ne 0 ] || td_ok=0
    if [ -e "$td_suite/$FX_SENTINEL" ] || [ -L "$td_suite/$FX_SENTINEL" ]; then td_ok=0; fi
    assert_ok tmpdir_repo_like_refused "$td_ok" "rc=$td_rc (repo TMPDIR refused; no sentinel)"
    # Confirm no sentinel, then exact rmdir of the empty child (no recursive rm).
    if [ ! -e "$td_suite/$FX_SENTINEL" ] && [ ! -L "$td_suite/$FX_SENTINEL" ]; then
        rmdir "$td_suite" 2>/dev/null || true
    fi
else
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (mktemp failed)\n' tmpdir_repo_like_refused
fi

echo
echo "=== TMPDIR macOS per-user root: exact-component match (pure unit) ==="
# fx_is_macos_tmproot must accept EXACTLY /private/var/folders/<one>/<two>/T (two
# nonempty slash-free components) and reject deeper descendants, which a shell
# `case` glob (`*/*/T`, where `*` spans slashes) would wrongly accept.
tm_ok=1
# accept exact two-component forms:
if fx_is_macos_tmproot /private/var/folders/A/B/T;            then :; else tm_ok=0; fi
if fx_is_macos_tmproot /private/var/folders/xx/yy/T;          then :; else tm_ok=0; fi
if fx_is_macos_tmproot /private/var/folders/Ab-/_cD9/T;       then :; else tm_ok=0; fi
# reject deeper / wrong-shape / empty-component / fixed-root / no-leaf forms:
if fx_is_macos_tmproot /private/var/folders/A/B/C/D/T;        then tm_ok=0; fi
if fx_is_macos_tmproot /private/var/folders/A/B/T/extra;      then tm_ok=0; fi
if fx_is_macos_tmproot /private/var/folders/A/T;              then tm_ok=0; fi
if fx_is_macos_tmproot /private/var/folders//B/T;             then tm_ok=0; fi
if fx_is_macos_tmproot /private/var/folders/A//T;             then tm_ok=0; fi
if fx_is_macos_tmproot /tmp;                                 then tm_ok=0; fi
if fx_is_macos_tmproot /private/tmp;                         then tm_ok=0; fi
if fx_is_macos_tmproot /private/var/folders/A/B;             then tm_ok=0; fi
assert_ok tmpdir_macos_exact_components "$tm_ok" "fx_is_macos_tmproot exact accept + deeper/wrong reject"

echo
echo "=== fx_init_suite fails closed when find fails/unavailable ==="
# A failing `find` must make fx_init_suite FAIL CLOSED (exit 64) BEFORE writing
# the sentinel/capability, leaving the suite dir untouched (still empty). We stub
# `find` with a shell FUNCTION inside the subshell (no second uninitialized temp
# dir, no executable stub), keep ONE fresh auxiliary suite, prove no
# sentinel/mutation, and clean it with an EXACT rmdir (no raw recursive cleanup
# outside fx_cleanup_suite; the suite is untouched/empty so rmdir suffices).
ff_suite=$(mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX" 2>/dev/null) || ff_suite=
if [ -n "$ff_suite" ]; then
    set +e
    ff_out=$( (
        find() { return 1; }   # POSIX function override; shadows the real find in this subshell only
        . "$SCRIPT_DIR/build-fixtures.sh"
        fx_init_suite "$ff_suite"
    ) 2>&1 ); ff_rc=$?
    set -e
    ff_ok=1
    [ "$ff_rc" -eq 64 ] || ff_ok=0
    # No sentinel written (capability never established).
    if [ -e "$ff_suite/$FX_SENTINEL" ] || [ -L "$ff_suite/$FX_SENTINEL" ]; then ff_ok=0; fi
    # The suite dir must still be empty (no mutation): POSIX glob emptiness check.
    ff_empty=1
    for ff_e in "$ff_suite"/* "$ff_suite"/.*; do
        [ -e "$ff_e" ] || [ -L "$ff_e" ] || continue
        case "${ff_e#"$ff_suite"/}" in .|..) continue ;; esac
        ff_empty=0; break
    done
    [ "$ff_empty" -eq 1 ] || ff_ok=0
    assert_ok fx_init_failclosed_missing_find "$ff_ok" "rc=$ff_rc (fail-closed; no sentinel; dir untouched)"
    # Exact cleanup of the single out-of-suite temp suite (no recursive rm; it is
    # untouched/empty so an exact rmdir suffices).
    rmdir "$ff_suite" 2>/dev/null || true
else
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (mktemp failed)\n' fx_init_failclosed_missing_find
fi

echo
echo "=== Symlinked home root (HOME_ROOT_SYMLINK; no traversal) ==="
# A home root that is a symlink to a sibling INSIDE the verified suite (holding a
# Nix artifact) must be recorded HOME_ROOT_SYMLINK and NOT traversed, so the
# detector never claims artifacts from outside the scan root.
hs_case="$SCRATCH/home_symlink_case"
hs_real="$SCRATCH/home_symlink_real"
mkdir -p "$hs_case" "$hs_real"
# Put a Nix artifact in the sibling real home (capability-gated: parent == suite).
fx_symlink "$hs_real" ".nix-profile" "/nix/var/nix/profiles/default"
# Make the case's /home a symlink to that sibling.
fx_symlink "$hs_case" "home" "$hs_real"
set +e
hs_out=$(clean_env "$DETECT" --root "$hs_case" 2>&1); hs_rc=$?
set -e
hs_ok=1
[ "$hs_rc" -eq 2 ] || hs_ok=0
case "$hs_out" in *"HOME_ROOT_SYMLINK"*) ;; *) hs_ok=0 ;; esac
case "$hs_out" in *"NIX_PROFILE_FILES"*) hs_ok=0 ;; esac   # must NOT have traversed
assert_ok home_symlink_root_refused "$hs_ok" "rc=$hs_rc (HOME_ROOT_SYMLINK; no traversal)"

echo
echo "=== Broken (unresolvable) home-root symlink (HOME_ROOT_SYMLINK; not skipped) ==="
# A home root that is a symlink whose target does NOT exist (a broken/unresolvable
# nonstandard /home link) must be recorded HOME_ROOT_SYMLINK and refused — NOT
# silently skipped. The outer presence gate in check_profiles admits directories
# OR symlinks to scan_home_root (a plain `[ -d ]` would follow a symlink and
# require its target to be an existing directory, so a broken symlink would fail
# `-d` and be dropped — the exact contract violation this pins). scan_home_root
# then records HOME_ROOT_SYMLINK without traversing. The existing
# home_symlink_root_refused case above covers a symlink to an EXISTING target;
# this case covers the BROKEN-target branch.
hb_case="$SCRATCH/home_broken_symlink_case"
mkdir -p "$hb_case"
fx_symlink "$hb_case" "home" "$hb_case/does-not-exist-target"
set +e
hb_out=$(clean_env "$DETECT" --root "$hb_case" 2>&1); hb_rc=$?
set -e
hb_ok=1
[ "$hb_rc" -eq 2 ] || hb_ok=0
case "$hb_out" in *"HOME_ROOT_SYMLINK"*) ;; *) hb_ok=0 ;; esac
assert_ok home_broken_symlink_refused "$hb_ok" "rc=$hb_rc (broken /home symlink -> HOME_ROOT_SYMLINK)"

echo
echo "=== macOS standard /home firmlink skip (Darwin real-host; verified target) ==="
# The detector is NO LONGER sourceable without running (main is unconditional
# now), so the former source-based standard_home_policy_unit unit test is replaced
# by this read-only REAL-HOST regression. On Darwin, when /home is the standard
# firmlink (a symlink whose canonical target resolves to
# /System/Volumes/Data/home — VERIFIED by is_standard_macos_home_firmlink, not
# assumed for every real-host Darwin /home), the read-only detector --root / must
# NOT report HOME_ROOT_SYMLINK for it. Other ambiguity (e.g. an unreadable
# /var/root) and exit 2 are acceptable here. The fake-root arbitrary /home symlink
# refusal is still proven end-to-end by home_symlink_root_refused above.
#
# Tighten the TEST CONDITION itself so a deliberately fail-closed NONSTANDARD
# macOS /home cannot cause a FALSE failure here. Assert/execute ONLY when ALL of
# (i) this is a real Darwin host, (ii) /home is a symlink, AND (iii) a POSIX
# cd -P / pwd -P canonicalization of /home (fx_canon) resolves to EXACTLY
# /System/Volumes/Data/home. Otherwise SKIP with truthful wording: non-Darwin,
# /home absent/real-directory, or /home a symlink to a different/unresolvable
# (nonstandard) target — the predicate is macOS-only and this is not the verified
# standard firmlink, so the regression must not assert on it.
shf_home_canon=
if [ -L /home ]; then
    shf_home_canon=$(fx_canon /home) || shf_home_canon=
fi
if [ "$(uname -s)" = "Darwin" ] && [ -L /home ] && [ "$shf_home_canon" = "/System/Volumes/Data/home" ]; then
    set +e
    shf_out=$(clean_env "$DETECT" --root / 2>&1); shf_rc=$?
    set -e
    shf_ok=1
    case "$shf_out" in *HOME_ROOT_SYMLINK*) shf_ok=0 ;; esac
    assert_ok darwin_home_firmlink_skip_realhost "$shf_ok" \
      "rc=$shf_rc (Darwin /home firmlink verified + skipped; no HOME_ROOT_SYMLINK)"
else
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (non-Darwin or /home absent/nonstandard/unresolvable; macOS-only standard-firmlink predicate)\n' darwin_home_firmlink_skip_realhost
fi

echo
echo "=== Remediation split (ambiguity-only vs definite evidence) ==="
# Ambiguity-only (no unmanaged/marker evidence): advisory refusal. Must NOT contain
# uninstall/removal instructions; MUST mention a privileged read-only recheck.
if [ "$(id -u)" -eq 0 ]; then
    SKIP=$((SKIP + 1)); printf '  SKIP  %-38s (running as root: unreadable not meaningful)\n' remediation_ambiguity_only
else
    am_dir="$SCRATCH/ambig_remed"
    mkdir -p "$am_dir"
    make_ambiguous_unreadable "$am_dir"
    set +e; am_out=$(clean_env "$DETECT" --root "$am_dir" 2>&1); am_rc=$?; set -e
    am_ok=1
    [ "$am_rc" -eq 2 ] || am_ok=0
    case "$am_out" in *"uninstall"*) am_ok=0 ;; esac       # no uninstall guidance
    case "$am_out" in *"rm -rf /nix"*) am_ok=0 ;; esac      # no removal instruction
    case "$am_out" in *"privileged"*) ;; *) am_ok=0 ;; esac # privileged preflight
    case "$am_out" in *"read-only"*) ;; *) am_ok=0 ;; esac  # read-only recheck
    assert_ok remediation_ambiguity_only "$am_ok" "rc=$am_rc (no uninstall; privileged read-only recheck)"
fi
# Definite unmanaged evidence: bounded vendor-uninstall guidance IS present.
um_dir="$SCRATCH/unmanaged_remed"
mkdir -p "$um_dir"
make_existing_install_linux "$um_dir"
set +e; um_out=$(clean_env "$DETECT" --root "$um_dir" 2>&1); um_rc=$?; set -e
um_ok=1
[ "$um_rc" -eq 2 ] || um_ok=0
case "$um_out" in *"uninstall"*) ;; *) um_ok=0 ;; esac
case "$um_out" in *"ITS OWN uninstaller"*) ;; *) um_ok=0 ;; esac
assert_ok remediation_definite_unmanaged "$um_ok" "rc=$um_rc (bounded vendor-uninstall guidance)"

echo
echo "RESULT: $PASS passed, $FAIL failed, $SKIP skipped."
if [ "$FAIL" -ne 0 ]; then
    echo "FAILED:${FAILED_CASES}"
    exit 1
fi
exit 0
