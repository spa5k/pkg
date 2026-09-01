#!/bin/sh
set -eu

die() { printf 'not ok - %s\n' "$*" >&2; exit 1; }
mode() {
    case $(uname -s) in
        Darwin) stat -f '%Lp' "$1" ;;
        *) stat -c '%a' "$1" ;;
    esac
}
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
sudo_log=$tmp/sudo.log
child_log=$tmp/child.log
pins=$tmp/assets.sha256
stage_root=$tmp/stage
mkdir "$stage_root"

cat >"$tmp/fake-sudo" <<'EOF'
#!/bin/sh
: >"$S6_TEST_SUDO_LOG"
[ "$1" = -- ] || exit 90
shift
exec "$@"
EOF
cat >"$tmp/asset" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" >"$S6_TEST_CHILD_LOG"
EOF
cat >"$tmp/PATH-installer" <<'EOF'
#!/bin/sh
exit 99
EOF
chmod 700 "$tmp/fake-sudo" "$tmp/asset" "$tmp/PATH-installer"
mkdir "$tmp/path"
mv "$tmp/PATH-installer" "$tmp/path/installer"
digest=$(sha256 "$tmp/asset")
printf '%s  aarch64-linux\n' "$digest" >"$pins"

run() {
    S6_TEST_MODE=1 S6_TEST_SUDO="$tmp/fake-sudo" S6_TEST_SUDO_LOG="$sudo_log" \
        S6_TEST_ASSETS_SHA256="$pins" S6_TEST_PLATFORM=${S6_TEST_PLATFORM_OVERRIDE:-aarch64-linux} \
        S6_TEST_STAGE_ROOT="$stage_root" S6_TEST_CHILD_LOG="$child_log" \
        PATH="$tmp/path:$PATH" "$script_dir/run.sh" "$@"
}
refuse_before_privilege() {
    rm -f "$sudo_log"
    if run "$@" >/dev/null 2>&1; then die "$1 was accepted"; fi
    [ ! -e "$sudo_log" ] || die "$1 reached privilege"
}

(cd "$tmp" && refuse_before_privilege asset)
ln -s "$tmp/asset" "$tmp/link"
refuse_before_privilege "$tmp/link"
refuse_before_privilege "$tmp"
printf '%064d  aarch64-linux\n' 0 >"$pins"
refuse_before_privilege "$tmp/asset"
printf '%s  aarch64-linux\n' "$digest" >"$pins"
S6_TEST_PLATFORM_OVERRIDE=x86_64-darwin
export S6_TEST_PLATFORM_OVERRIDE
refuse_before_privilege "$tmp/asset"
unset S6_TEST_PLATFORM_OVERRIDE

run "$tmp/asset"
evidence=$(find "$stage_root" -type f -name argv.txt -print | head -n 1)
[ -n "$evidence" ] || die "happy path made no evidence"
evidence_dir=${evidence%/*}
expected_argv=$tmp/expected-argv
printf '%s\n' '--diagnostic-endpoint' '' 'install' '--determinate' '--no-confirm' '--no-modify-profile' >"$expected_argv"
cmp "$expected_argv" "$evidence_dir/argv.txt" >/dev/null || die "recorded argv differs"
cmp "$expected_argv" "$child_log" >/dev/null || die "child argv differs"
[ "$(mode "$evidence_dir")" = 700 ] || die "evidence directory is not mode 0700"
for file in "$evidence_dir"/*.txt "$evidence_dir"/*.sha256; do
    [ "$(mode "$file")" = 600 ] || die "$file is not mode 0600"
done

rm -f "$child_log"
if S6_TEST_MODE=1 S6_TEST_STAGE_ROOT="$tmp/mismatch-stage" S6_TEST_CHILD_LOG="$child_log" \
    "$script_dir/stage.sh" "$tmp/asset" "$(printf '%064d' 0)" aarch64-linux >/dev/null 2>&1; then
    die "staged digest mismatch was accepted"
fi
[ ! -e "$child_log" ] || die "staged digest mismatch executed installer"

downloaders='(cu''rl|wge''t)'
shells='(s''h|ba''sh)'
if grep -E "$downloaders.*[|].*$shells" "$script_dir"/*.sh >/dev/null; then
    die "download-to-shell found"
fi
grep -F '"$installer" --diagnostic-endpoint' "$script_dir/stage.sh" >/dev/null || die "installer is not executed by absolute staged path"
printf '%s\n' 'ok - determinate installer harness'
