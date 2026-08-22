#!/bin/sh
set -eu
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
umask 077

die() { printf '%s\n' "$*" >&2; exit 1; }
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}
owner() {
    case $(uname -s) in
        Darwin) stat -f '%u' "$1" ;;
        *) stat -c '%u' "$1" ;;
    esac
}

[ "$#" -eq 3 ] || die "invalid stage arguments"
asset=$1
expected=$2
platform=$3
stage_root=/var/tmp/pkg-s6-determinate-installer
test_mode=0
if [ "${S6_TEST_MODE:-}" = 1 ]; then
    test_mode=1
    stage_root=${S6_TEST_STAGE_ROOT:-$stage_root}
fi
[ "$test_mode" -eq 1 ] || [ "$(id -u)" -eq 0 ] || die "stage.sh requires EUID 0"
case $asset in /*) ;; *) die "asset path must be absolute" ;; esac
[ ! -L "$stage_root" ] || die "stage root must not be a symlink"
if [ ! -e "$stage_root" ]; then
    mkdir -m 700 "$stage_root"
fi
[ -d "$stage_root" ] || die "stage root must be a directory"
if [ "$test_mode" -eq 0 ]; then
    [ "$(owner "$stage_root")" -eq 0 ] || die "stage root must be root-owned"
fi
chmod 700 "$stage_root"

evidence=$(mktemp -d "$stage_root/run.XXXXXX")
chmod 700 "$evidence"
if [ "$test_mode" -eq 0 ]; then
    chown 0:0 "$evidence"
    [ "$(owner "$evidence")" -eq 0 ] || die "evidence directory must be root-owned"
fi
installer=$evidence/installer
cp "$asset" "$installer"
chmod 700 "$installer"

actual=$(sha256 "$installer")
printf '%s\n' "$platform" >"$evidence/platform.txt"
printf '%s\n' "$expected" >"$evidence/expected.sha256"
printf '%s\n' "$actual" >"$evidence/actual.sha256"
chmod 600 "$evidence/platform.txt" "$evidence/expected.sha256" "$evidence/actual.sha256"
[ "$actual" = "$expected" ] || die "staged digest mismatch"

printf '%s\n' '--diagnostic-endpoint' '' 'install' '--determinate' '--no-confirm' '--no-modify-profile' >"$evidence/argv.txt"
chmod 600 "$evidence/argv.txt"
set +e
"$installer" --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile >"$evidence/output.txt" 2>&1
status=$?
set -e
printf '%s\n' "$status" >"$evidence/status.txt"
chmod 600 "$evidence/output.txt" "$evidence/status.txt"
exit "$status"
