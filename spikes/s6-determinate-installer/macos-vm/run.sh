#!/bin/sh
set -eu

die() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
write_argv() { file=$1; shift; : >"$file"; for arg in "$@"; do printf '%s\n' "$arg" >>"$file"; done; }
private_tree() {
    find "$1" -type d -exec chmod 0700 {} \;
    find "$1" -type f -exec chmod 0600 {} \;
}
active_child=
cleanup_active=0
wait_pid() {
    limit=$1
    child=$2
    wait_timed_out=0
    elapsed=0
    while kill -0 "$child" 2>/dev/null; do
        if [ "$elapsed" -ge "$limit" ]; then
            wait_timed_out=1
            kill -TERM "$child" 2>/dev/null || :
            grace=0
            while kill -0 "$child" 2>/dev/null && [ "$grace" -lt 5 ]; do
                sleep 1
                grace=$((grace + 1))
            done
            if kill -0 "$child" 2>/dev/null; then kill -KILL "$child" 2>/dev/null || :; fi
            wait "$child" 2>/dev/null || :
            if [ "$active_child" = "$child" ]; then active_child=; fi
            return 124
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done
    if wait "$child"; then wait_status=0; else wait_status=$?; fi
    if [ "$active_child" = "$child" ]; then active_child=; fi
    return "$wait_status"
}
bounded_host() {
    limit=$1
    shift
    signals_hold
    "$@" &
    active_child=$!
    signals_restore
    if wait_pid "$limit" "$active_child"; then bounded_status=0; else bounded_status=$?; fi
    active_child=
    return "$bounded_status"
}
has_exact_vm() {
    source=$1
    name=$2
    list_file=$3
    bounded_host 30 tart list --source "$source" --quiet >"$list_file" || return 2
    grep -Fx -- "$name" "$list_file" >/dev/null
}
signals_hold() { trap '' HUP INT TERM; }
signals_restore() {
    [ "$cleanup_active" -eq 0 ] || return 0
    trap 'terminate_for_signal 129' HUP
    trap 'terminate_for_signal 130' INT
    trap 'terminate_for_signal 143' TERM
}
terminate_for_signal() {
    signal_status=$1
    trap '' HUP INT TERM
    child=$active_child
    if [ -n "$child" ]; then
        if kill -0 "$child" 2>/dev/null; then
            kill -TERM "$child" 2>/dev/null || :
            grace=0
            while kill -0 "$child" 2>/dev/null && [ "$grace" -lt 5 ]; do
                sleep 1
                grace=$((grace + 1))
            done
            if kill -0 "$child" 2>/dev/null; then kill -KILL "$child" 2>/dev/null || :; fi
        fi
        wait "$child" 2>/dev/null || :
    fi
    active_child=
    exit "$signal_status"
}
trap 'terminate_for_signal 129' HUP
trap 'terminate_for_signal 130' INT
trap 'terminate_for_signal 143' TERM

[ "$#" -eq 7 ] || [ "$#" -eq 8 ] || die "usage: $0 --approve-destructive-vm --lane LANE --installer ABS --evidence ABS_NEW [--approve-observe-vendor-foreign-state]"
[ "$1" = --approve-destructive-vm ] || die "explicit destructive VM approval is required"
[ "$2" = --lane ] || die "expected --lane as argument 2"
lane=$3
[ "$4" = --installer ] || die "expected --installer as argument 4"
installer=$5
[ "$6" = --evidence ] || die "expected --evidence as argument 6"
out=$7
foreign_approval=
case $lane in
    lifecycle-diagnostics|crash-recovery|upstream-input)
        [ "$#" -eq 7 ] || die "extra arguments are forbidden for lane $lane"
        ;;
    foreign-nix)
        case $# in
            7) ;;
            8) [ "$8" = --approve-observe-vendor-foreign-state ] || die "invalid foreign-state approval"; foreign_approval=approve-observe-vendor-foreign-state ;;
            *) die "extra arguments are forbidden for lane $lane" ;;
        esac
        ;;
    *) die "unsupported lane: $lane" ;;
esac
[ "$(uname -s)" = Darwin ] || die "host must be Darwin"
[ "$(uname -m)" = arm64 ] || die "host must be arm64"

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
[ "$available_kb" -ge 33554432 ] || die "at least 32 GiB of free disk is required"
tart_home=${TART_HOME:-$HOME/.tart}
[ -d "$tart_home" ] && [ ! -L "$tart_home" ] || die "Tart storage path must be a non-symlink directory"
tart_available_kb=$(df -Pk "$tart_home" | awk 'END {print $4}')
case $tart_available_kb in ''|*[!0-9]*) die "could not determine Tart storage free disk" ;; esac
[ "$tart_available_kb" -ge 33554432 ] || die "at least 32 GiB of free Tart storage is required"

script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
repo_root=$(git -C "$script_dir/../../.." rev-parse --show-toplevel) || die "runner is not in a Git worktree"
[ -z "$(git -C "$repo_root" status --porcelain=v1 --untracked-files=all)" ] || die "runner worktree must be clean"
product_revision=$(git -C "$repo_root" rev-parse HEAD)
vendor_revision=4132ad07a15ee7d88c096ac7172b7afb2672866b
installer_pin=90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b
base=ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c
actual_installer_sha=$(sha256 "$installer")
[ "$actual_installer_sha" = "$installer_pin" ] || die "installer digest mismatch"

for tool in tart git shasum awk cat df find chmod uname sw_vers od tr sed sleep grep tar wc sort uniq cmp mv; do
    command -v "$tool" >/dev/null 2>&1 || die "required host tool is missing: $tool"
done
umask 077
mkdir -m 0700 "$out"
write_argv "$out/run.argv" "$0" "$@"
bounded_host 15 tart --version >"$out/tart-version.raw" || die "could not read Tart version"
tart_version=$(sed -n '1p' "$out/tart-version.raw")
[ "$tart_version" = 2.35.0 ] || die "Tart 2.35.0 is required"
has_exact_vm oci "$base" "$out/tart-list-oci.txt" || die "pinned base is not cached; refusing to clone"
export TART_NO_AUTO_PRUNE=1
token=$(LC_ALL=C od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
[ "${#token}" -eq 32 ] || die "could not generate run token"
vm_name=pkg-s6-dn03c-$lane-$token
set +e
has_exact_vm local "$vm_name" "$out/tart-list-local-preflight.txt"
collision_status=$?
set -e
case $collision_status in
    0) die "generated VM name already exists: $vm_name" ;;
    1) ;;
    *) die "could not check generated VM name" ;;
esac

printf '%s\n' "$product_revision" >"$out/product-git-revision"
printf '%s\n' "$vendor_revision" >"$out/vendor-full-revision"
printf '%s\n' "$installer_pin" >"$out/installer.expected.sha256"
printf '%s\n' "$actual_installer_sha" >"$out/installer.actual.sha256"
printf '%s\n' "$base" >"$out/base-image"
printf '%s\n' "$vm_name" >"$out/vm-name"
printf 'vm=%s\ntoken=%s\n' "$vm_name" "$token" >"$out/vm-owner"
printf '%s\n' "$available_kb" >"$out/evidence-available-kb"
printf '%s\n' "$tart_home" >"$out/tart-home"
printf '%s\n' "$tart_available_kb" >"$out/tart-available-kb"
printf '%s\n' "$tart_version" >"$out/tart-version"
printf '%s\n' "$lane" >"$out/lane"
case $lane in upstream-input) printf '%s\n' 'default NAT; public egress required' >"$out/network" ;; *) printf '%s\n' 'default NAT' >"$out/network" ;; esac
case $lane in
    lifecycle-diagnostics) printf '%s\n' baseline lifecycle-install reboot lifecycle-post-reboot lifecycle-repeat-install lifecycle-repair lifecycle-daemon lifecycle-uninstall lifecycle-repeat-uninstall reboot lifecycle-residue >"$out/phase-sequence" ;;
    crash-recovery) printf '%s\n' baseline crash-kill reboot crash-recover >"$out/phase-sequence" ;;
    foreign-nix)
        printf '%s\n' baseline foreign-synthetic-prepare reboot foreign-post-reboot foreign-refuse >"$out/phase-sequence"
        [ -z "$foreign_approval" ] || printf '%s\n' foreign-observe >>"$out/phase-sequence"
        ;;
    upstream-input) printf '%s\n' baseline upstream-install upstream-determinate-attempt >"$out/phase-sequence" ;;
esac
git show "$product_revision:spikes/s6-determinate-installer/macos-vm/inside.sh" >"$out/inside.sh"
inside_sha=$(sha256 "$out/inside.sh")
printf '%s\n' "$inside_sha" >"$out/inside.expected.sha256"
{
    sw_vers
    uname -a
} >"$out/host.txt" 2>&1

created=0
clone_attempted=0
run_pid=
success=0
cleanup() {
    original_status=$? cleanup_active=1
    signals_hold
    trap - EXIT
    cleanup_ok=1
    if [ "$created" -eq 1 ]; then
        if [ ! -f "$out/vm-owner" ] ||
            [ "$(sed -n '1p' "$out/vm-owner")" != "vm=$vm_name" ] ||
            [ "$(sed -n '2p' "$out/vm-owner")" != "token=$token" ]; then
            printf '%s\n' 'ownership record mismatch; VM preserved' >>"$out/cleanup"
            cleanup_ok=0
        else
            if [ -n "$run_pid" ] && kill -0 "$run_pid" 2>/dev/null; then
                if bounded_host 60 tart stop "$vm_name" >>"$out/cleanup" 2>&1; then :; else cleanup_ok=0; fi
                set +e
                wait_pid 60 "$run_pid"
                set -e
                [ "$wait_timed_out" -eq 0 ] || cleanup_ok=0
            fi
            if [ "$cleanup_ok" -eq 1 ]; then
                if bounded_host 60 tart delete "$vm_name" >>"$out/cleanup" 2>&1; then :; else cleanup_ok=0; fi
            fi
            set +e
            has_exact_vm local "$vm_name" "$out/tart-list-local-cleanup.txt"
            absent_status=$?
            set -e
            case $absent_status in
                0) printf '%s\n' 'VM still present after cleanup' >>"$out/cleanup"; cleanup_ok=0 ;;
                1) printf '%s\n' 'verified exact VM absence' >>"$out/cleanup" ;;
                *) printf '%s\n' 'could not verify VM absence' >>"$out/cleanup"; cleanup_ok=0 ;;
            esac
        fi
    elif [ "$clone_attempted" -eq 1 ]; then
        printf 'clone did not report success; exact name may need inspection: %s\n' "$vm_name" >>"$out/cleanup"
    else
        printf '%s\n' 'no VM created' >>"$out/cleanup"
    fi
    private_tree "$out"
    [ "$cleanup_ok" -eq 1 ] || exit 1
    [ "$success" -eq 1 ] || exit "$original_status"
    printf 'PASS: macOS VM lane %s\n' "$lane"
}
trap cleanup EXIT

clone_attempted=1
bounded_host 600 tart clone "$base" "$vm_name" >>"$out/tart.log" 2>&1
created=1
write_argv "$out/vm-resize.argv" tart set "$vm_name" --disk-size 80
bounded_host 60 tart set "$vm_name" --disk-size 80 >>"$out/tart.log" 2>&1 || die "could not resize cloned VM"
write_argv "$out/vm-run.argv" tart run --no-graphics --no-audio --no-clipboard --no-keyboard --no-pointer "$vm_name"
signals_hold
tart run --no-graphics --no-audio --no-clipboard --no-keyboard --no-pointer "$vm_name" >>"$out/tart.log" 2>&1 &
run_pid=$!
signals_restore

bounded_exec() {
    limit=$1
    stdin_file=$2
    shift 2
    signals_hold
    tart exec -i "$vm_name" "$@" <"$stdin_file" &
    active_child=$!
    signals_restore
    if wait_pid "$limit" "$active_child"; then bounded_status=0; else bounded_status=$?; fi
    active_child=
    return "$bounded_status"
}

ready=0
i=0
while [ "$i" -lt 60 ]; do
    if bounded_exec 10 /dev/null /usr/bin/true >>"$out/guest-agent.log" 2>&1; then ready=1; break; fi
    kill -0 "$run_pid" 2>/dev/null || die "Tart VM exited before Guest Agent became ready"
    i=$((i + 1))
    sleep 2
done
[ "$ready" -eq 1 ] || die "Tart Guest Agent did not become ready"
bounded_exec 15 /dev/null /usr/bin/sudo -n /usr/bin/true >>"$out/guest-agent.log" 2>&1 || die "passwordless guest sudo is unavailable"

guest_dir=/private/var/tmp/pkg-s6-dn03c-$token
marker=$guest_dir/owner-marker
guest_evidence=$guest_dir/evidence
bounded_exec 15 /dev/null /usr/bin/sudo -n /bin/sh -c '
    set -eu
    dir=$1 marker=$2 token=$3
    if [ -e "$dir" ] || [ -L "$dir" ]; then exit 1; fi
    umask 077
    /bin/mkdir -m 0700 "$dir"
    /usr/sbin/chown root:wheel "$dir"
    /usr/bin/printf "%s\n" "$token" >"$marker"
    /usr/sbin/chown root:wheel "$marker"
    /bin/chmod 0600 "$marker"
' sh "$guest_dir" "$marker" "$token" >>"$out/guest-agent.log" 2>&1 || die "could not create private guest staging"

guest_installer=$guest_dir/nix-installer
guest_inside=$guest_dir/inside.sh
bounded_exec 60 "$installer" /usr/bin/sudo -n /bin/sh -c 'set -eu; umask 077; /bin/cat >"$1"; /usr/sbin/chown root:wheel "$1"; /bin/chmod 0700 "$1"' sh "$guest_installer" || die "could not stream installer into guest"
bounded_exec 15 /dev/null /usr/bin/sudo -n /usr/bin/shasum -a 256 "$guest_installer" >"$out/installer.guest.sha256.line" || die "could not hash staged installer"
guest_installer_sha=$(awk '{print $1}' "$out/installer.guest.sha256.line")
case $guest_installer_sha in ''|*[!0-9a-f]*) die "staged installer digest is not hexadecimal" ;; esac
[ "${#guest_installer_sha}" -eq 64 ] || die "staged installer digest is not 64 hexadecimal characters"
printf '%s\n' "$guest_installer_sha" >"$out/installer.guest.sha256"
[ "$guest_installer_sha" = "$installer_pin" ] || die "staged installer digest mismatch"
bounded_exec 30 "$out/inside.sh" /usr/bin/sudo -n /bin/sh -c 'set -eu; umask 077; /bin/cat >"$1"; /usr/sbin/chown root:wheel "$1"; /bin/chmod 0700 "$1"' sh "$guest_inside" || die "could not stream inside.sh into guest"
bounded_exec 15 /dev/null /usr/bin/sudo -n /usr/bin/shasum -a 256 "$guest_inside" >"$out/inside.actual.sha256.line" || die "could not hash staged inside.sh"
guest_inside_sha=$(awk '{print $1}' "$out/inside.actual.sha256.line")
case $guest_inside_sha in ''|*[!0-9a-f]*) die "staged inside.sh digest is not hexadecimal" ;; esac
[ "${#guest_inside_sha}" -eq 64 ] || die "staged inside.sh digest is not 64 hexadecimal characters"
printf '%s\n' "$guest_inside_sha" >"$out/inside.actual.sha256"
[ "$guest_inside_sha" = "$inside_sha" ] || die "staged inside.sh digest mismatch"

mkdir -m 0700 "$out/phases" "$out/reboots"
printf 'PASS\n' >"$out/phases/phase-status.expected"
printf 'FAIL\n' >"$out/phases/phase-status.fail.expected"
validate_phase_archive() {
    phase=$1
    validation_archive=$2
    list=$out/phases/$phase.list
    verbose=$out/phases/$phase.verbose
    validation_stderr=$out/phases/$phase.validation.stderr
    archive_size=$(wc -c <"$validation_archive" | tr -d ' ')
    case $archive_size in ''|*[!0-9]*) die "could not determine phase archive size: $phase" ;; esac
    [ "$archive_size" -gt 0 ] && [ "$archive_size" -le 268435456 ] || die "phase archive size is invalid: $phase"
    bounded_host 30 /usr/bin/tar -tf "$validation_archive" >"$list" 2>"$validation_stderr" || die "could not list phase archive: $phase"
    bounded_host 30 /usr/bin/tar -tvf "$validation_archive" >"$verbose" 2>>"$validation_stderr" || die "could not inspect phase archive types: $phase"
    [ -s "$list" ] || die "phase archive is empty: $phase"
    while IFS= read -r entry; do
        case $entry in /*) die "phase archive has an absolute path: $entry" ;; esac
        case $entry in "$phase/"*) ;; *) die "phase archive has an unexpected prefix: $entry" ;; esac
        case $entry in *[!A-Za-z0-9._/-]*) die "phase archive has an unsafe name: $entry" ;; esac
        checked_entry=${entry%/}
        case "/$checked_entry/" in *'/../'*|*'/./'*|*'//'*) die "phase archive has an unsafe path: $entry" ;; esac
        case $checked_entry in */receipt.json) die "phase archive contains receipt bytes" ;; esac
    done <"$list"
    sed 's|/$||' "$list" | LC_ALL=C sort >"$out/phases/$phase.sorted"
    uniq -d "$out/phases/$phase.sorted" >"$out/phases/$phase.duplicates"
    [ ! -s "$out/phases/$phase.duplicates" ] || die "phase archive has duplicate paths: $phase"
    while IFS= read -r detail; do
        type=${detail%"${detail#?}"}
        case $type in -|d) ;; *) die "phase archive contains a link or special entry: $phase" ;; esac
    done <"$verbose"
    [ "$(wc -l <"$list" | tr -d ' ')" = "$(wc -l <"$verbose" | tr -d ' ')" ] || die "phase archive manifests disagree: $phase"
    grep -Fx -- "$phase/phase-status" "$list" >/dev/null || die "phase archive lacks phase-status: $phase"
    grep -Fx -- "$phase/phase-ledger" "$list" >/dev/null || die "phase archive lacks phase-ledger: $phase"
    bounded_host 30 /usr/bin/tar -xOf "$validation_archive" "$phase/phase-status" >"$out/phases/$phase.phase-status" 2>>"$validation_stderr" || die "could not read phase-status: $phase"
    bounded_host 30 /usr/bin/tar -xOf "$validation_archive" "$phase/phase-ledger" >"$out/phases/$phase.phase-ledger" 2>>"$validation_stderr" || die "could not read phase-ledger: $phase"
    if cmp -s "$out/phases/phase-status.expected" "$out/phases/$phase.phase-status"; then :
    elif cmp -s "$out/phases/phase-status.fail.expected" "$out/phases/$phase.phase-status"; then :
    else die "phase-status is not exactly PASS or FAIL: $phase"
    fi
    awk -v target="$phase" '
        $0 != "reboot" { print }
        $0 == target { found=1; exit }
        END { if (!found) exit 1 }
    ' "$out/phase-sequence" >"$out/phases/$phase.phase-ledger.expected" || die "phase is absent from the lane sequence: $phase"
    cmp -s "$out/phases/$phase.phase-ledger.expected" "$out/phases/$phase.phase-ledger" || die "phase-ledger does not match the expected lane prefix: $phase"
    printf '%s\n' "$archive_size" >"$out/phases/$phase.size"
}
capture_phase() {
    phase=$1
    archive_part=$out/phases/$phase.tar.part
    archive=$out/phases/$phase.tar
    bounded_exec 120 /dev/null /usr/bin/sudo -n /usr/bin/tar -cf - -C "$guest_evidence" "$phase" >"$archive_part" 2>"$out/phases/$phase.capture.stderr" || die "could not capture phase evidence: $phase"
    validate_phase_archive "$phase" "$archive_part"
    sha256 "$archive_part" >"$out/phases/$phase.tar.sha256"
    /bin/mv "$archive_part" "$archive" || die "could not finalize phase archive: $phase"
}
run_phase() {
    phase=$1
    approval=${2-}
    set +e
    if [ -n "$approval" ]; then
        write_argv "$out/phases/$phase.argv" /usr/bin/sudo -n "$guest_inside" "$phase" "$token" "$marker" "$guest_installer" "$installer_pin" "$guest_evidence" "$approval"
        bounded_exec 9000 /dev/null /usr/bin/sudo -n "$guest_inside" "$phase" "$token" "$marker" "$guest_installer" "$installer_pin" "$guest_evidence" "$approval" >"$out/phases/$phase.output" 2>&1
        guest_status=$?
    else
        write_argv "$out/phases/$phase.argv" /usr/bin/sudo -n "$guest_inside" "$phase" "$token" "$marker" "$guest_installer" "$installer_pin" "$guest_evidence"
        bounded_exec 9000 /dev/null /usr/bin/sudo -n "$guest_inside" "$phase" "$token" "$marker" "$guest_installer" "$installer_pin" "$guest_evidence" >"$out/phases/$phase.output" 2>&1
        guest_status=$?
    fi
    set -e
    printf '%s\n' "$guest_status" >"$out/phases/$phase.guest-status"
    capture_phase "$phase"
    case $phase:$guest_status in
        foreign-refuse:20)
            cmp -s "$out/phases/phase-status.expected" "$out/phases/$phase.phase-status" || die "guest status 20 does not match phase-status: $phase"
            ;;
        foreign-refuse:0)
            cmp -s "$out/phases/phase-status.expected" "$out/phases/$phase.phase-status" || die "guest status 0 does not match phase-status: $phase"
            die "foreign-refuse did not return status 20"
            ;;
        *:0)
            cmp -s "$out/phases/phase-status.expected" "$out/phases/$phase.phase-status" || die "guest status 0 does not match phase-status: $phase"
            ;;
        *)
            cmp -s "$out/phases/phase-status.fail.expected" "$out/phases/$phase.phase-status" || die "failed guest status does not match phase-status: $phase"
            die "guest phase failed with status $guest_status: $phase"
            ;;
    esac
}
wait_guest_ready() {
    ready=0
    i=0
    while [ "$i" -lt 150 ]; do
        if bounded_exec 1 /dev/null /usr/bin/true >>"$out/guest-agent.log" 2>&1; then ready=1; break; fi
        kill -0 "$run_pid" 2>/dev/null || die "Tart VM exited during guest reboot"
        i=$((i + 1))
        sleep 2
    done
    [ "$ready" -eq 1 ] || die "Guest Agent did not return after reboot"
    kill -0 "$run_pid" 2>/dev/null || die "Tart VM exited before post-reboot readiness was accepted"
}
revalidate_guest() {
    label=$1
    bounded_exec 30 /dev/null /usr/bin/sudo -n /bin/sh -c '
        set -eu
        marker=$1 token=$2 installer=$3 installer_sha=$4 inside=$5 inside_sha=$6
        [ -f "$marker" ] && [ ! -L "$marker" ]
        [ "$(/usr/bin/stat -f "%Su:%Sg:%Lp" "$marker")" = root:wheel:600 ]
        [ "$(/bin/cat "$marker")" = "$token" ]
        installer_line=$(/usr/bin/shasum -a 256 "$installer")
        inside_line=$(/usr/bin/shasum -a 256 "$inside")
        [ "${installer_line%% *}" = "$installer_sha" ]
        [ "${inside_line%% *}" = "$inside_sha" ]
        /usr/bin/printf "installer=%s\ninside=%s\n" "${installer_line%% *}" "${inside_line%% *}"
    ' sh "$marker" "$token" "$guest_installer" "$installer_pin" "$guest_inside" "$inside_sha" >"$out/reboots/$label.revalidation" 2>&1 || die "guest identity failed after reboot: $label"
}
reboot_guest() {
    label=$1
    bounded_exec 15 /dev/null /usr/sbin/sysctl -n kern.boottime >"$out/reboots/$label.before" 2>&1 || die "could not record pre-reboot kern.boottime"
    set +e
    bounded_exec 30 /dev/null /usr/bin/sudo -n /sbin/shutdown -r now >>"$out/reboots/$label.shutdown" 2>&1
    shutdown_status=$?
    set -e
    printf '%s\n' "$shutdown_status" >"$out/reboots/$label.shutdown.status"
    down=0
    i=0
    while [ "$i" -lt 60 ]; do
        if bounded_exec 1 /dev/null /usr/bin/true >/dev/null 2>&1; then :; else down=1; break; fi
        i=$((i + 1))
        sleep 1
    done
    [ "$down" -eq 1 ] || die "Guest Agent did not become unavailable for reboot"
    wait_guest_ready
    bounded_exec 15 /dev/null /usr/bin/sudo -n /usr/bin/true >>"$out/guest-agent.log" 2>&1 || die "passwordless guest sudo did not return after reboot"
    revalidate_guest "$label"
    bounded_exec 15 /dev/null /usr/sbin/sysctl -n kern.boottime >"$out/reboots/$label.after" 2>&1 || die "could not record post-reboot kern.boottime"
    if cmp -s "$out/reboots/$label.before" "$out/reboots/$label.after"; then
        die "raw kern.boottime did not change across reboot"
    else
        reboot_cmp_status=$?
    fi
    [ "$reboot_cmp_status" -eq 1 ] || die "could not compare raw kern.boottime across reboot"
    return 0
}

case $lane in
    lifecycle-diagnostics)
        run_phase baseline
        run_phase lifecycle-install
        reboot_guest after-install
        run_phase lifecycle-post-reboot
        run_phase lifecycle-repeat-install
        run_phase lifecycle-repair
        run_phase lifecycle-daemon
        run_phase lifecycle-uninstall
        run_phase lifecycle-repeat-uninstall
        reboot_guest after-uninstall
        run_phase lifecycle-residue
        ;;
    crash-recovery)
        run_phase baseline
        run_phase crash-kill
        reboot_guest after-crash
        run_phase crash-recover
        ;;
    foreign-nix)
        run_phase baseline
        run_phase foreign-synthetic-prepare
        reboot_guest after-foreign-prepare
        run_phase foreign-post-reboot
        run_phase foreign-refuse
        [ -z "$foreign_approval" ] || run_phase foreign-observe "$foreign_approval"
        ;;
    upstream-input)
        run_phase baseline
        run_phase upstream-install
        run_phase upstream-determinate-attempt
        ;;
esac
private_tree "$out"
success=1
