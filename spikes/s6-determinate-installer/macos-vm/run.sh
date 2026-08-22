#!/bin/sh
set -eu

die() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
private_tree() {
    find "$1" -type d -exec chmod 0700 {} \;
    find "$1" -type f -exec chmod 0600 {} \;
}
has_exact_vm() {
    listing=$(tart list) || return 2
    printf '%s\n' "$listing" | awk -v name="$1" '{ for (i = 1; i <= NF; i++) if ($i == name) found = 1 } END { exit !found }'
}
wait_pid() {
    limit=$1
    child=$2
    elapsed=0
    while kill -0 "$child" 2>/dev/null; do
        if [ "$elapsed" -ge "$limit" ]; then
            kill "$child" 2>/dev/null || :
            wait "$child" 2>/dev/null || :
            return 124
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done
    wait "$child"
}

[ "$#" -eq 3 ] || die "usage: $0 --approve-destructive-vm ABS_INSTALLER ABS_NEW_EVIDENCE"
[ "$1" = --approve-destructive-vm ] || die "explicit destructive VM approval is required"
[ "$(uname -s)" = Darwin ] || die "host must be Darwin"
[ "$(uname -m)" = arm64 ] || die "host must be arm64"

installer=$2
out=$3
for path in "$installer" "$out"; do
    case $path in /*) ;; *) die "paths must be absolute: $path" ;; esac
done
[ -f "$installer" ] && [ ! -L "$installer" ] || die "installer must be a regular non-symlink file"
[ ! -L "$out" ] || die "evidence path must not be a symlink"
[ ! -e "$out" ] || die "evidence path already exists: $out"
installer_dir=$(dirname "$installer")
installer_dir_real=$(CDPATH= cd -P "$installer_dir" && pwd) || die "installer parent is invalid"
[ "$installer" = "${installer_dir_real%/}/$(basename "$installer")" ] || die "installer path must be canonical and contain no symlinks"
out_parent=$(dirname "$out")
out_parent_real=$(CDPATH= cd -P "$out_parent" && pwd) || die "evidence parent does not exist"
[ "$out" = "${out_parent_real%/}/$(basename "$out")" ] || die "evidence path must be canonical and contain no symlinks"
available_kb=$(df -Pk "$out_parent" | awk 'END {print $4}')
case $available_kb in ''|*[!0-9]*) die "could not determine free disk" ;; esac
[ "$available_kb" -ge 16777216 ] || die "at least 16 GiB of free disk is required"

script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
repo_root=$(git -C "$script_dir/../../.." rev-parse --show-toplevel) || die "runner is not in a Git worktree"
[ -z "$(git -C "$repo_root" status --porcelain=v1 --untracked-files=all)" ] || die "runner worktree must be clean"
product_revision=$(git -C "$repo_root" rev-parse HEAD)
vendor_revision=4132ad07a15ee7d88c096ac7172b7afb2672866b
installer_pin=90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b
base=ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c
actual_installer_sha=$(sha256 "$installer")
[ "$actual_installer_sha" = "$installer_pin" ] || die "installer digest mismatch"

for tool in tart git shasum awk df find chmod uname sw_vers od tr sed sleep; do
    command -v "$tool" >/dev/null 2>&1 || die "required host tool is missing: $tool"
done
has_exact_vm "$base" || die "pinned base is not cached; refusing to pull"
export TART_NO_AUTO_PRUNE=1
token=$(LC_ALL=C od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
[ "${#token}" -eq 32 ] || die "could not generate run token"
vm_name=pkg-s6-dn03c-preflight-$token
set +e
has_exact_vm "$vm_name"
collision_status=$?
set -e
case $collision_status in
    0) die "generated VM name already exists: $vm_name" ;;
    1) ;;
    *) die "could not check generated VM name" ;;
esac

umask 077
mkdir -m 0700 "$out"
printf '%s\n' "$product_revision" >"$out/product-git-revision"
printf '%s\n' "$vendor_revision" >"$out/vendor-full-revision"
printf '%s\n' "$installer_pin" >"$out/installer.sha256"
printf '%s\n' "$base" >"$out/base-image"
printf '%s\n' "$vm_name" >"$out/vm-name"
printf 'vm=%s\ntoken=%s\n' "$vm_name" "$token" >"$out/vm-owner"
{
    sw_vers
    uname -a
    printf 'tart: '
    tart --version
} >"$out/host.txt" 2>&1

created=0
run_pid=
success=0
cleanup() {
    original_status=$?
    trap - EXIT HUP INT TERM
    cleanup_ok=1
    if [ "$created" -eq 1 ]; then
        if [ ! -f "$out/vm-owner" ] ||
            [ "$(sed -n '1p' "$out/vm-owner")" != "vm=$vm_name" ] ||
            [ "$(sed -n '2p' "$out/vm-owner")" != "token=$token" ]; then
            printf '%s\n' 'ownership record mismatch; VM preserved' >>"$out/cleanup"
            cleanup_ok=0
        else
            if [ -n "$run_pid" ] && kill -0 "$run_pid" 2>/dev/null; then
                tart stop "$vm_name" >>"$out/cleanup" 2>&1 &
                stop_pid=$!
                if wait_pid 60 "$stop_pid"; then :; else cleanup_ok=0; fi
                if ! wait_pid 60 "$run_pid"; then cleanup_ok=0; fi
            fi
            if [ "$cleanup_ok" -eq 1 ]; then
                tart delete "$vm_name" >>"$out/cleanup" 2>&1 &
                delete_pid=$!
                if wait_pid 60 "$delete_pid"; then :; else cleanup_ok=0; fi
            fi
            set +e
            has_exact_vm "$vm_name"
            absent_status=$?
            set -e
            case $absent_status in
                0) printf '%s\n' 'VM still present after cleanup' >>"$out/cleanup"; cleanup_ok=0 ;;
                1) printf '%s\n' 'verified exact VM absence' >>"$out/cleanup" ;;
                *) printf '%s\n' 'could not verify VM absence' >>"$out/cleanup"; cleanup_ok=0 ;;
            esac
        fi
    else
        printf '%s\n' 'no VM created' >>"$out/cleanup"
    fi
    private_tree "$out"
    [ "$cleanup_ok" -eq 1 ] || exit 1
    [ "$success" -eq 1 ] || exit "$original_status"
    printf '%s\n' 'PASS: macOS VM preflight'
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

created=1
tart clone "$base" "$vm_name" >>"$out/tart.log" 2>&1
tart run "$vm_name" --no-graphics --no-audio --no-clipboard --no-keyboard --no-pointer --net-softnet >>"$out/tart.log" 2>&1 &
run_pid=$!

bounded_exec() {
    limit=$1
    shift
    tart exec -i "$vm_name" "$@" &
    child=$!
    wait_pid "$limit" "$child"
}

ready=0
i=0
while [ "$i" -lt 60 ]; do
    if bounded_exec 10 /usr/bin/true </dev/null >>"$out/guest-agent.log" 2>&1; then ready=1; break; fi
    kill -0 "$run_pid" 2>/dev/null || die "Tart VM exited before Guest Agent became ready"
    i=$((i + 1))
    sleep 2
done
[ "$ready" -eq 1 ] || die "Tart Guest Agent did not become ready"
bounded_exec 15 /usr/bin/sudo -n /usr/bin/true </dev/null >>"$out/guest-agent.log" 2>&1 || die "passwordless guest sudo is unavailable"

guest_dir=/private/var/tmp/pkg-s6-dn03c-$token
marker=$guest_dir/owner-marker
bounded_exec 15 /usr/bin/sudo -n /bin/sh -c '
    set -eu
    dir=$1 marker=$2 token=$3
    [ ! -e "$dir" ] && [ ! -L "$dir" ]
    umask 077
    /bin/mkdir -m 0700 "$dir"
    /usr/sbin/chown root:wheel "$dir"
    /usr/bin/printf "%s\n" "$token" >"$marker"
    /usr/sbin/chown root:wheel "$marker"
    /bin/chmod 0600 "$marker"
' sh "$guest_dir" "$marker" "$token" </dev/null >>"$out/guest-agent.log" 2>&1 || die "could not create private guest staging"

guest_installer=$guest_dir/nix-installer
guest_inside=$guest_dir/inside.sh
bounded_exec 60 /usr/bin/sudo -n /bin/sh -c 'set -eu; umask 077; /bin/cat >"$1"; /usr/sbin/chown root:wheel "$1"; /bin/chmod 0600 "$1"' sh "$guest_installer" <"$installer" || die "could not stream installer into guest"
bounded_exec 30 /usr/bin/sudo -n /bin/sh -c 'set -eu; umask 077; /bin/cat >"$1"; /usr/sbin/chown root:wheel "$1"; /bin/chmod 0700 "$1"' sh "$guest_inside" <"$script_dir/inside.sh" || die "could not stream inside.sh into guest"
bounded_exec 60 /usr/bin/sudo -n "$guest_inside" "$token" "$marker" "$guest_installer" "$installer_pin" >"$out/guest-preflight.txt" 2>&1 || die "guest preflight failed"
private_tree "$out"
success=1
