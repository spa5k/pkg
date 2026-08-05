#!/bin/sh
# build-fixtures.sh — materializes fake-root fixtures for detect-unmanaged-nix.sh.
#
# Safe-spike helper. The detector is read-only; THIS fixture harness intentionally
# creates files/dirs/symlinks and chmods them, but ONLY inside a verified mktemp
# suite root whose immediate parent is TMPDIR and whose name matches pkg-s1.*.
#
# Capability model (NOT cryptographic — it prevents accidents/path confusion inside
# the 0700 mktemp tree; it is not a defense against a determined attacker with write
# access to TMPDIR): fx_init_suite establishes a process-local capability = the
# canonical suite root (FX_SUITE_ROOT) + a per-run token (PID/time/path). The token
# is written into the sentinel. Every primitive re-reads the sentinel and requires
# ALL of: capability initialized; case root is a real (non-symlink) directory;
# sentinel content == the per-run token; canonical immediate parent of the case root
# == FX_SUITE_ROOT. A hand-planted CONSTANT sentinel must NOT authorize mutation
# (it fails the token check, the parent check, or the capability-initialized check).
# fx_init_suite refuses /, /nix, /etc, /Library, /var, /Users and system subtrees,
# relative/missing/non-dir/symlink roots, arbitrary repo/home roots, non-empty dirs,
# and wrong temp naming/parent BEFORE writing anything. It also allowlists the
# CANONICAL TMPDIR parent (/tmp, /private/tmp, /var/tmp, /private/var/tmp, or a
# macOS per-user root /private/var/folders/*/*/T) so a user-controlled TMPDIR
# pointing at a repository/home cannot host a pkg-s1 suite. Every primitive runs
# fx_guard_chain: it walks each EXISTING path component from the case root through
# the relpath and refuses if any component (incl. the final destination) is a
# symlink or any intermediate component is a non-directory — so a fixture cannot
# write THROUGH a pre-existing symlink planted inside the case dir. fx_cleanup_suite
# is the ONLY sanctioned recursive cleanup: it re-verifies canonical DIR ==
# FX_SUITE_ROOT AND exact sentinel-token match before any chmod/rm, then clears the
# capability.
#
# Intended use (run-tests.sh does this — source FIRST, then init):
#   . ./build-fixtures.sh
#   suite=$(mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX")
#   fx_init_suite "$suite"                   # establishes the capability + sentinel
#   cdir="$suite/<case>"; mkdir -p "$cdir"
#   make_existing_install_linux "$cdir"
#   make_existing_install_linux /            # -> REFUSED before any mutation

# When sourced under zsh, behave POSIX-sh-like so unmatched globs are left literal
# (not errors) and word splitting matches dash/bash. No-op under dash/bash/sh.
if [ -n "${ZSH_VERSION:-}" ]; then
    emulate -L sh
fi

# Sentinel file name written into the suite root by fx_init_suite(). Its CONTENT
# (the per-run token) is the capability, not its mere existence.
FX_SENTINEL=".pkg-s1-fixture-suite-v1"

# Process-local capability. Unset until fx_init_suite succeeds; primitives refuse
# while unset. (Globals with unique prefixes; no `local` — this is non-recursive.)
FX_SUITE_ROOT=""
FX_TOKEN=""

# fx_canon DIR -> print canonical absolute path of an existing directory (POSIX:
# cd -P + pwd -P resolves symlinks, including macOS /var -> /private/var). Fails
# (nonzero, no output) if DIR is not an enterable directory.
fx_canon() {
    ( cd -P -- "$1" >/dev/null 2>&1 && pwd -P ) || return 1
}

# fx_is_protected_root CANON -> 0 if CANON is a system/critical root or subtree.
# Applied to the canonical SUITE root only (case roots are gated by the capability
# + canonical-parent check). Deliberately does NOT include temp locations
# (/tmp, /private/tmp, /var/folders, /private/var/folders) so the standard mktemp
# suite under TMPDIR is accepted on both Linux and macOS.
fx_is_protected_root() {
    case "$1" in
        /) return 0 ;;
        /nix|/nix/*) return 0 ;;
        /etc|/etc/*) return 0 ;;
        /private/etc|/private/etc/*) return 0 ;;
        /Library|/Library/*) return 0 ;;
        /Users|/Users/*) return 0 ;;
        /var|/var/root|/var/root/*) return 0 ;;
        /private/var|/private/var/root|/private/var/root/*) return 0 ;;
        /bin|/bin/*|/sbin|/sbin/*|/usr|/usr/*|/opt|/opt/*) return 0 ;;
        /System|/System/*) return 0 ;;
    esac
    return 1
}

# fx_is_macos_tmproot CANON -> 0 iff CANON is EXACTLY
# /private/var/folders/<one>/<two>/T with <one> and <two> nonempty and
# slash-free (a macOS per-user TMPDIR leaf). A shell `case` glob
# (/private/var/folders/*/*/T) is NOT sufficient on its own because in a `case`
# pattern `*` SPANS slashes, so it would also accept deeper arbitrary
# descendants such as /private/var/folders/a/b/c/d/T. This helper does the
# exact-two-component check after a coarse shape filter. Fixed canonical roots
# (/tmp, /private/tmp, /var/tmp, /private/var/tmp) are matched separately in
# fx_init_suite; this helper covers ONLY the macOS per-user folder root.
fx_is_macos_tmproot() {
    fxim=$1
    # Coarse prefix/shape filter (fast reject of obviously-wrong paths).
    case "$fxim" in
        /private/var/folders/*/*/T) ;;
        *) return 1 ;;
    esac
    # Peel the fixed prefix and the /T suffix, then require EXACTLY two nonempty,
    # slash-free components (the load-bearing exact-component check).
    fxim_tail=${fxim#/private/var/folders/}   # <one>/<two>/T
    fxim_tail=${fxim_tail%/T}                  # <one>/<two>
    case "$fxim_tail" in */*) ;; *) return 1 ;; esac   # need the separating slash
    fxim_one=${fxim_tail%%/*}
    fxim_two=${fxim_tail#*/}
    [ -n "$fxim_one" ] || return 1
    # Reject empty second component or any stray slash (=> deeper descendant).
    case "$fxim_two" in '') return 1 ;; */*) return 1 ;; esac
    return 0
}

# fx_init_suite DIR — verify DIR is a fresh, correctly-named mktemp suite directly
# under TMPDIR, then establish the process-local capability (FX_SUITE_ROOT + token)
# and write the token into the sentinel. Refuses EVERYTHING before writing.
fx_init_suite() {
    # Safe default-empty expansion: a missing argument under `set -u` yields a
    # documented exit 64 instead of an "unbound variable" crash.
    fx_arg=${1:-}
    [ -n "$fx_arg" ] || { printf 'fx_init_suite: empty argument\n' >&2; exit 64; }
    # Absolute, existing, real directory.
    case "$fx_arg" in
        /*) ;;
        *) printf 'fx_init_suite: refusing relative path "%s"\n' "$fx_arg" >&2; exit 64 ;;
    esac
    [ -e "$fx_arg" ] || { printf 'fx_init_suite: refusing missing path "%s"\n' "$fx_arg" >&2; exit 64; }
    [ -d "$fx_arg" ] || { printf 'fx_init_suite: refusing non-directory "%s"\n' "$fx_arg" >&2; exit 64; }
    [ -L "$fx_arg" ] && { printf 'fx_init_suite: refusing symlink suite root "%s"\n' "$fx_arg" >&2; exit 64; }
    # Canonicalize (resolves macOS /var -> /private/var consistently for both the
    # suite and TMPDIR, so a normal /var/folders temp root is NOT rejected).
    fx_suite_canon=$(fx_canon "$fx_arg") || { printf 'fx_init_suite: cannot canonicalize "%s"\n' "$fx_arg" >&2; exit 64; }
    # Protected system root? (defense-in-depth; the TMPDIR-parent check below is
    # load-bearing for arbitrary repo/home roots.)
    if fx_is_protected_root "$fx_suite_canon"; then
        printf 'fx_init_suite: refusing protected/system root "%s" (canonical "%s")\n' "$fx_arg" "$fx_suite_canon" >&2
        exit 64
    fi
    # Canonical parent MUST be canonical TMPDIR (suite is a direct child of TMPDIR).
    fx_tmp_canon=$(fx_canon "${TMPDIR:-/tmp}") || { printf 'fx_init_suite: cannot canonicalize TMPDIR "%s"\n' "${TMPDIR:-/tmp}" >&2; exit 64; }
    # TMPDIR allowlist: only canonical SYSTEM temp roots may host a pkg-s1 suite, so
    # a user-controlled TMPDIR pointing at a repository/home is rejected BEFORE any
    # sentinel is written. Allowed canonical parents: the FIXED roots /tmp,
    # /private/tmp (macOS alias of /tmp), /var/tmp and /private/var/tmp
    # (documented, intentional; canonical roots only), and the macOS per-user temp
    # root /private/var/folders/<one>/<two>/T — accepted via fx_is_macos_tmproot,
    # which requires EXACTLY two nonempty slash-free components under
    # /private/var/folders (a `case` glob alone would let `*` span slashes and
    # wrongly accept deeper arbitrary descendants). Everything else is refused,
    # so a user-controlled TMPDIR outside these canonical roots cannot host a suite.
    case "$fx_tmp_canon" in
        /tmp|/private/tmp|/var/tmp|/private/var/tmp) ;;
        *)
            if fx_is_macos_tmproot "$fx_tmp_canon"; then
                :
            else
                printf 'fx_init_suite: refusing TMPDIR "%s" (canonical "%s"): not an allowed canonical temp parent (/tmp, /private/tmp, /var/tmp, /private/var/tmp, or macOS /private/var/folders/<one>/<two>/T with exactly two components). Suite must be a direct child of a standard system temp root.\n' "${TMPDIR:-/tmp}" "$fx_tmp_canon" >&2
                exit 64
            fi ;;
    esac
    fx_ipar_canon=$(fx_canon "$(dirname "$fx_suite_canon")") || { printf 'fx_init_suite: cannot resolve parent of "%s"\n' "$fx_arg" >&2; exit 64; }
    if [ "$fx_ipar_canon" != "$fx_tmp_canon" ]; then
        printf 'fx_init_suite: refusing "%s": canonical parent "%s" != canonical TMPDIR "%s". Suite must be a direct child of TMPDIR (mktemp -d "${TMPDIR:-/tmp}/pkg-s1.XXXXXXXX").\n' "$fx_arg" "$fx_ipar_canon" "$fx_tmp_canon" >&2
        exit 64
    fi
    # Basename must have the exact pkg-s1. prefix (mktemp template pkg-s1.XXXXXXXX).
    fx_base=${fx_suite_canon##*/}
    case "$fx_base" in
        pkg-s1.*) ;;
        *) printf 'fx_init_suite: refusing suite name "%s": basename must start with "pkg-s1." (template pkg-s1.XXXXXXXX)\n' "$fx_base" >&2; exit 64 ;;
    esac
    # Directory must be empty (no entries at all, including dotfiles). Use a POSIX
    # find -prune idiom ("$dir"/. ! -name . -prune -print) that lists ONLY the
    # immediate children (files/dirs/symlinks, incl. dotfiles) without descending:
    # find matches dotfiles under -name patterns (unlike a shell glob) and emits no
    # "unmatched glob" error, so it behaves even when the library is sourced by zsh
    # after a source-local `emulate sh` has reverted. FAIL CLOSED: if `find` is
    # unavailable or exits nonzero, refuse (exit 64) BEFORE writing the
    # sentinel/capability — a directory whose emptiness cannot be verified must
    # NOT be authorized as a suite (the former `|| true` was fail-open).
    if fx_children=$(find "$fx_suite_canon"/. ! -name . -prune -print 2>/dev/null); then
        :
    else
        printf 'fx_init_suite: cannot list children of "%s" (find failed or unavailable); refusing to initialize a suite whose emptiness cannot be verified.\n' "$fx_arg" >&2
        exit 64
    fi
    if [ -n "$fx_children" ]; then
        printf 'fx_init_suite: refusing non-empty directory "%s"\n' "$fx_arg" >&2; exit 64
    fi
    # Establish capability: per-run token from PID + epoch + suite path. NOT
    # cryptographic; prevents accidents/path confusion inside the 0700 mktemp tree.
    fx_t_pid=$$
    fx_t_time=$(date +%s 2>/dev/null || echo 0)
    FX_TOKEN="pkg-s1-suite:pid=$fx_t_pid:t=$fx_t_time:root=$fx_suite_canon"
    FX_SUITE_ROOT=$fx_suite_canon
    printf '%s\n' "$FX_TOKEN" > "$FX_SUITE_ROOT/$FX_SENTINEL"
    chmod 0600 "$FX_SUITE_ROOT/$FX_SENTINEL" 2>/dev/null || true
}

# fx_cleanup_suite DIR — the ONLY sanctioned recursive cleanup of the suite. Requires
# ALL of: capability initialized; canonical DIR == FX_SUITE_ROOT; sentinel present
# with content == FX_TOKEN. Then restores any mode-0000 entries (so rm can recurse)
# and removes the suite, and clears the process-local capability. Returns nonzero
# (does NOT exit) on any mismatch so an EXIT trap can continue without aborting.
# This is the only path that performs a recursive rm; reproducible docs must call it
# (never a raw `rm -rf` on the suite).
fx_cleanup_suite() {
    [ -n "${FX_SUITE_ROOT:-}" ] || { printf 'fx_cleanup_suite: capability not initialized\n' >&2; return 1; }
    [ -n "${FX_TOKEN:-}" ] || { printf 'fx_cleanup_suite: capability token missing\n' >&2; return 1; }
    # Safe default-empty expansion: a missing argument under `set -u` yields a
    # documented nonzero return instead of an "unbound variable" crash.
    [ -n "${1:-}" ] || { printf 'fx_cleanup_suite: empty argument\n' >&2; return 1; }
    fx_cs_canon=$(fx_canon "$1") || { printf 'fx_cleanup_suite: cannot canonicalize "%s"\n' "$1" >&2; return 1; }
    [ "$fx_cs_canon" = "$FX_SUITE_ROOT" ] || { printf 'fx_cleanup_suite: "%s" (canonical "%s") != suite root "%s"\n' "$1" "$fx_cs_canon" "$FX_SUITE_ROOT" >&2; return 1; }
    [ -f "$FX_SUITE_ROOT/$FX_SENTINEL" ] || { printf 'fx_cleanup_suite: sentinel missing from "%s"\n' "$FX_SUITE_ROOT" >&2; return 1; }
    fx_csent=$(cat "$FX_SUITE_ROOT/$FX_SENTINEL" 2>/dev/null || printf '')
    [ "$fx_csent" = "$FX_TOKEN" ] || { printf 'fx_cleanup_suite: sentinel token mismatch; refusing to remove "%s"\n' "$FX_SUITE_ROOT" >&2; return 1; }
    find "$FX_SUITE_ROOT" -type d -perm 0000 -exec chmod 0700 {} + 2>/dev/null || true
    find "$FX_SUITE_ROOT" -type f -perm 0000 -exec chmod 0600 {} + 2>/dev/null || true
    rm -rf "$FX_SUITE_ROOT"
    FX_SUITE_ROOT=
    FX_TOKEN=
}

# fx_guard_root ROOT — refuse anything that is not a case dir directly beneath the
# verified suite root, with a matching capability. Runs before any mutation in
# every primitive. Mere sentinel existence does NOT authorize; the content must
# equal the per-run token AND the case root's canonical parent must equal the suite.
fx_guard_root() {
    # Capability MUST be established in this process.
    [ -n "${FX_SUITE_ROOT:-}" ] || { printf 'fx: capability not initialized (call fx_init_suite first); a hand-planted sentinel does not authorize mutation\n' >&2; exit 64; }
    [ -n "${FX_TOKEN:-}" ] || { printf 'fx: capability token missing\n' >&2; exit 64; }
    case "$1" in
        "") printf 'fx: refusing empty root\n' >&2; exit 64 ;;
        /*) ;;
        *) printf 'fx: refusing relative root "%s"\n' "$1" >&2; exit 64 ;;
    esac
    # Case root must be an existing real directory, not a symlink.
    [ -L "$1" ] && { printf 'fx: refusing symlink case root "%s"\n' "$1" >&2; exit 64; }
    [ -e "$1" ] || { printf 'fx: refusing missing root "%s"\n' "$1" >&2; exit 64; }
    [ -d "$1" ] || { printf 'fx: refusing non-directory root "%s"\n' "$1" >&2; exit 64; }
    # Suite sentinel must exist and its content must equal the capability token.
    [ -f "$FX_SUITE_ROOT/$FX_SENTINEL" ] || { printf 'fx: capability sentinel missing from suite root "%s"\n' "$FX_SUITE_ROOT" >&2; exit 64; }
    fx_sent=$(cat "$FX_SUITE_ROOT/$FX_SENTINEL" 2>/dev/null || printf '')
    if [ "$fx_sent" != "$FX_TOKEN" ]; then
        printf 'fx: capability token mismatch (sentinel content does not equal the per-run token); a hand-planted constant sentinel does not authorize mutation\n' >&2
        exit 64
    fi
    # Canonical immediate parent of the case root MUST equal FX_SUITE_ROOT.
    fx_gpar_canon=$(fx_canon "$(dirname "$1")") || { printf 'fx: cannot resolve parent of "%s"\n' "$1" >&2; exit 64; }
    if [ "$fx_gpar_canon" != "$FX_SUITE_ROOT" ]; then
        printf 'fx: refusing root "%s": canonical parent "%s" != suite root "%s". Fixtures may only mutate case directories directly beneath the verified suite root.\n' "$1" "$fx_gpar_canon" "$FX_SUITE_ROOT" >&2
        exit 64
    fi
}

# fx_guard_rel REL — refuse traversal/absolute relpaths so a fixture cannot write
# outside its case dir via "../../etc/...".
fx_guard_rel() {
    case "$1" in
        ""|.|..|../*|*/..|*/../*|/../*|/*)
            printf 'fx: refusing unsafe relpath "%s"\n' "$1" >&2; exit 64 ;;
    esac
}

# fx_guard_chain ROOT REL — refuse if ANY existing path component from ROOT through
# ROOT/REL is a symlink, or if any existing INTERMEDIATE component is a
# non-directory. This prevents a fixture from writing THROUGH a pre-existing symlink
# planted inside the case dir (e.g. an earlier fx_symlink ROOT escape -> <sibling
# outside ROOT>, after which fx_file ROOT escape/pwn would otherwise follow it and
# write outside ROOT). Runs before every mkdir/chmod/write/symlink. Space/newline
# safe: manual POSIX slash-peeling (like has_dot_segment); REL is already validated
# non-absolute and traversal-free by fx_guard_rel. Components need not exist yet —
# only EXISTING components are inspected (missing components are created by mkdir).
fx_guard_chain() {
    fgc_root=$1; fgc_rel=$2
    fgc_cur=$fgc_root
    fgc_rest=$fgc_rel
    case "$fgc_rest" in /*) fgc_rest=${fgc_rest#?} ;; esac   # strip one leading slash
    while [ -n "$fgc_rest" ]; do
        fgc_comp=${fgc_rest%%/*}
        if [ "$fgc_comp" = "$fgc_rest" ]; then
            fgc_rest=
        else
            fgc_rest=${fgc_rest#"$fgc_comp"}
            case "$fgc_rest" in /*) fgc_rest=${fgc_rest#?} ;; esac
        fi
        [ -n "$fgc_comp" ] || continue      # tolerate consecutive slashes
        fgc_cur=$fgc_cur/$fgc_comp
        if [ -e "$fgc_cur" ] || [ -L "$fgc_cur" ]; then
            if [ -L "$fgc_cur" ]; then
                printf 'fx: refusing path through an existing symlink component "%s"\n' "$fgc_cur" >&2
                exit 64
            fi
            if [ -n "$fgc_rest" ] && [ ! -d "$fgc_cur" ]; then
                printf 'fx: refusing path through an existing non-directory component "%s"\n' "$fgc_cur" >&2
                exit 64
            fi
        fi
    done
}

# fx_dir ROOT RELPATH [mode]
fx_dir() {
    fx_root=$1; fx_rel=$2; fx_mode=${3:-0755}
    fx_guard_root "$fx_root"
    fx_guard_rel "$fx_rel"
    fx_guard_chain "$fx_root" "$fx_rel"
    mkdir -p "$fx_root/$fx_rel"
    chmod "$fx_mode" "$fx_root/$fx_rel" 2>/dev/null || true
}
# fx_file ROOT RELPATH CONTENT
fx_file() {
    fx_root=$1; fx_rel=$2; fx_content=$3
    fx_guard_root "$fx_root"
    fx_guard_rel "$fx_rel"
    fx_guard_chain "$fx_root" "$fx_rel"
    mkdir -p "$(dirname "$fx_root/$fx_rel")"
    printf '%s\n' "$fx_content" > "$fx_root/$fx_rel"
}
# fx_empty ROOT RELPATH   (empty regular file)
fx_empty() {
    fx_root=$1; fx_rel=$2
    fx_guard_root "$fx_root"
    fx_guard_rel "$fx_rel"
    fx_guard_chain "$fx_root" "$fx_rel"
    mkdir -p "$(dirname "$fx_root/$fx_rel")"
    : > "$fx_root/$fx_rel"
}
# fx_symlink ROOT RELPATH TARGET   (link created inside case dir; target is a string)
fx_symlink() {
    fx_root=$1; fx_rel=$2; fx_target=$3
    fx_guard_root "$fx_root"
    fx_guard_rel "$fx_rel"
    fx_guard_chain "$fx_root" "$fx_rel"
    mkdir -p "$(dirname "$fx_root/$fx_rel")"
    ln -s "$fx_target" "$fx_root/$fx_rel"
}

# A representative populated store path. (Fixture only; never created on a host.)
fx_store_path() {
    fx_root=$1
    # fx_dir self-guards (fx_guard_root) before any mkdir, so an explicit guard
    # here would be a duplicate consecutive fx_guard_root on the same root.
    fx_dir "$fx_root" "nix/store" 1775
    fx_file "$fx_root" "nix/store/0c2a7m9x4y3b2c1d0e9f8a7b6c5d4e3f-hello-2.12.1/bin/hello" \
        "#!/bin/sh
echo 'Hello, world!'"
    fx_file "$fx_root" "nix/store/0c2a7m9x4y3b2c1d0e9f8a7b6c5d4e3f-hello-2.12.1/share/doc" "doc"
}

# ---- cases -----------------------------------------------------------------

make_clean() {
    fx_guard_root "$1"
    : # genuinely empty root
}

make_existing_install_linux() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_store_path "$fx_root"
    fx_file "$fx_root" "etc/nix/nix.conf" "build-users-group = nixbld
sandbox = true"
    fx_file "$fx_root" "etc/systemd/system/nix-daemon.service" "[Unit]
Description=Nix Daemon
[Service]
ExecStart=/nix/var/nix/profiles/default/bin/nix-daemon"
    fx_file "$fx_root" "etc/systemd/system/nix-daemon.socket" "[Socket]
ListenStream=/nix/var/nix/daemon-socket/socket"
    fx_dir  "$fx_root" "nix/var/nix/daemon-socket"
    fx_empty "$fx_root" "nix/var/nix/daemon-socket/socket"
    fx_dir  "$fx_root" "nix/var/nix/db"
    fx_dir  "$fx_root" "nix/var/nix/profiles/per-user"
    fx_file "$fx_root" "etc/tmpfiles.d/nix-daemon.conf" "d /nix/var/nix 0755 root root -"
    fx_file "$fx_root" "etc/passwd" "root:x:0:0:root:/root:/bin/sh
nixbld1:x:30000:30000:Nix build user 1:/var/empty:/sbin/nologin
nixbld2:x:30001:30000:Nix build user 2:/var/empty:/sbin/nologin"
    fx_file "$fx_root" "etc/group" "root:x:0:
nixbld:x:30000:"
}

make_existing_install_macos() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_store_path "$fx_root"
    fx_file "$fx_root" "Library/LaunchDaemons/org.nixos.nix-daemon.plist" "<?xml version=\"1.0\"?>
<plist version=\"1.0\"><dict><key>Label</key><string>org.nixos.nix-daemon</string></dict></plist>"
    fx_file "$fx_root" "etc/synthetic.conf" "nix"
    fx_file "$fx_root" "etc/fstab" "UUID=AB12CD34-5678-90EF-1234-567890ABCDEF /nix apfs rw,noauto,nobrowse,suid,owner"
    fx_dir  "$fx_root" "nix/var/nix/daemon-socket"
    fx_empty "$fx_root" "nix/var/nix/daemon-socket/socket"
    fx_dir  "$fx_root" "nix/var/nix/db"
    fx_file "$fx_root" "etc/passwd" "root:x:0:0:root:/var/root:/bin/sh
_nixbld1:x:30000:30000:Nix build user 1:/var/empty:/usr/bin/false"
    fx_file "$fx_root" "etc/group" "root:x:0:
_nixbld:x:30000:"
}

# Unreadable /nix. Built mode 000 so a non-root detector run cannot read it.
make_ambiguous_unreadable() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_dir "$fx_root" "nix" 0000
}

# A pkg ownership marker is present but nothing else. PROVES the marker alone
# never authorizes anything: install/preflight => REFUSE.
make_product_marker_only() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_dir  "$fx_root" "var/lib/pkg"
    fx_file "$fx_root" "var/lib/pkg/.managed-nix" '{"product":"pkg","managed_nix":true}'
}

# Linux: only a systemd unit (no /nix at all).
make_linux_service() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_file "$fx_root" "etc/systemd/system/nix-daemon.service" "[Service]
ExecStart=/usr/bin/nix-daemon"
    fx_symlink "$fx_root" "etc/systemd/system/multi-user.wants/nix-daemon.service" \
        "/etc/systemd/system/nix-daemon.service"
}

# macOS: only a launchd plist.
make_macos_launchd() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_file "$fx_root" "Library/LaunchDaemons/org.nixos.nix-daemon.plist" \
        "<plist version=\"1.0\"><dict><key>Label</key><string>org.nixos.nix-daemon</string></dict></plist>"
}

# macOS APFS volume evidence: synthetic.conf + fstab + /nix synthetic symlink.
make_macos_apfs_synthetic_fstab() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_file    "$fx_root" "etc/synthetic.conf" "nix	System/Volumes/Data/nix"
    fx_file    "$fx_root" "etc/fstab" "none apfs rw /nix"
    fx_symlink "$fx_root" "nix" "/System/Volumes/Data/nix"
}

# /nix is a symlink to a non-standard location (ambiguous mount/synthetic link).
make_symlink_mount() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_symlink "$fx_root" "nix" "/private/nix"
}

# Nix binary installed under the scanned root (fixture-driven PATH analog).
make_nix_on_path() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_file "$fx_root" "usr/local/bin/nix" "#!/bin/sh
exec nix-real \"\$@\""
    fx_guard_chain "$fx_root" "usr/local/bin/nix"
    chmod 0755 "$fx_root/usr/local/bin/nix"
}

make_db_and_socket() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_dir   "$fx_root" "nix/var/nix/db"
    fx_dir   "$fx_root" "nix/var/nix/daemon-socket"
    fx_empty "$fx_root" "nix/var/nix/daemon-socket/socket"
}

# Per-user profile files. The ONLY user dir contains a SPACE in its name, so this
# case cannot pass via an ordinary (space-free) path: the home enumeration must be
# whitespace-safe to find it. (The former ordinary "alice" profile is removed.)
make_profile_only() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_dir     "$fx_root" "home/Some User"
    fx_symlink "$fx_root" "home/Some User/.nix-profile" "/nix/var/nix/profiles/default"
    fx_file    "$fx_root" "etc/profile.d/nix-daemon.sh" "# nix shell integration"
}

# A real-ish /etc/group that is UNREADABLE to a non-root detector run -> ambiguous.
make_group_unreadable() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_file "$fx_root" "etc/group" "root:x:0:"
    fx_guard_chain "$fx_root" "etc/group"
    chmod 0000 "$fx_root/etc/group"
}

# Unreadable pkg marker (e.g. bad perms) => ambiguous.
make_marker_unreadable() {
    fx_root=$1
    fx_guard_root "$fx_root"
    fx_dir "$fx_root" "var/lib/pkg" 0755
    fx_empty "$fx_root" "var/lib/pkg/.managed-nix"
    fx_guard_chain "$fx_root" "var/lib/pkg/.managed-nix"
    chmod 0000 "$fx_root/var/lib/pkg/.managed-nix"
}
