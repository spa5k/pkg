#!/bin/sh
# detect-unmanaged-nix.sh — Spike S1 fail-closed, read-only unmanaged-Nix detector.
#
# Part of PR-4 / Spike S1 (store-prefix & coexistence). This is a SAFE SPIKE probe,
# NOT production code. See findings.md for the full analysis and DR-001 for the
# decision it informs.
#
# PURPOSE (single, narrow): install/preflight. This script is the UNPRIVILEGED
# early read-only scan; it can REFUSE (advisory) but can never AUTHORIZE
# installation on its own. What authorizes proceeding is a FULL read-only
# PRIVILEGED preflight re-run by the signed installer/helper IMMEDIATELY before any
# mutation. ANY Nix artifact — up to and including a lone pkg ownership marker —
# causes a REFUSE (exit 2). pkg never auto-deletes anything and provides no --force.
# There is NO runtime mode, NO mode reclassification, and NO recognition of an
# existing /nix tree as "ours" in this spike. Runtime/doctor recognition
# (reclassifying an already-present /nix as pkg-owned) is explicitly DEFERRED to
# PR-9/PR-12, and must require an authenticated/validated ownership receipt PLUS
# verification of the COMPLETE expected managed-artifact set — never a path or a
# marker alone. This spike does not design or implement that.
#
# Safety contract (enforced by construction — there is no code path that mutates):
#   * READ-ONLY. It only stat/read/grep/list. It never calls sudo, rm, mkdir on the
#     target host, install, apt/dnf, systemctl start/stop/enable/disable,
#     launchctl load/bootstrap/remove, diskutil (mutating verbs), mount/unmount,
#     chown, or chmod the target host.
#   * It never creates, deletes, or otherwise mutates /nix, /etc/nix, /etc/fstab,
#     /etc/synthetic.conf, any service unit/plist, or any user account.
#   * It never touches an existing Nix installation.
#   * Optional corroborating probes (systemctl/launchctl/mount/getent/dscl/diskutil)
#     are restricted to READ-ONLY list/status queries, guarded behind `command -v`,
#     scoped to the applicable OS, and only run on a real host (root == "/").
#     A present-but-failing corroborator is treated as ambiguous -> REFUSE where
#     practical; a tool absent on the other OS is never a finding.
#   * Ambiguous or unreadable relevant state => REFUSE (fail closed).
#
# Portability: targets POSIX sh (#!/bin/sh). Runs on dash, bash (incl. the
# macOS 3.2 system bash in POSIX mode), zsh, and busybox ash. No arrays, no
# bashisms, no `local` reliance, no non-POSIX `find -maxdepth`. Uses `awk` (POSIX)
# only for JSON string escaping. Variables are global with unique prefixes (this is
# a non-recursive script).
#
# Usage:
#   detect-unmanaged-nix.sh [--root DIR] [--json] [-q] [--help]
#
# Exit codes:
#   0  clean — no Nix artifacts detected.
#   2  refuse — one or more unmanaged OR ambiguous artifacts found (incl. a lone
#      pkg ownership marker, which is corroborating only and never authorizes).
#   64 usage error (unknown arg; bare --root with no value; --root that is
#      relative, missing, a symlink other than literal "/", an unsafe subtree, or
#      the removed --mode flag).
#
# Env:
#   PKG_DETECT_ROOT    same as --root (default "/")
#
# There are NO PKG_PROBE_* bypass knobs: a user-controlled environment must not be
# able to disable fail-closed corroborators. Live corroborators run ONLY at the
# literal root "/" and are gated by `command -v`; tests use fake roots to avoid them.

set -eu

# ----------------------------------------------------------------------------
# Globals / counters
# ----------------------------------------------------------------------------
ROOT="${PKG_DETECT_ROOT:-/}"
JSON=0
QUIET=0
ROOT_INSPECTABLE=1 # set to 0 by validate_root if an existing root cannot be
                   # canonicalized/entered/read/searched; main() then records
                   # ROOT_UNINSPECTABLE after emit_open and skips all scans so an
                   # unreadable fake root can never scan CLEAN.
N_FIND=0           # total findings
N_UNMANAGED=0      # definite foreign Nix
N_AMBIG=0          # cannot determine (unreadable, stale marker, query failed, etc.)
N_MARKER=0         # pkg ownership marker (corroborating ONLY; never authorizes;
                   # in install/preflight its presence is itself a refusal)

# A pkg ownership marker is ONE corroborating signal. It NEVER by itself authorizes
# takeover or implies the store is safe. In install mode a lone marker is a refusal.
MARKER_RELPATH_LINUX="var/lib/pkg/.managed-nix"
MARKER_RELPATH_MACOS="Library/Application Support/pkg/.managed-nix"

# Critical Nix paths whose mere presence means foreign Nix (relative to ROOT).
REL_NIX_ROOT="nix"
REL_NIX_STORE="nix/store"
REL_NIX_VAR="nix/var/nix"
REL_NIX_SOCKET="nix/var/nix/daemon-socket/socket"
REL_NIX_DB="nix/var/nix/db"
REL_NIX_PROFILES="nix/var/nix/profiles"
REL_ETC_NIX="etc/nix"
REL_NIX_CONF="etc/nix/nix.conf"

# OS detection (scopes real-host corroborators to the applicable platform).
KERNEL=$(uname -s 2>/dev/null || echo unknown)
case "$KERNEL" in
    Linux)  DET_OS=linux ;;
    Darwin) DET_OS=macos ;;
    *)      DET_OS=unknown ;;
esac

# usage -----------------------------------------------------------------------
usage() {
    cat <<'EOF'
Usage: detect-unmanaged-nix.sh [--root DIR] [--json] [-q] [--help]

Read-only, fail-closed install/preflight detector for an existing (unmanaged)
Nix installation. Any positive OR ambiguous artifact — including a lone pkg
ownership marker — causes a REFUSE (exit 2). pkg never deletes an existing
installation; there is no --force and no runtime/mode recognition.

  --root DIR      filesystem root to scan (default "/", or $PKG_DETECT_ROOT).
                  Must be absolute and an existing directory. A value is
                  REQUIRED: a bare `--root` (no following argument) exits 64.
                  Unsafe roots (/nix, /etc/nix, /etc/systemd,
                  /Library/LaunchDaemons, ...) and any non-"/" symlink root
                  are rejected. Roots containing "."/".." segments and roots
                  whose canonical physical target is an unsafe scanned/system
                  subtree are also rejected. Only the literal "/" is accepted
                  as a symlink.
  --json          emit machine-readable JSON instead of human text. Messages
                  carry only signal IDs and fixed/redacted text — no untrusted
                  dynamic values (env values, file contents, symlink targets,
                  paths) are ever included; strings are JSON-escaped.
  -q, --quiet     suppress detail; only the exit code is meaningful.
  -h, --help      show this help.

Exit codes: 0 clean | 2 refuse | 64 usage error.
EOF
}

# ----------------------------------------------------------------------------
# Output helpers (text + json). record() is the single append point.
# ----------------------------------------------------------------------------
# json_escape: stdin -> stdout, escaped for a JSON string body. Robust: escapes
# backslash, double-quote, and control chars 0x00-0x1f (short forms + \uXXXX).
# Used defensively on every message even though messages carry only controlled
# text (signal IDs + fixed/redacted sentences + numeric counts; env-var
# detection is PRESENCE-ONLY — no env-var names, counts, or values are emitted),
# so hostile inputs can never produce invalid JSON.
json_escape() {
    LC_ALL=C awk '
    function esc(s,   i, n, c, o, code) {
        o = ""; n = length(s)
        for (i = 1; i <= n; i++) {
            c = substr(s, i, 1)
            if (c == "\\") { o = o "\\\\" }
            else if (c == "\"") { o = o "\\\"" }
            else if (c == "\n") { o = o "\\n" }
            else if (c == "\r") { o = o "\\r" }
            else if (c == "\t") { o = o "\\t" }
            else if (c == "\b") { o = o "\\b" }
            else if (c == "\f") { o = o "\\f" }
            else {
                code = _ord[c] + 0
                if (code > 0 && code < 32) { o = o sprintf("\\u%04x", code) }
                else { o = o c }
            }
        }
        return o
    }
    BEGIN { for (i = 1; i < 256; i++) _ord[sprintf("%c", i)] = i }
    { printf "%s%s", (NR > 1 ? "\\n" : ""), esc($0) }
    '
}

emit_open() {
    if [ "$JSON" -eq 1 ]; then
        printf '%s\n' '{"findings":['
    fi
}

# record KIND ID PLATFORM MESSAGE
record() {
    r_kind=$1; r_id=$2; r_plat=$3; r_msg=$4
    N_FIND=$((N_FIND + 1))
    case "$r_kind" in
        unmanaged) N_UNMANAGED=$((N_UNMANAGED + 1)) ;;
        ambiguous) N_AMBIG=$((N_AMBIG + 1)) ;;
        marker)    N_MARKER=$((N_MARKER + 1)) ;;
    esac
    if [ "$QUIET" -eq 0 ]; then
        if [ "$JSON" -eq 1 ]; then
            [ "$N_FIND" -gt 1 ] && printf ',\n '
            r_msg_esc=$(printf '%s' "$r_msg" | json_escape)
            printf '{"kind":"%s","id":"%s","platform":"%s","message":"%s"}' \
                "$r_kind" "$r_id" "$r_plat" "$r_msg_esc"
        else
            printf '  [%s] %-14s %-10s %s\n' "$r_kind" "$r_id" "$r_plat" "$r_msg"
        fi
    fi
}

emit_close() {
    decide_result
    if [ "$JSON" -eq 1 ]; then
        printf '\n],"summary":{"total":%d,"unmanaged":%d,"ambiguous":%d,"marker":%d,"mode":"install","result":"%s"}}\n' \
            "$N_FIND" "$N_UNMANAGED" "$N_AMBIG" "$N_MARKER" "$RESULT"
    elif [ "$QUIET" -eq 0 ]; then
        printf '\nsummary: %d finding(s): %d unmanaged, %d ambiguous, %d marker (mode=install)\n' \
            "$N_FIND" "$N_UNMANAGED" "$N_AMBIG" "$N_MARKER"
        if [ "$RESULT" = "clean" ]; then
            printf 'RESULT: CLEAN\n'
        else
            printf 'RESULT: REFUSE\n'
        fi
    fi
}

# Install/preflight: ANY finding (unmanaged, ambiguous, or marker) => refuse.
decide_result() {
    RESULT=clean
    if [ "$N_FIND" -gt 0 ]; then
        RESULT=refuse
    fi
}

# ----------------------------------------------------------------------------
# Root validation / safety
# ----------------------------------------------------------------------------
# is_unsafe_root CANON -> 0 if CANON is inside a subtree the detector scans
# (/nix, /etc/nix, systemd/launchd/tmpfiles dirs, the pkg marker path, and their
# macOS /private aliases). Applied to BOTH the literal and the canonical root in
# validate_root. The absolute/existing/non-"/" symlink-root rules are enforced by
# validate_root (below), not by this matcher, so they are not restated here.
is_unsafe_root() {
    case "$1" in
        /nix|/nix/*) return 0 ;;
        /etc/nix|/etc/nix/*) return 0 ;;
        /etc/systemd|/etc/systemd/*) return 0 ;;
        /etc/tmpfiles.d|/etc/tmpfiles.d/*) return 0 ;;
        /Library/LaunchDaemons|/Library/LaunchDaemons/*) return 0 ;;
        /Library/LaunchAgents|/Library/LaunchAgents/*) return 0 ;;
        "/Library/Application Support"|"/Library/Application Support"/*) return 0 ;;
        /var/lib/pkg|/var/lib/pkg/*) return 0 ;;
        # macOS /private aliases of /etc, /var (canonical forms), so a symlink or
        # alias chain that resolves into a scanned subtree is still rejected.
        /private/etc/nix|/private/etc/nix/*) return 0 ;;
        /private/etc/systemd|/private/etc/systemd/*) return 0 ;;
        /private/etc/tmpfiles.d|/private/etc/tmpfiles.d/*) return 0 ;;
        /private/var/lib/pkg|/private/var/lib/pkg/*) return 0 ;;
    esac
    return 1
}

# has_dot_segment PATH -> 0 if any path component is "." or ".." (lexical traversal
# alias guard). POSIX: peel components one at a time without arrays/globbing.
has_dot_segment() {
    hds=$1
    case "$hds" in /*) hds=${hds#?} ;; esac   # strip a single leading slash
    while [ -n "$hds" ]; do
        hds_c=${hds%%/*}
        case "$hds_c" in .|..) return 0 ;; esac
        hds=${hds#"$hds_c"}
        case "$hds" in /*) hds=${hds#?} ;; *) hds= ;; esac
    done
    return 1
}

# resolve_canon DIR -> print canonical absolute path of an existing directory
# (POSIX: cd -P + pwd -P). Returns nonzero if not enterable. Resolves macOS
# /var -> /private/var, so a normal /var/folders temp root is NOT mis-rejected.
resolve_canon() {
    ( cd -P -- "$1" >/dev/null 2>&1 && pwd -P ) || return 1
}

validate_root() {
    case "$ROOT" in
        /*) ;;  # absolute, ok
        *) printf 'error: --root must be an absolute path (got "%s")\n' "$ROOT" >&2; exit 64 ;;
    esac
    # Reject any root containing "." or ".." segments (lexical traversal alias:
    # e.g. /tmp/../nix). Caught before existence so a missing alias is still refused.
    if has_dot_segment "$ROOT"; then
        printf 'error: refusing --root with a "." or ".." segment: "%s"\n' "$ROOT" >&2
        exit 64
    fi
    # Reject any non-"/" symlink root. Do NOT follow a fake-root symlink.
    if [ "$ROOT" != "/" ] && [ -L "$ROOT" ]; then
        printf 'error: refusing symlink --root "%s": a symlink root could redirect scans into the real filesystem. Use "/" or a real directory.\n' "$ROOT" >&2
        exit 64
    fi
    if [ ! -e "$ROOT" ]; then
        printf 'error: --root does not exist: "%s"\n' "$ROOT" >&2; exit 64
    fi
    if [ ! -d "$ROOT" ]; then
        printf 'error: --root is not a directory: "%s"\n' "$ROOT" >&2; exit 64
    fi
    if [ "$ROOT" != "/" ] && is_unsafe_root "$ROOT"; then
        printf 'error: refusing unsafe --root "%s": it is inside a subtree the detector scans. Use "/" or an independent scratch directory.\n' "$ROOT" >&2
        exit 64
    fi
    # Also validate the CANONICAL physical directory: a symlink/alias chain must
    # not resolve into an unsafe scanned/system subtree. This does NOT reject a
    # normal macOS /var/folders temp root: /var -> /private/var is fine because
    # /private/var/folders is not a scanned Nix/system subtree.
    if vr_canon=$(resolve_canon "$ROOT" 2>/dev/null); then
        if [ "$vr_canon" != "$ROOT" ] && is_unsafe_root "$vr_canon"; then
            printf 'error: refusing --root "%s": canonical target "%s" is inside a subtree the detector scans. Use "/" or an independent scratch directory.\n' "$ROOT" "$vr_canon" >&2
            exit 64
        fi
    else
        # An existing root we cannot enter (cd -P fails) cannot be inspected. Do
        # NOT silently continue (that would let an unreadable fake root scan CLEAN).
        ROOT_INSPECTABLE=0
    fi
    # An existing-but-unreadable root cannot be listed/verified -> ambiguous refuse.
    [ -r "$ROOT" ] || ROOT_INSPECTABLE=0
}

# ----------------------------------------------------------------------------
# Read-only primitives (all non-mutating)
# ----------------------------------------------------------------------------
# probe_exists RELPATH -> 0 if present (incl. dangling symlink).
probe_exists() {
    p="$ROOT/$1"
    [ -e "$p" ] || [ -L "$p" ]
}

# probe_unreadable RELPATH -> 0 if present but not readable (ambiguous signal).
probe_unreadable() {
    p="$ROOT/$1"
    if [ -e "$p" ] || [ -L "$p" ]; then
        [ ! -r "$p" ]
        return $?
    fi
    return 1
}

# probe_dir_nonempty RELPATH -> 0 if dir exists and has >=1 entry.
probe_dir_nonempty() {
    p="$ROOT/$1"
    [ -d "$p" ] || return 1
    # POSIX glob non-empty check (spaces/newlines safe; no `ls` parsing).
    for pe in "$p"/* "$p"/.*; do
        [ -e "$pe" ] || [ -L "$pe" ] || continue
        case "${pe#"$p"/}" in .|..) continue ;; esac
        return 0
    done
    return 1
}

# ----------------------------------------------------------------------------
# Signal checks. Each calls record() with a stable id, platform, and a FIXED,
# redacted message (no untrusted dynamic values). Platforms: any|linux|macos.
# ----------------------------------------------------------------------------
check_nix_tree() {
    # /nix root. A symlink is a strong foreign signal; do not print its target
    # (hostile). An unreadable /nix is ambiguous (fail closed).
    if [ -L "$ROOT/$REL_NIX_ROOT" ]; then
        record unmanaged NIX_ROOT_SYMLINK any \
          "/nix exists and is a symlink; consistent with a macOS APFS 'Nix Store' synthetic volume or a hand-rolled root."
    elif probe_exists "$REL_NIX_ROOT"; then
        if probe_unreadable "$REL_NIX_ROOT"; then
            record ambiguous NIX_ROOT_UNREADABLE any \
              "/nix exists but is not readable; cannot verify it is unmanaged. pkg refuses rather than guess."
        else
            record unmanaged NIX_ROOT any "/nix exists; not created by this installer."
        fi
    fi
    # /nix/store
    if probe_exists "$REL_NIX_STORE"; then
        if probe_unreadable "$REL_NIX_STORE"; then
            record ambiguous NIX_STORE_UNREADABLE any "/nix/store exists but is not readable; cannot inspect contents."
        elif probe_dir_nonempty "$REL_NIX_STORE"; then
            record unmanaged NIX_STORE_POPULATED any \
              "/nix/store exists and is non-empty; a populated Nix store is already present."
        else
            record unmanaged NIX_STORE_EMPTY any "/nix/store exists but is empty; still a foreign Nix layout the installer did not create."
        fi
    fi
    # /nix/var/nix (daemon state)
    if probe_exists "$REL_NIX_VAR"; then
        record unmanaged NIX_VAR any "/nix/var/nix exists; daemon state/profiles/gcroots/db layout is present."
    fi
    if probe_exists "$REL_NIX_SOCKET"; then
        record unmanaged NIX_DAEMON_SOCKET any \
          "/nix/var/nix/daemon-socket/socket exists; a Nix daemon has been (or is) configured. Socket collision risk with pkg's daemon."
    fi
    if probe_exists "$REL_NIX_DB"; then
        record unmanaged NIX_DB any "/nix/var/nix/db exists; a Nix store database is present."
    fi
    if probe_exists "$REL_NIX_PROFILES"; then
        record unmanaged NIX_PROFILES any "/nix/var/nix/profiles exists; a Nix profile tree is present."
    fi
}

check_etc_nix() {
    if probe_exists "$REL_ETC_NIX"; then
        record unmanaged ETC_NIX_DIR any "/etc/nix exists; foreign Nix configuration directory."
    fi
    if probe_exists "$REL_NIX_CONF"; then
        if probe_unreadable "$REL_NIX_CONF"; then
            record ambiguous NIX_CONF_UNREADABLE any "/etc/nix/nix.conf exists but is not readable; cannot verify it is not foreign."
        else
            record unmanaged NIX_CONF any "/etc/nix/nix.conf exists; foreign Nix configuration."
        fi
    fi
}

check_systemd() {
    # Linux unit search paths under ROOT. File-based; runs on any OS (fail closed).
    for d in etc/systemd/system lib/systemd/system usr/lib/systemd/system run/systemd/system; do
        ud="$ROOT/$d"
        [ -e "$ud" ] || continue
        if [ ! -d "$ud" ]; then
            continue
        fi
        if [ ! -r "$ud" ]; then
            record ambiguous SYSTEMD_DIR_UNREADABLE linux "/$d exists but is not readable; cannot check for Nix units. pkg refuses rather than guess."
            continue
        fi
        for name in nix-daemon.service nix-daemon.socket nix.service nix-store.service; do
            if [ -e "$ud/$name" ] || [ -L "$ud/$name" ]; then
                record unmanaged SYSTEMD_UNIT linux "/$d/$name exists; a Nix systemd unit is installed."
            fi
        done
        for sub in multi-user.wants sockets.target.wants; do
            wd="$ud/$sub"
            [ -e "$wd" ] || [ -L "$wd" ] || continue
            [ -d "$wd" ] || continue
            if [ ! -r "$wd" ]; then
                record ambiguous SYSTEMD_WANTS_UNREADABLE linux "/$d/$sub exists but is not readable; cannot check for enabled Nix units. pkg refuses rather than guess."
                continue
            fi
            for name in nix-daemon.service nix-daemon.socket nix.service; do
                if [ -e "$wd/$name" ] || [ -L "$wd/$name" ]; then
                    record unmanaged SYSTEMD_WANTS linux "/$d/$sub/$name exists; a Nix systemd unit is enabled."
                fi
            done
        done
    done
    if probe_exists "etc/tmpfiles.d/nix-daemon.conf"; then
        record unmanaged SYSTEMD_TMPFILES linux "/etc/tmpfiles.d/nix-daemon.conf exists; Nix tmpfiles config present."
    fi
    # Real-host corroborator (Linux only). Distinguish "no match" from "query failed".
    if [ "$DET_OS" = linux ] && [ "$ROOT" = "/" ] && command -v systemctl >/dev/null 2>&1; then
        if sc_units=$(systemctl list-unit-files --no-legend 2>/dev/null); then
            if printf '%s\n' "$sc_units" | grep -qE '(^|[[:space:]])nix[-._]'; then
                record unmanaged SYSTEMCTL_LIST linux "systemctl list-unit-files mentions a Nix unit."
            fi
        else
            record ambiguous SYSTEMCTL_QUERY_FAILED linux "systemctl list-unit-files query failed; cannot corroborate. pkg refuses rather than guess."
        fi
    fi
}

check_launchd() {
    for d in Library/LaunchDaemons Library/LaunchAgents; do
        ud="$ROOT/$d"
        [ -e "$ud" ] || continue
        if [ ! -d "$ud" ]; then
            continue
        fi
        if [ ! -r "$ud" ]; then
            record ambiguous LAUNCHD_DIR_UNREADABLE macos "/$d exists but is not readable; cannot check for Nix plists. pkg refuses rather than guess."
            continue
        fi
        # Tight match: official "org.nixos.*", "nix-daemon", or a name beginning
        # "nix"/"_nixbld". Avoids false positives on unrelated "unix"/"phoenix"
        # artifacts while remaining fail-closed for real Nix jobs. Enumerate entries
        # with a POSIX glob (spaces/newlines safe); grep a single controlled name,
        # not `ls` output.
        ld_hit=0
        for le in "$ud"/*; do
            [ -e "$le" ] || [ -L "$le" ] || continue
            if printf '%s\n' "${le##*/}" | grep -qE '(org[.]nixos[.])|(^nix([-._]|$))|(^_nixbld)'; then
                ld_hit=1; break
            fi
        done
        if [ "$ld_hit" -eq 1 ]; then
            record unmanaged LAUNCHD_PLIST macos "/$d contains a Nix launchd job (e.g. org.nixos.nix-daemon)."
        fi
    done
    # Real-host corroborator (macOS only).
    if [ "$DET_OS" = macos ] && [ "$ROOT" = "/" ] && command -v launchctl >/dev/null 2>&1; then
        if lc_jobs=$(launchctl list 2>/dev/null); then
            if printf '%s\n' "$lc_jobs" | grep -qiE 'nixos[.]|nix-daemon'; then
                record unmanaged LAUNCHCTL_LIST macos "launchctl list mentions a Nix job (e.g. org.nixos.nix-daemon)."
            fi
        else
            record ambiguous LAUNCHCTL_QUERY_FAILED macos "launchctl list query failed; cannot corroborate. pkg refuses rather than guess."
        fi
    fi
}

check_synthetic_fstab() {
    sc="$ROOT/etc/synthetic.conf"
    if [ -e "$sc" ] && [ ! -r "$sc" ]; then
        record ambiguous SYNTHETIC_CONF_UNREADABLE macos "/etc/synthetic.conf exists but is not readable; cannot check for a 'nix' entry."
    elif [ -f "$sc" ]; then
        if grep -qE "^nix($|[[:space:]])" "$sc" 2>/dev/null; then
            record unmanaged SYNTHETIC_CONF_NIX macos \
              "/etc/synthetic.conf contains a 'nix' entry; used to synthesize a root-level /nix (macOS APFS volume)."
        fi
    fi
    fb="$ROOT/etc/fstab"
    if [ -e "$fb" ] && [ ! -r "$fb" ]; then
        record ambiguous FSTAB_UNREADABLE any "/etc/fstab exists but is not readable; cannot check for a /nix mount. pkg refuses rather than guess."
    elif [ -f "$fb" ]; then
        if grep -qiE "(^[[:space:]]*[^#].*[[:space:]]/nix([[:space:]]|$))" "$fb" 2>/dev/null \
           || grep -qiE "[[:space:]]/nix[[:space:]].*apfs" "$fb" 2>/dev/null; then
            record unmanaged FSTAB_NIX any \
              "/etc/fstab references /nix; consistent with a foreign Nix mount (macOS APFS 'Nix Store' volume or Linux bind mount)."
        fi
    fi
}

check_apfs_mount() {
    # Real-host read-only `mount` corroborator (both OSes).
    if [ "$ROOT" = "/" ] && command -v mount >/dev/null 2>&1; then
        if ap_m=$(mount 2>/dev/null); then
            if printf '%s\n' "$ap_m" | grep -qiE '(on /nix([[:space:]]|$)|Nix Store)'; then
                record unmanaged MOUNT_NIX macos "mount(8) lists a filesystem on /nix or a volume labeled 'Nix Store'."
            fi
        else
            record ambiguous MOUNT_QUERY_FAILED any "mount query failed; cannot corroborate a /nix filesystem. pkg refuses rather than guess."
        fi
    fi
    # Real-host read-only `diskutil apfs list` corroborator (macOS only) for an
    # unmounted APFS volume labeled "Nix Store". READ-ONLY; never mutated.
    if [ "$DET_OS" = macos ] && [ "$ROOT" = "/" ] && command -v diskutil >/dev/null 2>&1; then
        if ap_list=$(diskutil apfs list 2>/dev/null); then
            if printf '%s' "$ap_list" | grep -qiE 'Nix Store'; then
                record unmanaged DISKUTIL_NIX_APFS macos "diskutil apfs list reports an APFS volume labeled 'Nix Store'."
            fi
        else
            record ambiguous DISKUTIL_QUERY_FAILED macos "diskutil apfs list query failed; cannot corroborate an unmounted 'Nix Store' volume. pkg refuses rather than guess."
        fi
    fi
}

check_users_groups() {
    passwd="$ROOT/etc/passwd"
    group="$ROOT/etc/group"
    if [ -e "$passwd" ] && [ ! -r "$passwd" ]; then
        record ambiguous PASSWD_UNREADABLE any "/etc/passwd exists but is not readable; cannot check for Nix build users."
    elif [ -f "$passwd" ]; then
        if grep -qE "^(nixbld|_nixbld)[0-9]+:" "$passwd" 2>/dev/null; then
            ug_n=$(grep -cE "^(nixbld|_nixbld)[0-9]+:" "$passwd" 2>/dev/null || true)
            record unmanaged NIXBLD_USERS any "/etc/passwd has $ug_n Nix build user(s) (nixbld* and/or _nixbld*)."
        fi
    fi
    if [ -e "$group" ] && [ ! -r "$group" ]; then
        record ambiguous GROUP_UNREADABLE any "/etc/group exists but is not readable; cannot check for a Nix build group. pkg refuses rather than guess."
    elif [ -f "$group" ]; then
        if grep -qE "^(nixbld|_nixbld):" "$group" 2>/dev/null; then
            record unmanaged NIXBLD_GROUP any "/etc/group defines a nixbld or _nixbld build-users group."
        fi
    fi
    # Real-host corroborators (read-only), scoped by OS. No env bypass. macOS
    # stores users in OpenDirectory, so /etc/passwd alone misses them -> dscl on a Mac.
    if [ "$ROOT" = "/" ]; then
        # Linux: query the FULL getent passwd/group database and distinguish a
        # successful no-match from a query failure (absence of getent stays
        # optional because the static /etc/passwd and /etc/group are still checked).
        if [ "$DET_OS" = linux ] && command -v getent >/dev/null 2>&1; then
            if ug_pw=$(getent passwd 2>/dev/null); then
                if printf '%s\n' "$ug_pw" | grep -qE '^(nixbld|_nixbld)[0-9]+:'; then
                    record unmanaged GETENT_NIXBLD_USER linux "getent passwd reports a Nix build user (nixbld*/_nixbld*) in the name service."
                fi
            else
                record ambiguous GETENT_PASSWD_QUERY_FAILED linux "getent passwd query failed; cannot corroborate Nix build users. pkg refuses rather than guess."
            fi
            if ug_gr=$(getent group 2>/dev/null); then
                if printf '%s\n' "$ug_gr" | grep -qE '^(nixbld|_nixbld):'; then
                    record unmanaged GETENT_NIXBLD_GROUP linux "getent group reports a Nix build group (nixbld/_nixbld) in the name service."
                fi
            else
                record ambiguous GETENT_GROUP_QUERY_FAILED linux "getent group query failed; cannot corroborate a Nix build group. pkg refuses rather than guess."
            fi
        fi
        if [ "$DET_OS" = macos ] && command -v dscl >/dev/null 2>&1; then
            if ds_ulist=$(dscl . -list /Users 2>/dev/null); then
                if printf '%s\n' "$ds_ulist" | grep -qE '^_?nixbld'; then
                    record unmanaged DSCL_NIXBLD_USER macos "dscl reports Nix build user(s) (_nixbld*) in OpenDirectory."
                fi
                if ds_glist=$(dscl . -list /Groups 2>/dev/null); then
                    if printf '%s\n' "$ds_glist" | grep -qE '^_?nixbld'; then
                        record unmanaged DSCL_NIXBLD_GROUP macos "dscl reports a Nix build group in OpenDirectory."
                    fi
                else
                    record ambiguous DSCL_GROUPS_QUERY_FAILED macos "dscl -list /Groups failed; cannot corroborate a Nix build group. pkg refuses rather than guess."
                fi
            else
                record ambiguous DSCL_QUERY_FAILED macos "dscl user listing failed; cannot corroborate Nix build users. pkg refuses rather than guess."
            fi
        fi
    fi
}

# check_one_home PATH -> record + return 0 if a per-user Nix artifact is found
# directly under PATH; else return 1 (no finding).
check_one_home() {
    coh=$1
    for coh_name in .nix-profile .nix-defexpr .nix-channels; do
        if [ -e "$coh/$coh_name" ] || [ -L "$coh/$coh_name" ]; then
            record unmanaged NIX_PROFILE_FILES any "user Nix profile files found (e.g. $coh_name); a Nix profile was configured for a user."
            return 0
        fi
    done
    if [ -e "$coh/.local/state/nix" ] || [ -L "$coh/.local/state/nix" ] || \
       [ -e "$coh/.cache/nix" ] || [ -L "$coh/.cache/nix" ]; then
        record unmanaged NIX_PROFILE_FILES any "per-user Nix state/cache found (e.g. ~/.local/state/nix or ~/.cache/nix); a Nix profile was configured."
        return 0
    fi
    return 1
}

# scan_home_root DIR — DIR is a home root (/home, /Users, $HOME, /var/root, ...).
# Do NOT follow a symlinked home root or child home: a symlink may escape ROOT, so
# it is recorded as ambiguous (HOME_ROOT_SYMLINK / HOME_DIR_SYMLINK) and NOT
# traversed. An unreadable home root/dir is HOME_ROOT_UNREADABLE /
# HOME_DIR_UNREADABLE. Otherwise check DIR and each immediate child home via
# check_one_home. Enumeration uses POSIX quoted globs so spaces/newlines in names
# are handled as individual pathnames (no ls/find).
scan_home_root() {
    shr=$1
    [ -e "$shr" ] || [ -L "$shr" ] || return 0
    if [ -L "$shr" ]; then
        record ambiguous HOME_ROOT_SYMLINK any "a home root is a symlink; pkg does not traverse symlinks into unverified locations. pkg refuses rather than guess."
        return 0
    fi
    [ -d "$shr" ] || return 0
    if [ ! -r "$shr" ]; then
        record ambiguous HOME_ROOT_UNREADABLE any "a home root is not readable; cannot enumerate user Nix profiles. pkg refuses rather than guess."
        return 0
    fi
    # The home root itself may be someone's home (e.g. $HOME).
    check_one_home "$shr" || true
    for child in "$shr"/*; do
        # -L first (symlink -> ambiguous, do not traverse); then -d selects regular
        # dirs (its existence test subsumes the old separate [ -e ] check).
        if [ -L "$child" ]; then
            record ambiguous HOME_DIR_SYMLINK any "a home directory entry is a symlink; pkg does not traverse symlinks into unverified locations. pkg refuses rather than guess."
            continue
        fi
        [ -d "$child" ] || continue
        if [ ! -r "$child" ]; then
            record ambiguous HOME_DIR_UNREADABLE any "a home directory is not readable; cannot check it for user Nix profiles. pkg refuses rather than guess."
            continue
        fi
        check_one_home "$child" || true
    done
}

# is_standard_macos_home_firmlink PATH -> 0 iff PATH is the OS-standard macOS
# /home firmlink (-> /System/Volumes/Data/home), an OS-level firmlink from the
# system firmlinks list, NOT user-controlled and NOT a Nix artifact. VERIFIED, not
# assumed: requires ROOT="/", DET_OS=macos, PATH is a symlink, AND its canonical
# target — resolved with the existing POSIX resolve_canon (cd -P + pwd -P) — is
# EXACTLY /System/Volumes/Data/home. (PATH is normally "$ROOT/$h"; on a real host
# that is "//home" for h=home, and cd -P collapses the benign leading double
# slash.) Any other case returns 1 so the generic scan_home_root symlink guard
# still applies: if /home is ABSENT, a REAL DIRECTORY, a symlink to a DIFFERENT or
# UNRESOLVABLE target, or if this is not a real macOS host, /home is scanned
# normally and a symlink /home is recorded HOME_ROOT_SYMLINK (fail-closed). This
# replaces the earlier blind "every real-host Darwin /home is the standard link"
# assumption. Read-only: stat/readlink/cd/pwd only.
is_standard_macos_home_firmlink() {
    [ "$ROOT" = "/" ] || return 1
    [ "$DET_OS" = macos ] || return 1
    [ -L "$1" ] || return 1
    ismf_target=$(resolve_canon "$1" 2>/dev/null) || return 1
    [ "$ismf_target" = "/System/Volumes/Data/home" ] || return 1
    return 0
}

# standard_home_suffixes -> print the home-root SUFFIXES (relative to ROOT) to
# enumerate, one per line. Always returns the FIXED full set (root, home, Users,
# var/root); the suffixes are space-free strings, so callers may word-split the
# output safely and then quote "$ROOT/$suffix". The macOS OS-standard /home
# firmlink is NOT excluded here: it is skipped in check_profiles via the
# is_standard_macos_home_firmlink predicate (which VERIFIES the canonical target
# is exactly /System/Volumes/Data/home, rather than assuming every real-host
# Darwin /home is the standard link). On Linux (real /home directory) and on
# macOS FAKE roots (test-controlled /home symlink) the predicate returns 1, so /home
# is scanned and a symlink /home is still refused (HOME_ROOT_SYMLINK). Pure helper:
# reads/writes nothing on the host; emits only the fixed suffix list.
standard_home_suffixes() {
    printf '%s\n' root home Users var/root
}

check_profiles() {
    # Machine-global default profile under /nix/var/nix/profiles.
    if probe_exists "$REL_NIX_PROFILES/default"; then
        record unmanaged NIX_DEFAULT_PROFILE any "/nix/var/nix/profiles/default exists; a Nix default profile is present."
    fi
    # Enumerate the standard home roots via standard_home_suffixes. The suffixes
    # are FIXED space-free strings, so word-splitting the helper output is safe;
    # the full path "$ROOT/$h" is quoted, so a ROOT that contains spaces is
    # handled. scan_home_root handles an unreadable home root (=> ambiguous) and
    # enumerates each immediate child home with POSIX globs so spaces and newlines
    # in user/home-root names are handled as individual pathnames (no `ls` parsing,
    # no `find -maxdepth`, no unquoted word-splitting).
    #
    # macOS /home exception (VERIFIED, not assumed): skip the standard /home
    # firmlink ONLY when is_standard_macos_home_firmlink confirms ROOT="/",
    # DET_OS=macos, /home is a symlink, and its canonical target resolves to
    # EXACTLY /System/Volumes/Data/home. In every other case (absent, a real
    # directory, a different symlink target, or unresolvable) /home is scanned
    # normally — so a fake-root /home symlink is still refused
    # (HOME_ROOT_SYMLINK) and a non-standard real-host /home is still fail-closed.
    for h in $(standard_home_suffixes); do
        shrp="$ROOT/$h"
        is_standard_macos_home_firmlink "$shrp" && continue
        # Presence gate: admit existing directories OR symlinks to scan_home_root.
        # A plain `[ -d ]` follows a symlink and requires its (resolved) target to
        # be an existing directory, so a BROKEN or unresolvable nonstandard /home
        # symlink would fail `-d` and be SILENTLY skipped — even though the contract
        # says ANY nonstandard symlink must be refused. scan_home_root itself records
        # HOME_ROOT_SYMLINK and refuses (without traversing) any symlink, so
        # admitting symlinks here hands them off to be fail-closed rather than
        # dropped. An absent ordinary path (neither a directory nor a symlink) is
        # still skipped.
        { [ -d "$shrp" ] || [ -L "$shrp" ]; } || continue
        scan_home_root "$shrp"
    done
    # On a real-host scan, also ensure the current $HOME is checked even if it
    # lives outside the standard roots (e.g. a custom HOME). HOME is read via a
    # default-empty expansion so an environment with no HOME cannot crash under
    # `set -u`; an empty/unset HOME skips this branch ([ -d "" ] is false). Skip a
    # HOME ONLY when it is a standard root (or a child of one) that the loop above
    # ACTUALLY enumerated, so a custom HOME under a SKIPPED /home (the verified
    # macOS firmlink) or under /root on macOS (not a standard macOS root) is still
    # scanned:
    #   * verified standard /home firmlink skipped (real macOS): only /Users and
    #     /var/root were enumerated -> a HOME under /home/* or /root/* is scanned.
    #   * every other case (Linux; macOS fake root; macOS where /home is absent, a
    #     real dir, or a non-standard/unresolvable symlink): the full set
    #     (/root, /home, /Users, /var/root) was enumerated -> a HOME at or under
    #     any of them is skipped to avoid a double scan.
    if [ "$ROOT" = "/" ] && [ -d "${HOME:-}" ]; then
        if is_standard_macos_home_firmlink "/home"; then
            case "${HOME:-}" in
                /Users|/Users/*|/var/root|/var/root/*) ;;
                *) scan_home_root "${HOME:-}" ;;
            esac
        else
            case "${HOME:-}" in
                /root|/home|/Users|/var/root) ;;
                /root/*|/home/*|/Users/*|/var/root/*) ;;
                *) scan_home_root "${HOME:-}" ;;
            esac
        fi
    fi
    # /etc/profile.d Nix shell-integration snippet (tight match: "nix"/"nixos"
    # as words; avoids unrelated unix/phoenix files while staying fail-closed).
    pd="$ROOT/etc/profile.d"
    if [ -e "$pd" ] && [ ! -d "$pd" ]; then
        :
    elif [ -e "$pd" ] && [ ! -r "$pd" ]; then
        record ambiguous PROFILE_D_UNREADABLE any "/etc/profile.d exists but is not readable; cannot check for Nix shell integration. pkg refuses rather than guess."
    elif [ -d "$pd" ]; then
        pd_hit=0
        for pe in "$pd"/*; do
            [ -e "$pe" ] || [ -L "$pe" ] || continue
            if printf '%s\n' "${pe##*/}" | grep -qiwE 'nix|nixos'; then pd_hit=1; break; fi
        done
        # Use if/then (not `&& record`) so the function's last statement returns 0
        # even when pd_hit=0; otherwise a standalone `check_profiles` call under
        # `set -e` would crash on a clean host that has a non-Nix /etc/profile.d.
        if [ "$pd_hit" -eq 1 ]; then
            record unmanaged PROFILE_D_NIX any "/etc/profile.d contains a Nix shell-integration snippet."
        fi
    fi
}

check_binaries_on_path() {
    # `command -v` ignores --root (uses process PATH). Only meaningful on a real
    # host scan. There is NO install-time /opt/pkg/** whitelist: any Nix binary
    # reachable on PATH before installation is a refusal. Only the binary NAME
    # is recorded; the resolved path is never echoed (it may be hostile).
    [ "$ROOT" = "/" ] || return 0
    for b in nix nix-daemon nix-store nix-env nix-build nix-collect-garbage; do
        if command -v "$b" >/dev/null 2>&1; then
            record unmanaged PATH_BINARY any "$b found on PATH; a Nix binary is reachable before installation."
        fi
    done
}

check_binaries_under_root() {
    # Fixture-driven: look for nix* binaries under common bindirs within ROOT.
    for bd in bin usr/bin usr/local/bin opt/homebrew/bin; do
        d="$ROOT/$bd"
        [ -d "$d" ] || continue
        [ -r "$d" ] || { record ambiguous BINDIR_UNREADABLE any "/$bd exists but is not readable; cannot check for Nix binaries. pkg refuses rather than guess."; continue; }
        for b in nix nix-daemon nix-store nix-env nix-build; do
            if [ -e "$d/$b" ] || [ -L "$d/$b" ]; then
                record unmanaged ROOT_BINARY any "/$bd/$b exists; a Nix binary is installed under this root."
            fi
        done
    done
}

check_env() {
    # Detect the PRESENCE of one-or-more exported env vars whose name begins
    # with NIX_ (empty-valued included), and an exported IN_NIX_SHELL, and emit
    # a FIXED generic redacted message. We deliberately do NOT parse, count, or
    # echo variable names: the POSIX `env` output is LINE-ORIENTED serialization,
    # and a value containing a newline can introduce a line of the form
    # `NIX_FOO=...` that is INDISTINGUISHABLE from a real assignment. Echoing
    # such an extracted "name" would leak VALUE-DERIVED text (the original
    # overwatch finding: a non-NIX var whose multiline value contained a
    # `NIX_SPOOFED_FROM_VALUE=...` line was misparsed and the spoofed name
    # echoed). We therefore report PRESENCE ONLY — never names, never counts,
    # never values or value-derived text — and refuse conservatively. Honest
    # residual: this can false-positive on a non-NIX variable whose multiline
    # value happens to contain a `NIX_SOMETHING=...` line; that conservative
    # refusal is preferred over any value-derived leak, and is inherent to
    # line-oriented POSIX env serialization. Real exported NIX_* variables
    # (empty-valued included) and IN_NIX_SHELL are still refused. No eval; no
    # debug flag for values.
    # Implementation: the environment is queried with TWO direct `env | grep`
    # pipelines. env's stdout is consumed by grep directly and is NEVER read into
    # a shell variable, persisted, or reflected in any finding/output (the former
    # `ce_env=$(env ...)` captured the entire environment into a variable; that is
    # removed). If `env` itself is unavailable or exits nonzero we cannot
    # determine presence at all, so we FAIL CLOSED with a fixed ambiguous finding
    # rather than report clean.
    if ! env >/dev/null 2>&1; then
        record ambiguous ENV_QUERY_FAILED any \
          "the environment could not be queried (env failed); cannot check for NIX environment variables. pkg refuses rather than guess."
        return 0
    fi
    if env 2>/dev/null | LC_ALL=C grep -qE '^NIX_[A-Za-z0-9_]*='; then
        record unmanaged ENV_NIX_VAR any \
          "one or more NIX_* environment variables are present (presence-only; names, counts, and values redacted); a Nix shell/env is configured."
    fi
    if env 2>/dev/null | LC_ALL=C grep -q '^IN_NIX_SHELL='; then
        record unmanaged ENV_IN_NIX_SHELL any \
          "IN_NIX_SHELL is present (presence-only; value redacted); a Nix shell is active in this environment."
    fi
}

check_pkg_marker() {
    # A pkg ownership marker is ONE corroborating signal. It NEVER authorizes
    # takeover or implies the store is safe. In install/preflight its lone
    # presence is itself a refusal (a prior/foreign install or a planted marker).
    for rel in "$MARKER_RELPATH_LINUX" "$MARKER_RELPATH_MACOS"; do
        if probe_exists "$rel"; then
            if probe_unreadable "$rel"; then
                record ambiguous MARKER_UNREADABLE any "A pkg ownership marker exists but is not readable; cannot corroborate."
            else
                record marker PKG_MARKER any "pkg ownership marker present at /$rel (corroborating signal only; never authorizes takeover; install refuses)."
            fi
        fi
    done
}

# ----------------------------------------------------------------------------
# Remediation copy (printed on REFUSE). Honest, bounded, non-destructive.
# ----------------------------------------------------------------------------
# print_remediation — split by result. Ambiguity-only (no definite unmanaged or
# marker evidence) is an advisory refusal: an unprivileged scan can NEVER certify
# clean or authorize installation; it must NOT instruct removal. Definite
# unmanaged/marker evidence gives bounded vendor-uninstall guidance; pkg never
# removes anything. In both cases the ONLY thing that can authorize proceeding is
# a full read-only privileged preflight re-run immediately before mutation.
print_remediation() {
    [ "$QUIET" -eq 1 ] && return 0
    [ "$JSON" -eq 1 ] && return 0
    if [ "$N_UNMANAGED" -eq 0 ] && [ "$N_MARKER" -eq 0 ] && [ "$N_AMBIG" -gt 0 ]; then
        cat <<'EOF'

--------------------------------------------------------------------------------
pkg will NOT install or modify anything yet. Nothing was removed, and NOTHING
should be removed on the basis of this scan.

This unprivileged read-only scan could NOT certify the host clean: it found only
AMBIGUOUS state (unreadable or unrecognizable). An unprivileged scan can never
authorize installation.

Before any mutation, the signed privileged installer/helper MUST repeat the FULL
read-only preflight immediately. Only a CLEAN privileged preflight can authorize
proceeding. This two-phase contract closes the unprivileged permission gap and
shrinks the TOCTOU window to the moment before mutation.

If the privileged preflight is STILL ambiguous after a full read-only recheck,
STOP and seek support. There is no --force and no way to proceed past ambiguity.
--------------------------------------------------------------------------------
EOF
    else
        cat <<'EOF'

--------------------------------------------------------------------------------
pkg will NOT install, run, or modify anything. v1 takes EXCLUSIVE ownership of
/nix and will never coexist with, modify, or delete an existing Nix.

Definite unmanaged/foreign Nix evidence was found. To proceed, pick ONE path:
  1) Do not install pkg on this host (pkg cannot share /nix with another Nix in v1).
  2) Back up anything you need (profiles, generations), then fully uninstall the
     EXISTING Nix using ITS OWN uninstaller. pkg NEVER removes it:
       - Linux official multi-user: stop/disable nix-daemon.{service,socket}, then
         follow that installer's documented removal steps for /etc/{nix,
         tmpfiles.d/nix-daemon.conf,profile.d/nix*.sh}, the nixbld* users/group,
         and /nix.
       - macOS official: run the installer's own uninstaller if present (e.g.
         /nix/nix-installer uninstall), then remove the "Nix Store" APFS volume and
         the /etc/synthetic.conf 'nix' entry per that installer.
       - Determinate Nix Installer: `nix-installer uninstall` (its own tool).
  3) Remove leftover artifacts the uninstaller did not clean, by hand.
  4) Re-run: pkg doctor   (then pkg install)

pkg never runs `rm -rf /nix`, never stops or removes a foreign service, and
provides no --force override in v1. If any finding above is AMBIGUOUS (unreadable
or unrecognizable state), pkg refuses rather than guess; a privileged read-only
recheck immediately before mutation is the only thing that can authorize install.
--------------------------------------------------------------------------------
EOF
    fi
}

# ----------------------------------------------------------------------------
# main
# ----------------------------------------------------------------------------
main() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --root)
                if [ $# -lt 2 ]; then
                    printf 'error: --root requires a value (bare --root)\n' >&2
                    exit 64
                fi
                ROOT=$2; shift 2 ;;
            --root=*)
                ROOT=${1#--root=}; shift ;;
            --json) JSON=1; shift ;;
            -q|--quiet) QUIET=1; shift ;;
            -h|--help) usage; exit 0 ;;
            --mode|--mode=*)
                printf 'error: --mode is not supported. This detector is install/preflight only: any Nix artifact (including a lone marker) is refused. Runtime/doctor recognition is deferred to PR-9/PR-12 and will require an authenticated ownership receipt plus verification of the complete expected managed-artifact set.\n' >&2
                exit 64 ;;
            *) printf 'error: unknown argument "%s"\n' "$1" >&2; usage >&2; exit 64 ;;
        esac
    done

    validate_root

    [ "$QUIET" -eq 0 ] && [ "$JSON" -eq 0 ] && \
        printf 'Scanning root "%s" (mode=install)\n' "$ROOT"

    emit_open

    if [ "$ROOT_INSPECTABLE" -eq 0 ]; then
        # An existing root that cannot be entered/read/searched cannot be verified
        # Nix-free. Record a single ambiguous finding (after emit_open, preserving
        # the JSON schema) and skip all scans so it can never report CLEAN.
        record ambiguous ROOT_UNINSPECTABLE any \
          "the scan root exists but cannot be entered/read/searched; cannot verify it is Nix-free. pkg refuses rather than guess."
    else
        # Order: most decisive first. Marker is corroborating-only and itself a refusal.
        check_nix_tree
        check_etc_nix
        check_binaries_on_path
        check_binaries_under_root
        check_systemd
        check_launchd
        check_synthetic_fstab
        check_apfs_mount
        check_users_groups
        check_profiles
        check_env
        check_pkg_marker
    fi

    emit_close

    if [ "$RESULT" = "refuse" ]; then
        print_remediation
        exit 2
    fi
    exit 0
}

# Run main UNCONDITIONALLY. main is NOT gated on the invoking command's basename:
# a symlink or a renamed executable must behave identically to the canonical name
# (overwatch: a basename == "detect-unmanaged-nix.sh" guard made a differently-
# named copy SILENTLY exit 0, defeating every safety check). Sourcing this file
# therefore also runs a scan, so pure-helper coverage is provided by behavior-level
# regressions (run-tests.sh) rather than by sourcing. There is no PKG_PROBE_*
# bypass and no --mode, and no safety check is conditional on the basename or on
# any environment variable.
main "$@"
