#!/bin/sh
set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
export PATH
DETSYS_IDS_TELEMETRY=disabled
diagnostic_endpoint=http://127.0.0.1:18080
export DETSYS_IDS_TELEMETRY
umask 077

phase_dir=
active_vendor_pid=
strict_vendor_failed=0
strict_product_failed=0
die() {
    set +e
    if [ -n "$phase_dir" ] && [ -d "$phase_dir" ] && [ ! -L "$phase_dir" ]; then
        printf '%s\n' "FAIL: $*" >>"$phase_dir/results"
    fi
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}
sha256() {
    sha256_line=$(shasum -a 256 <"$1") || return 1
    printf '%s\n' "${sha256_line%% *}"
}
record() { printf '%s: %s\n' "$1" "$2" >>"$phase_dir/results"; }
write_argv() { argv_file=$1; shift; : >"$argv_file"; for argv_item in "$@"; do printf '%s\n' "$argv_item" >>"$argv_file"; done; }
path_exists() { [ -e "$1" ] || [ -L "$1" ]; }

path_hex() {
    LC_ALL=C printf %s "$1" | /usr/bin/od -An -v -tx1 | /usr/bin/tr -d ' \n'
}
stat_state() {
    stat_path=$1
    stat_state_line=$(LC_ALL=C /usr/bin/stat -f '%d:%i:%p:%u:%g:%z:%l:%m:%c:%Lp:%HT' "$stat_path") || die "could not inspect path identity"
    stat_saved_ifs=$IFS
    IFS=:
    read -r stat_device stat_inode stat_full_mode stat_uid stat_gid stat_size stat_nlink stat_mtime stat_ctime stat_mode stat_type <<EOF
$stat_state_line
EOF
    IFS=$stat_saved_ifs
    case $stat_device:$stat_inode:$stat_full_mode:$stat_uid:$stat_gid:$stat_size:$stat_nlink:$stat_mtime:$stat_ctime in
        *[!0-9:]*) die "path identity contains a non-numeric field" ;;
    esac
    case $stat_mode in ''|*[!0-7]*) die "path mode is not octal" ;; esac
}
inventory_entries() {
    shift
    inventory_root=$1 inventory_root_device=$2 inventory_output=$3
    shift 3
    case $inventory_root_device in ''|*[!0-9]*) die "inventory root device is invalid" ;; esac
    case $inventory_output in /*) ;; *) die "inventory output path is not absolute" ;; esac
    [ -f "$inventory_output" ] && [ ! -L "$inventory_output" ] || die "inventory output is unsafe"
    [ "$(LC_ALL=C /usr/bin/stat -f '%u:%Lp' "$inventory_output")" = "$(id -u):600" ] || die "inventory output is not private"
    inventory_link_index=0
    for inventory_path do
        case $inventory_path in "$inventory_root"|"$inventory_root"/*) ;; *) die "inventory path escaped its root" ;; esac
        stat_state "$inventory_path"
        inventory_before=$stat_state_line
        inventory_device=$stat_device inventory_mode=$stat_mode inventory_uid=$stat_uid inventory_gid=$stat_gid
        inventory_size=$stat_size inventory_nlink=$stat_nlink inventory_type=$stat_type
        [ "$inventory_device" = "$inventory_root_device" ] || die "inventory path crossed a device boundary"
        inventory_sha=- inventory_target_hex=-
        case $inventory_type in
            Directory) inventory_kind=d ;;
            'Regular File')
                inventory_kind=f
                [ "$inventory_nlink" -eq 1 ] || die "regular inventory file has multiple hard links"
                inventory_sha=$(sha256 "$inventory_path") || die "could not hash regular inventory file"
                case $inventory_sha in *[!0-9a-f]*|'') die "regular inventory file hash is invalid" ;; esac
                [ "${#inventory_sha}" -eq 64 ] || die "regular inventory file hash length is invalid"
                ;;
            'Symbolic Link')
                inventory_kind=l
                [ "$inventory_nlink" -eq 1 ] || die "inventory symlink has multiple hard links"
                inventory_link_index=$((inventory_link_index + 1))
                inventory_link_output=$inventory_output.link.$$.$inventory_link_index
                [ ! -e "$inventory_link_output" ] && [ ! -L "$inventory_link_output" ] || die "inventory readlink temporary path exists"
                trap '/bin/rm -f "$inventory_link_output"' EXIT HUP INT TERM
                LC_ALL=C /usr/bin/readlink "$inventory_path" >"$inventory_link_output" || die "could not read inventory symlink"
                inventory_link_size=$(wc -c <"$inventory_link_output" | /usr/bin/tr -d ' ')
                case $inventory_link_size in ''|*[!0-9]*) die "inventory readlink length is invalid" ;; esac
                [ "$inventory_link_size" -eq $((inventory_size + 1)) ] || die "inventory readlink length differs from lstat size"
                inventory_link_hex=$(LC_ALL=C /usr/bin/od -An -v -tx1 "$inventory_link_output" | /usr/bin/tr -d ' \n')
                [ "${#inventory_link_hex}" -eq $((inventory_link_size * 2)) ] || die "inventory readlink hex length is invalid"
                case $inventory_link_hex in *0a) inventory_target_hex=${inventory_link_hex%0a} ;; *) die "inventory readlink output lacks its delimiter" ;; esac
                [ "${#inventory_target_hex}" -eq $((inventory_size * 2)) ] || die "inventory symlink target hex differs from lstat size"
                /bin/rm -f "$inventory_link_output" || die "could not remove inventory readlink temporary file"
                trap - EXIT HUP INT TERM
                ;;
            *) die "inventory contains an unsupported file type" ;;
        esac
        stat_state "$inventory_path"
        [ "$stat_state_line" = "$inventory_before" ] || die "inventory path changed while it was inspected"
        inventory_path_hex=$(path_hex "$inventory_path") || die "could not encode inventory path"
        printf 'path_hex=%s type=%s mode=%s uid=%s gid=%s size=%s nlink=%s sha256=%s target_hex=%s\n' \
            "$inventory_path_hex" "$inventory_kind" "$inventory_mode" "$inventory_uid" "$inventory_gid" \
            "$inventory_size" "$inventory_nlink" "$inventory_sha" "$inventory_target_hex" >>"$inventory_output"
    done
}

if [ "${1-}" = --inventory-entries ]; then
    [ "$#" -ge 5 ] || die "inventory scanner requires a root, device, output, and path"
    inventory_entries "$@"
    exit 0
fi

wait_bounded() {
    wait_limit=$1 wait_child=$2 wait_elapsed=0
    while kill -0 "$wait_child" 2>/dev/null; do
        if [ "$wait_elapsed" -ge "$wait_limit" ]; then
            kill -TERM "$wait_child" 2>/dev/null || :
            wait_grace=0
            while kill -0 "$wait_child" 2>/dev/null && [ "$wait_grace" -lt 5 ]; do sleep 1; wait_grace=$((wait_grace + 1)); done
            if kill -0 "$wait_child" 2>/dev/null; then kill -KILL "$wait_child" 2>/dev/null || :; fi
            signals_hold
            wait "$wait_child" 2>/dev/null || :
            if [ "$active_vendor_pid" = "$wait_child" ]; then active_vendor_pid=; fi
            signals_restore
            return 124
        fi
        sleep 1
        wait_elapsed=$((wait_elapsed + 1))
    done
    signals_hold
    if wait "$wait_child"; then wait_status=0; else wait_status=$?; fi
    if [ "$active_vendor_pid" = "$wait_child" ]; then active_vendor_pid=; fi
    signals_restore
    return "$wait_status"
}
signals_hold() { trap '' HUP INT TERM; }
signals_restore() {
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
}
cleanup_children() {
    signals_hold
    [ -n "$active_vendor_pid" ] || return 0
    if kill -0 "$active_vendor_pid" 2>/dev/null; then
        kill -TERM "$active_vendor_pid" 2>/dev/null || :
        cleanup_grace=0
        while kill -0 "$active_vendor_pid" 2>/dev/null && [ "$cleanup_grace" -lt 5 ]; do sleep 1; cleanup_grace=$((cleanup_grace + 1)); done
        if kill -0 "$active_vendor_pid" 2>/dev/null; then kill -KILL "$active_vendor_pid" 2>/dev/null || :; fi
    fi
    wait "$active_vendor_pid" 2>/dev/null || :
    active_vendor_pid=
}
run_recorded() {
    run_name=$1 run_limit=$2
    shift 2
    write_argv "$phase_dir/$run_name.argv" "$@"
    signals_hold
    (umask 022; exec "$@") </dev/null >"$phase_dir/$run_name.output" 2>&1 &
    run_pid=$!
    active_vendor_pid=$run_pid
    signals_restore
    set +e
    wait_bounded "$run_limit" "$run_pid"
    last_status=$?
    set -e
    printf '%s\n' "$last_status" >"$phase_dir/$run_name.status"
}
finalize_exit() {
    original_status=$?
    trap - EXIT
    trap '' HUP INT TERM
    set +e
    if [ "$original_status" -ne 0 ] && [ -n "$phase_dir" ] && [ -d "$phase_dir" ] && [ ! -L "$phase_dir" ]; then
        status_file=$phase_dir/phase-status
        status_size=
        if [ -f "$status_file" ] && [ ! -L "$status_file" ]; then status_size=$(wc -c <"$status_file" 2>/dev/null); fi
        if ! { [ "$status_size" -eq 5 ] 2>/dev/null && grep -Fx PASS "$status_file" >/dev/null 2>&1; }; then
            [ ! -L "$status_file" ] && printf '%s\n' FAIL >"$status_file"
            ledger_copy=$phase_dir/.phase-ledger-copy
            if [ -n "${ledger-}" ] && [ -f "$ledger" ] && [ ! -L "$ledger" ] \
                && [ ! -L "$phase_dir/phase-ledger" ] && [ ! -e "$ledger_copy" ] && [ ! -L "$ledger_copy" ]; then
                if cp "$ledger" "$ledger_copy" && chmod 0600 "$ledger_copy" && cmp -s "$ledger" "$ledger_copy" \
                    && mv -f "$ledger_copy" "$phase_dir/phase-ledger"; then
                    :
                else
                    rm -f "$ledger_copy"
                fi
            fi
        fi
    fi
    cleanup_children
    exit "$original_status"
}
trap finalize_exit EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

receipt_identity() {
    receipt_name=$1 receipt=/nix/receipt.json
    [ -f "$receipt" ] && [ ! -L "$receipt" ] || die "receipt is not a regular non-symlink file"
    stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' "$receipt" >"$phase_dir/$receipt_name.stat"
    stat -f %z "$receipt" >"$phase_dir/$receipt_name.size"
    sha256 "$receipt" >"$phase_dir/$receipt_name.sha256"
}
capture_inventory_once() {
    inventory_file=$1 inventory_raw=$1.raw
    : >"$inventory_raw"
    if ! path_exists /etc/nix; then
        printf '%s\n' 'state=absent path_hex=2f6574632f6e6978' >"$inventory_file"
        /bin/rm -f "$inventory_raw"
        return 0
    fi
    case $0 in /*) ;; *) die "inventory scanner path is not absolute" ;; esac
    stat_state /etc/nix
    [ "$stat_type" = Directory ] || die "/etc/nix inventory root is not a real directory"
    inventory_root_before=$stat_state_line inventory_root_device=$stat_device
    LC_ALL=C /usr/bin/find -P /etc/nix -xdev -exec /bin/sh "$0" --inventory-entries /etc/nix "$inventory_root_device" "$inventory_raw" {} + || die "could not inventory /etc/nix"
    stat_state /etc/nix
    [ "$stat_state_line" = "$inventory_root_before" ] || die "/etc/nix changed during inventory"
    LC_ALL=C /usr/bin/sort "$inventory_raw" >"$inventory_file" || die "could not sort /etc/nix inventory"
    /bin/rm -f "$inventory_raw" || die "could not remove raw /etc/nix inventory"
}
capture_fixed_identity() {
    identity_path=$1 identity_file=$2
    identity_path_hex=$(path_hex "$identity_path") || die "could not encode fixed identity path"
    if ! path_exists "$identity_path"; then
        printf 'state=absent path_hex=%s type=- mode=- uid=- gid=- size=- nlink=- sha256=-\n' "$identity_path_hex" >"$identity_file"
        return 0
    fi
    stat_state "$identity_path"
    identity_before=$stat_state_line identity_mode=$stat_mode identity_uid=$stat_uid identity_gid=$stat_gid
    identity_size=$stat_size identity_nlink=$stat_nlink
    [ "$stat_type" = 'Regular File' ] || die "fixed identity path is not a non-symlink regular file"
    [ "$identity_nlink" -eq 1 ] || die "fixed identity file has multiple hard links"
    identity_sha=$(sha256 "$identity_path") || die "could not hash fixed identity file"
    case $identity_sha in *[!0-9a-f]*|'') die "fixed identity hash is invalid" ;; esac
    [ "${#identity_sha}" -eq 64 ] || die "fixed identity hash length is invalid"
    stat_state "$identity_path"
    [ "$stat_state_line" = "$identity_before" ] || die "fixed identity changed while it was inspected"
    printf 'state=present path_hex=%s type=f mode=%s uid=%s gid=%s size=%s nlink=%s sha256=%s\n' \
        "$identity_path_hex" "$identity_mode" "$identity_uid" "$identity_gid" "$identity_size" "$identity_nlink" "$identity_sha" >"$identity_file"
}
capture_residue_contract_once() {
    residue_stem=$1
    capture_inventory_once "$residue_stem.etc-nix.inventory"
    capture_fixed_identity /etc/fstab "$residue_stem.fstab.identity"
}
capture_residue_contract() {
    residue_prefix=$1 residue_first=$1.residue-scan-1 residue_second=$1.residue-scan-2
    capture_residue_contract_once "$residue_first"
    capture_residue_contract_once "$residue_second"
    for residue_suffix in etc-nix.inventory fstab.identity; do
        /usr/bin/cmp -s "$residue_first.$residue_suffix" "$residue_second.$residue_suffix" || die "residue identity was not stable across two scans: $residue_suffix"
    done
    for residue_suffix in etc-nix.inventory fstab.identity; do
        /bin/mv "$residue_first.$residue_suffix" "$residue_prefix.$residue_suffix" || die "could not finalize residue identity: $residue_suffix"
        /bin/rm -f "$residue_second.$residue_suffix" || die "could not remove second residue scan: $residue_suffix"
    done
    capture_fixed_identity /var/log/determinate-nix-init.log "$residue_prefix.determinate-nix-init-log.identity"
    capture_fixed_identity /var/log/determinate-nix-daemon.log "$residue_prefix.determinate-nix-daemon-log.identity"
}
compare_residue_contract() {
    contract_left=$1 contract_right=$2 contract_reason=$3
    for contract_suffix in etc-nix.inventory fstab.identity determinate-nix-init-log.identity determinate-nix-daemon-log.identity; do
        /usr/bin/cmp -s "$contract_left.$contract_suffix" "$contract_right.$contract_suffix" || die "$contract_reason: $contract_suffix"
    done
}
stable_log_identity() {
    (
        stable_log_canonical=$1.canonical.$$
        [ ! -e "$stable_log_canonical" ] && [ ! -L "$stable_log_canonical" ] || exit 1
        trap '/bin/rm -f "$stable_log_canonical"' EXIT
        trap 'exit 129' HUP
        trap 'exit 130' INT
        trap 'exit 143' TERM
        (umask 077; set -C; : >"$stable_log_canonical") 2>/dev/null || exit 1
        [ "$(wc -l <"$1" | /usr/bin/tr -d ' ')" -eq 1 ] || exit 1
        LC_ALL=C awk '
            NF != 9 || $2 !~ /^path_hex=[0-9a-f]+$/ { exit 1 }
            $1 == "state=absent" {
                if ($3 != "type=-" || $4 != "mode=-" || $5 != "uid=-" || $6 != "gid=-" || $7 != "size=-" || $8 != "nlink=-" || $9 != "sha256=-") exit 1
                valid=1
                next
            }
            $1 == "state=present" {
                if ($3 != "type=f" || $4 !~ /^mode=[0-7]+$/ || $5 !~ /^uid=[0-9]+$/ || $6 !~ /^gid=[0-9]+$/ || $7 !~ /^size=[0-9]+$/ || $8 !~ /^nlink=1$/ || $9 !~ /^sha256=[0-9a-f]+$/ || length($9) != 71) exit 1
                valid=1
                next
            }
            { exit 1 }
            END {
                if (NR != 1 || !valid) exit 1
                print $1, $2, $3, $4, $5, $6, $7, $8, $9
            }
        ' "$1" >"$stable_log_canonical" || exit 1
        /usr/bin/cmp -s "$1" "$stable_log_canonical" || exit 1
        if LC_ALL=C awk '{ print $1, $2, $3, $4, $5, $6, $8 }' "$stable_log_canonical"; then stable_log_status=0; else stable_log_status=$?; fi
        /bin/rm -f "$stable_log_canonical" || exit 1
        trap - EXIT
        exit "$stable_log_status"
    )
}
compare_active_residue_contract() {
    active_left=$1 active_right=$2 active_reason=$3
    for active_suffix in etc-nix.inventory fstab.identity; do
        /usr/bin/cmp -s "$active_left.$active_suffix" "$active_right.$active_suffix" || die "$active_reason: $active_suffix"
    done
    for active_suffix in determinate-nix-init-log.identity determinate-nix-daemon-log.identity; do
        active_left_identity=$(stable_log_identity "$active_left.$active_suffix") || die "invalid active log identity: $active_suffix"
        active_right_identity=$(stable_log_identity "$active_right.$active_suffix") || die "invalid active log identity: $active_suffix"
        [ "$active_left_identity" = "$active_right_identity" ] || die "$active_reason: $active_suffix"
    done
}
identity_is_exact() {
    identity_count=$(grep -F -x -c -- "$2" "$1" || :)
    [ "$identity_count" -eq 1 ] && [ "$(wc -l <"$1" | /usr/bin/tr -d ' ')" -eq 1 ]
}
require_clean_residue_contract() {
    clean_prefix=$1
    identity_is_exact "$clean_prefix.etc-nix.inventory" 'state=absent path_hex=2f6574632f6e6978' || die "clean snapshot contains /etc/nix"
    identity_is_exact "$clean_prefix.fstab.identity" 'state=absent path_hex=2f6574632f6673746162 type=- mode=- uid=- gid=- size=- nlink=- sha256=-' || die "clean snapshot contains /etc/fstab"
    identity_is_exact "$clean_prefix.determinate-nix-init-log.identity" 'state=absent path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d696e69742e6c6f67 type=- mode=- uid=- gid=- size=- nlink=- sha256=-' || die "clean snapshot contains the Determinate init log"
    identity_is_exact "$clean_prefix.determinate-nix-daemon-log.identity" 'state=absent path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d6461656d6f6e2e6c6f67 type=- mode=- uid=- gid=- size=- nlink=- sha256=-' || die "clean snapshot contains the Determinate daemon log"
}
require_installed_residue_contract() {
    installed_prefix=$1
    grep -E '^path_hex=2f6574632f6e6978 type=d mode=[0-7]+ uid=[0-9]+ gid=[0-9]+ size=[0-9]+ nlink=[0-9]+ sha256=- target_hex=-$' "$installed_prefix.etc-nix.inventory" >/dev/null || die "installed snapshot lacks the /etc/nix root directory"
    grep -E '^state=present path_hex=2f6574632f6673746162 type=f mode=[0-7]+ uid=[0-9]+ gid=[0-9]+ size=[0-9]+ nlink=1 sha256=[0-9a-f]{64}$' "$installed_prefix.fstab.identity" >/dev/null || die "installed snapshot lacks stable /etc/fstab identity"
    grep -E '^state=present path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d696e69742e6c6f67 type=f mode=[0-7]+ uid=[0-9]+ gid=[0-9]+ size=[0-9]+ nlink=1 sha256=[0-9a-f]{64}$' "$installed_prefix.determinate-nix-init-log.identity" >/dev/null || die "installed snapshot lacks the Determinate init log"
    grep -E '^state=present path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d6461656d6f6e2e6c6f67 type=f mode=[0-7]+ uid=[0-9]+ gid=[0-9]+ size=[0-9]+ nlink=1 sha256=[0-9a-f]{64}$' "$installed_prefix.determinate-nix-daemon-log.identity" >/dev/null || die "installed snapshot lacks the Determinate daemon log"
}
snapshot() {
    snapshot_name=$1 snapshot_prefix=$phase_dir/$snapshot_name
    sw_vers >"$snapshot_prefix.platform" || die "could not record macOS version"
    uname -a >>"$snapshot_prefix.platform" || die "could not record kernel version"
    printf 'console-user=%s\n' "$console_user" >>"$snapshot_prefix.platform"
    snapshot_boot=$(sysctl -n kern.boottime) || die "could not record raw kernel boot time"
    printf 'kern-boottime=%s\n' "$snapshot_boot" >>"$snapshot_prefix.platform"
    capture_residue_contract "$snapshot_prefix"
    : >"$snapshot_prefix.paths"
    for snapshot_path in /nix /nix/receipt.json /nix/nix-installer /etc/nix /usr/local/bin/determinate-nixd /etc/fstab /etc/synthetic.conf /opt/pkg '/Library/Application Support/pkg'; do
        if [ -L "$snapshot_path" ]; then
            stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' "$snapshot_path" >>"$snapshot_prefix.paths"
        elif [ -e "$snapshot_path" ]; then
            stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' "$snapshot_path" >>"$snapshot_prefix.paths"
        else printf 'absent path=%s\n' "$snapshot_path" >>"$snapshot_prefix.paths"; fi
    done
    diskutil apfs list >"$snapshot_prefix.apfs" 2>&1 || die "could not record APFS state"
    mount >"$snapshot_prefix.mounts" 2>&1 || die "could not record mounts"
    : >"$snapshot_prefix.config"
    for config_file in /etc/synthetic.conf; do
        if [ -f "$config_file" ] && [ ! -L "$config_file" ]; then grep -Ein '(^|[[:space:]/])(nix|Nix Store)([[:space:]/]|$)' "$config_file" >>"$snapshot_prefix.config" || :; fi
    done
    : >"$snapshot_prefix.launchd-files"
    for launch_dir in /Library/LaunchDaemons /Library/LaunchAgents; do
        [ -d "$launch_dir" ] || continue
        find "$launch_dir" -maxdepth 1 \( -type f -o -type l \) \( -iname '*nix*' -o -iname '*determinate*' -o -iname '*pkg*' \) -print >>"$snapshot_prefix.launchd-files" || die "could not record launchd files"
    done
    launchctl print system >"$snapshot_prefix.launchd-system" 2>&1 || die "could not record system launchd"
    grep -Ei '(^|[^[:alnum:]_])(nix|determinate|pkg)([^[:alnum:]_]|$)' "$snapshot_prefix.launchd-system" >"$snapshot_prefix.launchd-jobs" || :
    set +e
    security find-generic-password -a 'Nix Store' -s 'Nix Store' /Library/Keychains/System.keychain >/dev/null 2>&1
    snapshot_keychain_status=$?
    set -e
    case $snapshot_keychain_status in 0) printf '%s\n' present >"$snapshot_prefix.keychain" ;; 44) printf '%s\n' absent >"$snapshot_prefix.keychain" ;; *) die "System Keychain metadata probe failed: $snapshot_keychain_status" ;; esac
    dscl . -list /Groups >"$snapshot_prefix.groups.all" || die "could not record local groups"
    grep -E '^(nixbld|_nixbld|_?pkg)$' "$snapshot_prefix.groups.all" >"$snapshot_prefix.groups" || :
    dscl . -list /Users >"$snapshot_prefix.users.all" || die "could not record local users"
    grep -E '^(_?nixbld[0-9]+|_?pkg)$' "$snapshot_prefix.users.all" >"$snapshot_prefix.users" || :
    find /var/run /private/var/run -xdev -type s \( -iname '*nix*' -o -iname '*determinate*' -o -iname '*pkg*' \) -print 2>/dev/null >"$snapshot_prefix.sockets" || die "could not record sockets"
}
record_free_disk() {
    df -Pk / >"$phase_dir/guest-disk.df" || die "could not record guest free disk"
    vendor_available_kb=$(awk 'END {print $4}' "$phase_dir/guest-disk.df") || die "could not parse guest free disk"
    case $vendor_available_kb in ''|*[!0-9]*) die "could not determine guest free disk" ;; esac
    printf '%s\n' "$vendor_available_kb" >"$phase_dir/vendor-free-kb"
}
require_first_vendor_gates() {
    [ "$vendor_available_kb" -ge 31457280 ] || die "at least 30 GiB of guest free disk is required before first vendor execution"
    case $console_user in ''|root|loginwindow|_mbsetupuser) die "a real graphical console user is required before first vendor execution" ;; esac
    id "$console_user" >/dev/null 2>&1 || die "graphical console user does not exist"
    console_uid=$(id -u "$console_user") console_gid=$(id -g "$console_user")
    secure_token_state=$(sysadminctl -secureTokenStatus "$console_user" 2>&1) || die "could not read console secure-token state"
    printf '%s\n' "$secure_token_state" | grep -F 'Secure token is ENABLED' >/dev/null || die "graphical console user lacks a secure token"
    SUDO_USER=$console_user SUDO_UID=$console_uid SUDO_GID=$console_gid
    export SUDO_USER SUDO_UID SUDO_GID
}
require_installer_version() {
    run_recorded installer-version 60 "$staged" --version
    [ "$last_status" -eq 0 ] || die "installer version command failed"
    grep -F '3.22.1' "$phase_dir/installer-version.output" >/dev/null || die "installer version is not 3.22.1"
}
require_functional_nix() {
    nix_bin=/nix/var/nix/profiles/default/bin/nix
    [ -x "$nix_bin" ] || die "installed Nix executable is missing"
    run_recorded nix-version 60 "$nix_bin" --version
    [ "$last_status" -eq 0 ] || die "installed Nix version command failed"
    run_recorded nix-daemon-ping 120 "$nix_bin" store ping --store daemon
    [ "$last_status" -eq 0 ] || die "installed Nix daemon is not functional"
}
require_reboot_since() {
    reboot_phase=$1
    previous_boot=$(cat "$evidence/$reboot_phase/boot-session")
    [ "$current_boot" != "$previous_boot" ] || die "required reboot after $reboot_phase was not observed"
}
content_hex() {
    od -An -tx1 "$1" >"$phase_dir/content-hex.raw" || die "could not read fixture bytes"
    tr -d ' \n' <"$phase_dir/content-hex.raw"
}
plist_expect() {
    plist_file=$1 plist_key=$2 plist_expected=$3 plist_output=$4
    /usr/libexec/PlistBuddy -c "Print $plist_key" "$plist_file" >"$plist_output" 2>&1 || die "could not read pinned plist key $plist_key"
    [ "$(cat "$plist_output")" = "$plist_expected" ] || die "pinned plist value differs at $plist_key"
}
assert_installed_state() {
    installed_name=$1
    diskutil apfs list >"$phase_dir/$installed_name.apfs" 2>&1 || die "could not inspect installed APFS state"
    installed_volume_count=$(grep -Ec 'Name:[[:space:]]+Nix Store([[:space:]]|$)' "$phase_dir/$installed_name.apfs" || :)
    [ "$installed_volume_count" -eq 1 ] || die "installed state does not contain exactly one Nix Store APFS volume"
    diskutil info -plist 'Nix Store' >"$phase_dir/$installed_name.volume.plist" 2>&1 || die "could not inspect the Nix Store APFS volume"
    installed_volume_name=$(plutil -extract VolumeName raw -o - "$phase_dir/$installed_name.volume.plist") || die "could not read Nix Store volume name"
    installed_filesystem=$(plutil -extract FilesystemType raw -o - "$phase_dir/$installed_name.volume.plist") || die "could not read Nix Store filesystem type"
    installed_encryption=$(plutil -extract Encryption raw -o - "$phase_dir/$installed_name.volume.plist") || die "could not read Nix Store encryption state"
    installed_uuid=$(plutil -extract VolumeUUID raw -o - "$phase_dir/$installed_name.volume.plist") || die "could not read Nix Store volume UUID"
    [ "$installed_volume_name" = 'Nix Store' ] || die "Nix Store APFS volume name differs"
    [ "$installed_filesystem" = apfs ] || die "Nix Store filesystem is not APFS"
    [ "$installed_encryption" = true ] || die "Nix Store APFS volume is not encrypted"
    printf '%s\n' "$installed_uuid" >"$phase_dir/$installed_name.volume-uuid"
    printf '%s\n' "$installed_uuid" | grep -E '^[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}$' >/dev/null || die "Nix Store volume UUID is empty or invalid"
    [ -f /etc/fstab ] && [ ! -L /etc/fstab ] || die "installed fstab is missing or unsafe"
    installed_fstab="UUID=$(printf '%s\n' "$installed_uuid" | tr 'ABCDEF' 'abcdef') /nix apfs rw,noatime,noauto,nobrowse,nosuid,owners # Added by the Determinate Nix Installer"
    grep -Fxc "$installed_fstab" /etc/fstab >"$phase_dir/$installed_name.fstab-count" || die "exact Determinate fstab entry is absent"
    [ "$(cat "$phase_dir/$installed_name.fstab-count")" -eq 1 ] || die "exact Determinate fstab entry is not unique"
    awk '$2 == "/nix" {count++} END {print count + 0}' /etc/fstab >"$phase_dir/$installed_name.fstab-nix-count" || die "could not count fstab /nix entries"
    [ "$(cat "$phase_dir/$installed_name.fstab-nix-count")" -eq 1 ] || die "fstab contains an extra /nix entry"
    [ -f /etc/synthetic.conf ] && [ ! -L /etc/synthetic.conf ] || die "installed synthetic.conf is missing or unsafe"
    installed_synthetic_hex=$(content_hex /etc/synthetic.conf) || die "could not inspect installed synthetic.conf"
    [ "$installed_synthetic_hex" = 6e69780a ] || die "installed synthetic.conf is not exactly nix newline"
    : >"$phase_dir/$installed_name.launchd-found.raw"
    for installed_launch_dir in /Library/LaunchDaemons /Library/LaunchAgents; do
        [ -d "$installed_launch_dir" ] || continue
        find "$installed_launch_dir" -maxdepth 1 \( -type f -o -type l \) \( -iname '*nix*' -o -iname '*determinate*' \) -print >>"$phase_dir/$installed_name.launchd-found.raw" || die "could not inspect installed launchd files"
    done
    sort "$phase_dir/$installed_name.launchd-found.raw" >"$phase_dir/$installed_name.launchd-found"
    printf '%s\n' \
        /Library/LaunchDaemons/systems.determinate.nix-daemon.plist \
        /Library/LaunchDaemons/systems.determinate.nix-store.plist >"$phase_dir/$installed_name.launchd-expected"
    cmp -s "$phase_dir/$installed_name.launchd-expected" "$phase_dir/$installed_name.launchd-found" || die "installed state does not have exactly the two pinned launchd files"
    for installed_plist in \
        /Library/LaunchDaemons/systems.determinate.nix-store.plist \
        /Library/LaunchDaemons/systems.determinate.nix-daemon.plist; do
        [ -f "$installed_plist" ] && [ ! -L "$installed_plist" ] || die "required launchd file is missing or unsafe: $installed_plist"
        [ "$(stat -f '%Su:%Sg:%Lp' "$installed_plist")" = root:wheel:644 ] || die "required launchd file identity is unexpected: $installed_plist"
        stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' "$installed_plist" >>"$phase_dir/$installed_name.launchd-files"
    done
    store_plist=/Library/LaunchDaemons/systems.determinate.nix-store.plist
    daemon_plist=/Library/LaunchDaemons/systems.determinate.nix-daemon.plist
    plist_expect "$store_plist" :ProgramArguments:0 /usr/local/bin/determinate-nixd "$phase_dir/$installed_name.store-argv-0"
    plist_expect "$store_plist" :ProgramArguments:1 init "$phase_dir/$installed_name.store-argv-1"
    plist_expect "$daemon_plist" :ProgramArguments:0 /usr/local/bin/determinate-nixd "$phase_dir/$installed_name.daemon-argv-0"
    plist_expect "$daemon_plist" :ProgramArguments:1 daemon "$phase_dir/$installed_name.daemon-argv-1"
    plist_expect "$daemon_plist" ':Sockets:determinate-nixd.socket:SockPathName' /var/run/determinate-nixd.socket "$phase_dir/$installed_name.determinate-socket"
    plist_expect "$daemon_plist" ':Sockets:nix-daemon.socket:SockPathName' /var/run/nix-daemon.socket "$phase_dir/$installed_name.nix-socket"
    for pinned_plist in "$store_plist" "$daemon_plist"; do
        set +e
        /usr/libexec/PlistBuddy -c 'Print :ProgramArguments:2' "$pinned_plist" >/dev/null 2>&1
        extra_argument_status=$?
        set -e
        [ "$extra_argument_status" -ne 0 ] || die "pinned launchd program has an extra argument: $pinned_plist"
    done
    hook_plist=/Library/LaunchDaemons/systems.determinate.nix-installer.nix-hook.plist
    [ ! -e "$hook_plist" ] && [ ! -L "$hook_plist" ] || die "no-modify-profile install created the forbidden nix-hook file"
    for installed_job in systems.determinate.nix-store systems.determinate.nix-daemon; do
        launchctl print "system/$installed_job" >"$phase_dir/$installed_name.launchd-$installed_job" 2>&1 || die "required launchd job is absent: $installed_job"
    done
    set +e
    launchctl print system/systems.determinate.nix-installer.nix-hook >"$phase_dir/$installed_name.launchd-nix-hook-absent" 2>&1
    hook_job_status=$?
    set -e
    [ "$hook_job_status" -ne 0 ] || die "no-modify-profile install loaded the forbidden nix-hook job"
    security find-generic-password -a 'Nix Store' -s 'Nix Store' /Library/Keychains/System.keychain >"$phase_dir/$installed_name.keychain-metadata" 2>&1 || die "Nix Store System Keychain metadata item is absent"
    dscl . -list /Groups >"$phase_dir/$installed_name.groups.all" || die "could not inspect installed groups"
    grep -Fx nixbld "$phase_dir/$installed_name.groups.all" >"$phase_dir/$installed_name.group" || die "nixbld group is absent"
    dscl . -read /Groups/nixbld PrimaryGroupID GroupMembership >"$phase_dir/$installed_name.group-record" || die "could not inspect pinned nixbld group"
    grep -Fx 'PrimaryGroupID: 350' "$phase_dir/$installed_name.group-record" >/dev/null || die "nixbld group ID is not pinned GID 350"
    sed -n 's/^GroupMembership: //p' "$phase_dir/$installed_name.group-record" >"$phase_dir/$installed_name.membership-line" || die "could not read nixbld membership"
    tr ' ' '\n' <"$phase_dir/$installed_name.membership-line" >"$phase_dir/$installed_name.membership-lines" || die "could not split nixbld membership"
    sed '/^$/d' "$phase_dir/$installed_name.membership-lines" >"$phase_dir/$installed_name.membership-raw" || die "could not normalize nixbld membership"
    sort "$phase_dir/$installed_name.membership-raw" >"$phase_dir/$installed_name.membership" || die "could not sort nixbld membership"
    : >"$phase_dir/$installed_name.membership-expected.raw"
    installed_user_number=1
    while [ "$installed_user_number" -le 32 ]; do
        printf '_nixbld%s\n' "$installed_user_number" >>"$phase_dir/$installed_name.membership-expected.raw"
        installed_user_number=$((installed_user_number + 1))
    done
    sort "$phase_dir/$installed_name.membership-expected.raw" >"$phase_dir/$installed_name.membership-expected" || die "could not sort expected nixbld membership"
    cmp -s "$phase_dir/$installed_name.membership-expected" "$phase_dir/$installed_name.membership" || die "nixbld group membership differs from the pinned 32 users"
    dscl . -list /Users >"$phase_dir/$installed_name.users.all" || die "could not inspect installed users"
    grep -E '^_nixbld[0-9]+$' "$phase_dir/$installed_name.users.all" >"$phase_dir/$installed_name.build-users" || die "Nix build users are absent"
    installed_user_count=$(wc -l <"$phase_dir/$installed_name.build-users") || die "could not count Nix build users"
    [ "$installed_user_count" -eq 32 ] || die "installed state does not have exactly 32 Nix build users"
    installed_user_number=1
    while [ "$installed_user_number" -le 32 ]; do
        grep -Fx "_nixbld$installed_user_number" "$phase_dir/$installed_name.build-users" >/dev/null || die "required build user is absent: _nixbld$installed_user_number"
        dscl . -read "/Users/_nixbld$installed_user_number" UniqueID PrimaryGroupID >"$phase_dir/$installed_name.user-$installed_user_number" || die "could not inspect pinned build user _nixbld$installed_user_number"
        grep -Fx "UniqueID: $((350 + installed_user_number))" "$phase_dir/$installed_name.user-$installed_user_number" >/dev/null || die "pinned build user ID differs: _nixbld$installed_user_number"
        grep -Fx 'PrimaryGroupID: 350' "$phase_dir/$installed_name.user-$installed_user_number" >/dev/null || die "pinned build user group differs: _nixbld$installed_user_number"
        installed_user_number=$((installed_user_number + 1))
    done
    receipt_identity "$installed_name.receipt"
    [ -f /nix/nix-installer ] && [ ! -L /nix/nix-installer ] || die "installed installer helper is missing or unsafe"
    sha256 /nix/nix-installer >"$phase_dir/$installed_name.installer.sha256"
    [ "$(cat "$phase_dir/$installed_name.installer.sha256")" = "$expected_sha" ] || die "installed installer helper differs from the pin"
    [ -x /usr/local/bin/determinate-nixd ] && [ ! -L /usr/local/bin/determinate-nixd ] || die "determinate-nixd is missing or unsafe"
    [ "$(stat -f '%Su:%Sg:%Lp' /usr/local/bin/determinate-nixd)" = root:wheel:555 ] || die "determinate-nixd identity is unexpected"
    [ -d /nix/store ] && [ ! -L /nix/store ] || die "Nix store is missing or unsafe"
    mount >"$phase_dir/$installed_name.mounts" 2>&1 || die "could not inspect installed mount state"
    grep -E '[[:space:]]on[[:space:]]/nix[[:space:]].*\(apfs[,)]' "$phase_dir/$installed_name.mounts" >"$phase_dir/$installed_name.nix-mount" || die "Nix Store is not mounted at /nix as APFS"
    find /nix/store -mindepth 1 -maxdepth 1 -print -quit >"$phase_dir/$installed_name.store-first" 2>&1 || die "could not inspect Nix store"
    [ -s "$phase_dir/$installed_name.store-first" ] || die "Nix store is empty"
    require_functional_nix
}
record_foreign_state() {
    foreign_name=$1 sentinel=/nix/.pkg-s6-dn03c-foreign-$token
    mount >"$phase_dir/$foreign_name.mounts" 2>&1 || die "could not record foreign mount state"
    if grep -E '[[:space:]]on[[:space:]]/nix[[:space:]]' "$phase_dir/$foreign_name.mounts" >"$phase_dir/$foreign_name.nix-mount"; then
        printf '%s\n' mounted >"$phase_dir/$foreign_name.mount-state"
    else
        : >"$phase_dir/$foreign_name.nix-mount"
        printf '%s\n' unmounted >"$phase_dir/$foreign_name.mount-state"
    fi
    if [ -f "$sentinel" ] && [ ! -L "$sentinel" ]; then
        printf '%s\n' visible >"$phase_dir/$foreign_name.sentinel-visibility"
        sha256 "$sentinel" >"$phase_dir/$foreign_name.sentinel.sha256"
    elif [ "$(cat "$phase_dir/$foreign_name.mount-state")" = mounted ]; then
        printf '%s\n' 'hidden-by-mounted-filesystem; not proved deleted' >"$phase_dir/$foreign_name.sentinel-visibility"
    else
        die "foreign sentinel is absent while /nix is unmounted"
    fi
}

strict_residue() {
    residue_snapshot=$1
    residue_dirty=0
    : >"$phase_dir/vendor-residue"
    if [ "$phase" = lifecycle-residue ]; then
        residue_baseline=$evidence/baseline/after
        for residue_suffix in etc-nix.inventory fstab.identity determinate-nix-init-log.identity determinate-nix-daemon-log.identity; do
            if /usr/bin/cmp -s "$residue_baseline.$residue_suffix" "$residue_snapshot.$residue_suffix"; then
                :
            else
                residue_compare_status=$?
                [ "$residue_compare_status" -eq 1 ] || die "could not compare baseline residue identity: $residue_suffix"
                printf 'changed identity=%s\n' "$residue_suffix" >>"$phase_dir/vendor-residue"
                residue_dirty=1
            fi
        done
    else
        if ! identity_is_exact "$residue_snapshot.etc-nix.inventory" 'state=absent path_hex=2f6574632f6e6978'; then printf '%s\n' 'present identity=etc-nix.inventory' >>"$phase_dir/vendor-residue"; residue_dirty=1; fi
        if ! identity_is_exact "$residue_snapshot.fstab.identity" 'state=absent path_hex=2f6574632f6673746162 type=- mode=- uid=- gid=- size=- nlink=- sha256=-'; then printf '%s\n' 'present identity=fstab.identity' >>"$phase_dir/vendor-residue"; residue_dirty=1; fi
        if ! identity_is_exact "$residue_snapshot.determinate-nix-init-log.identity" 'state=absent path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d696e69742e6c6f67 type=- mode=- uid=- gid=- size=- nlink=- sha256=-'; then printf '%s\n' 'present identity=determinate-nix-init-log.identity' >>"$phase_dir/vendor-residue"; residue_dirty=1; fi
        if ! identity_is_exact "$residue_snapshot.determinate-nix-daemon-log.identity" 'state=absent path_hex=2f7661722f6c6f672f64657465726d696e6174652d6e69782d6461656d6f6e2e6c6f67 type=- mode=- uid=- gid=- size=- nlink=- sha256=-'; then printf '%s\n' 'present identity=determinate-nix-daemon-log.identity' >>"$phase_dir/vendor-residue"; residue_dirty=1; fi
    fi
    for residue_path in /nix /nix/receipt.json /nix/nix-installer /usr/local/bin/determinate-nixd; do
        if path_exists "$residue_path"; then printf 'present path=%s\n' "$residue_path" >>"$phase_dir/vendor-residue"; residue_dirty=1; fi
    done
    diskutil apfs list >"$phase_dir/residue.apfs" 2>&1 || die "could not inspect APFS residue"
    if grep -E 'Name:[[:space:]]+Nix Store([[:space:]]|$)' "$phase_dir/residue.apfs" >/dev/null; then
        printf '%s\n' 'present APFS=Nix Store' >>"$phase_dir/vendor-residue"
        residue_dirty=1
    fi
    for residue_file in /etc/synthetic.conf; do
        if [ -L "$residue_file" ]; then
            printf 'symlink file=%s\n' "$residue_file" >>"$phase_dir/vendor-residue"
            residue_dirty=1
        elif [ -f "$residue_file" ] && grep -Ei '(^|[[:space:]/])(nix|Nix Store)([[:space:]/]|$)' "$residue_file" >/dev/null; then
            printf 'entry file=%s\n' "$residue_file" >>"$phase_dir/vendor-residue"
            residue_dirty=1
        fi
    done
    for residue_dir in /Library/LaunchDaemons /Library/LaunchAgents; do
        [ -d "$residue_dir" ] || continue
        find "$residue_dir" -maxdepth 1 \( -type f -o -type l \) \( -iname '*nix*' -o -iname '*determinate*' \) -print >>"$phase_dir/vendor-residue" || die "could not inspect launchd residue"
    done
    [ ! -s "$phase_dir/vendor-residue" ] || residue_dirty=1
    launchctl print system >"$phase_dir/residue.launchd-system" 2>&1 || die "could not inspect launchd residue"
    grep -Ei '(^|[^[:alnum:]_])(nix|determinate)([^[:alnum:]_]|$)' "$phase_dir/residue.launchd-system" >"$phase_dir/vendor-launchd-residue" || :
    [ ! -s "$phase_dir/vendor-launchd-residue" ] || residue_dirty=1
    set +e
    security find-generic-password -a 'Nix Store' -s 'Nix Store' /Library/Keychains/System.keychain >/dev/null 2>&1
    residue_keychain_status=$?
    set -e
    case $residue_keychain_status in
        0) printf '%s\n' present >"$phase_dir/vendor-keychain-residue"; residue_dirty=1 ;;
        44) printf '%s\n' absent >"$phase_dir/vendor-keychain-residue" ;;
        *) die "System Keychain residue probe failed: $residue_keychain_status" ;;
    esac
    dscl . -list /Groups >"$phase_dir/residue.groups.all" || die "could not inspect group residue"
    dscl . -list /Users >"$phase_dir/residue.users.all" || die "could not inspect user residue"
    { grep -E '^(nixbld|_nixbld)$' "$phase_dir/residue.groups.all" || :; grep -E '^_?nixbld[0-9]+$' "$phase_dir/residue.users.all" || :; } >"$phase_dir/vendor-account-residue"
    [ ! -s "$phase_dir/vendor-account-residue" ] || residue_dirty=1
    find /var/run /private/var/run -xdev -type s \( -iname '*nix*' -o -iname '*determinate*' \) -print 2>/dev/null >"$phase_dir/vendor-socket-residue" || die "could not inspect socket residue"
    [ ! -s "$phase_dir/vendor-socket-residue" ] || residue_dirty=1
    if [ "$residue_dirty" -eq 0 ]; then printf '%s\n' PASS >"$phase_dir/vendor-outcome"; else printf '%s\n' FAIL >"$phase_dir/vendor-outcome"; fi

    product_dirty=0
    : >"$phase_dir/product-residue"
    for product_path in /opt/pkg '/Library/Application Support/pkg'; do
        if path_exists "$product_path"; then printf 'present path=%s\n' "$product_path" >>"$phase_dir/product-residue"; product_dirty=1; fi
    done
    for product_dir in /Library/LaunchDaemons /Library/LaunchAgents; do
        [ -d "$product_dir" ] || continue
        find "$product_dir" -maxdepth 1 \( -type f -o -type l \) -iname '*pkg*' -print >>"$phase_dir/product-residue" || die "could not inspect product launchd residue"
    done
    { grep -E '^_?pkg$' "$phase_dir/residue.groups.all" || :; grep -E '^_?pkg$' "$phase_dir/residue.users.all" || :; } >"$phase_dir/product-account-residue"
    [ ! -s "$phase_dir/product-residue" ] || product_dirty=1
    [ ! -s "$phase_dir/product-account-residue" ] || product_dirty=1
    if [ "$product_dirty" -eq 0 ]; then printf '%s\n' PASS >"$phase_dir/product-residue-outcome"; else printf '%s\n' FAIL >"$phase_dir/product-residue-outcome"; fi
    strict_vendor_failed=$residue_dirty
    strict_product_failed=$product_dirty
}

case $# in 6|7) ;; *) die "usage: inside.sh PHASE TOKEN MARKER STAGED_INSTALLER INSTALLER_SHA256 GUEST_EVIDENCE_DIR [APPROVAL]" ;; esac
phase=$1 token=$2 marker=$3 staged=$4 expected_sha=$5 evidence=$6 approval=${7-}
[ "$(id -u)" -eq 0 ] || die "guest lane requires root"
[ "$(uname -s)" = Darwin ] || die "guest must be Darwin"
[ "$(uname -m)" = arm64 ] || die "guest must be arm64"
[ "$(sysctl -n kern.hv_vmm_present)" = 1 ] || die "guest virtualization marker is absent"
case $(sysctl -n hw.model) in VirtualMac*) ;; *) die "guest model is not VirtualMac" ;; esac
case $token in ''|*[!0-9a-f]*) die "token must be lowercase hexadecimal" ;; esac
[ "${#token}" -eq 32 ] || die "token must have 32 hexadecimal characters"
case $expected_sha in *[!0-9a-f]*|'') die "installer digest must be lowercase hexadecimal" ;; esac
[ "${#expected_sha}" -eq 64 ] || die "installer digest must have 64 hexadecimal characters"
for canonical_path in "$marker" "$staged" "$evidence"; do case $canonical_path in /private/var/tmp/*) ;; *) die "guest paths must be under /private/var/tmp" ;; esac; done
[ -f "$marker" ] && [ ! -L "$marker" ] || die "guest ownership marker is missing"
[ "$(stat -f '%Su:%Sg:%Lp' "$marker")" = root:wheel:600 ] || die "guest ownership marker is not private"
[ "$(cat "$marker")" = "$token" ] || die "guest ownership marker does not match"
[ -f "$staged" ] && [ ! -L "$staged" ] || die "staged installer is not a regular non-symlink file"
[ "$(stat -f '%Su:%Sg:%Lp' "$staged")" = root:wheel:700 ] || die "staged installer must be root:wheel mode 0700"
staged_parent=$(CDPATH= cd -P "$(dirname "$staged")" && pwd) || die "staged installer parent is invalid"
[ "$staged" = "${staged_parent%/}/$(basename "$staged")" ] || die "staged installer path is not canonical"
[ "$(stat -f '%Su:%Sg:%Lp' "$staged_parent")" = root:wheel:700 ] || die "guest staging directory is not private"
[ "$(sha256 "$staged")" = "$expected_sha" ] || die "staged installer digest mismatch"
marker_parent=$(CDPATH= cd -P "$(dirname "$marker")" && pwd) || die "marker parent is invalid"
[ "$marker" = "${marker_parent%/}/$(basename "$marker")" ] || die "marker path is not canonical"
[ "$(stat -f '%Su:%Sg:%Lp' "$marker_parent")" = root:wheel:700 ] || die "marker parent is not private"

console_user=$(stat -f %Su /dev/console) || die "could not record graphical console owner"

ledger=$evidence/phase-ledger
if [ "$phase" = baseline ]; then
    [ "$#" -eq 6 ] || die "baseline does not accept approval"
    [ ! -e "$evidence" ] && [ ! -L "$evidence" ] || die "baseline evidence path must be absent"
    evidence_parent=$(CDPATH= cd -P "$(dirname "$evidence")" && pwd) || die "evidence parent is invalid"
    [ "$evidence" = "${evidence_parent%/}/$(basename "$evidence")" ] || die "evidence path is not canonical"
    mkdir -m 0700 "$evidence"
    chown root:wheel "$evidence"
    : >"$ledger"
else
    [ -d "$evidence" ] && [ ! -L "$evidence" ] || die "lane evidence directory is missing or unsafe"
    [ "$(stat -f '%Su:%Sg:%Lp' "$evidence")" = root:wheel:700 ] || die "lane evidence directory is not private"
    [ -f "$ledger" ] && [ ! -L "$ledger" ] || die "phase ledger is missing or unsafe"
    evidence_parent=$(CDPATH= cd -P "$(dirname "$evidence")" && pwd) || die "evidence parent is invalid"
    [ "$evidence" = "${evidence_parent%/}/$(basename "$evidence")" ] || die "evidence path is not canonical"
fi
case $phase in
    baseline) expected_ledger='' ;;
    lifecycle-install) expected_ledger='baseline' ;;
    lifecycle-post-reboot) expected_ledger='baseline
lifecycle-install' ;;
    lifecycle-repeat-install) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot' ;;
    lifecycle-repair) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot
lifecycle-repeat-install' ;;
    lifecycle-daemon) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot
lifecycle-repeat-install
lifecycle-repair' ;;
    lifecycle-uninstall) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot
lifecycle-repeat-install
lifecycle-repair
lifecycle-daemon' ;;
    lifecycle-repeat-uninstall) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot
lifecycle-repeat-install
lifecycle-repair
lifecycle-daemon
lifecycle-uninstall' ;;
    lifecycle-residue) expected_ledger='baseline
lifecycle-install
lifecycle-post-reboot
lifecycle-repeat-install
lifecycle-repair
lifecycle-daemon
lifecycle-uninstall
lifecycle-repeat-uninstall' ;;
    crash-kill) expected_ledger='baseline' ;;
    crash-recover) expected_ledger='baseline
crash-kill' ;;
    foreign-synthetic-prepare) expected_ledger='baseline' ;;
    foreign-post-reboot) expected_ledger='baseline
foreign-synthetic-prepare' ;;
    foreign-refuse) expected_ledger='baseline
foreign-synthetic-prepare
foreign-post-reboot' ;;
    foreign-observe) expected_ledger='baseline
foreign-synthetic-prepare
foreign-post-reboot
foreign-refuse' ;;
    upstream-install) expected_ledger='baseline' ;;
    upstream-determinate-attempt) expected_ledger='baseline
upstream-install' ;;
    *) die "unsupported phase: $phase" ;;
esac
[ "$(cat "$ledger")" = "$expected_ledger" ] || die "phase order is invalid for $phase"
if [ "$phase" = foreign-observe ]; then
    [ "$#" -eq 7 ] && [ "$approval" = approve-observe-vendor-foreign-state ] || die "foreign observation requires exact second approval"
else
    [ "$#" -eq 6 ] || die "$phase does not accept approval"
fi
phase_dir=$evidence/$phase
[ ! -e "$phase_dir" ] && [ ! -L "$phase_dir" ] || die "phase evidence already exists"
mkdir -m 0700 "$phase_dir"
chown root:wheel "$phase_dir"
printf '%s\n' "$phase" >>"$ledger"
printf '%s\n' "$expected_sha" >"$phase_dir/installer.expected.sha256"
sha256 "$staged" >"$phase_dir/installer.actual.sha256"
printf '%s\n' "$console_user" >"$phase_dir/console-user"
current_boot=$(sysctl -n kern.boottime) || die "could not record raw kernel boot time"
printf '%s\n' "$current_boot" >"$phase_dir/boot-session"
record_free_disk
snapshot before
case $phase in
    lifecycle-install) compare_residue_contract "$evidence/baseline/after" "$phase_dir/before" "install pre-state differs from clean baseline" ;;
    lifecycle-uninstall) compare_active_residue_contract "$evidence/lifecycle-daemon/after" "$phase_dir/before" "uninstall pre-state differs from daemon post-state" ;;
    lifecycle-repeat-uninstall) compare_residue_contract "$evidence/lifecycle-uninstall/after" "$phase_dir/before" "repeat-uninstall pre-state differs from uninstall post-state" ;;
esac

phase_exit=0
case $phase in
    baseline)
        strict_residue "$phase_dir/before"
        ;;
    lifecycle-install)
        require_first_vendor_gates
        require_installer_version
        run_recorded install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile
        initial_install_status=$last_status
        snapshot install-preassert
        run_recorded install-preassert-determinate-nixd-status 60 /usr/local/bin/determinate-nixd status
        run_recorded install-preassert-nix-store-ping 120 /nix/var/nix/profiles/default/bin/nix store ping --store daemon
        [ "$initial_install_status" -eq 0 ] || die "initial Determinate install failed"
        assert_installed_state after-install
        sha256 /nix/nix-installer >"$phase_dir/installed-installer.sha256"
        [ "$(cat "$phase_dir/installed-installer.sha256")" = "$expected_sha" ] || die "installed installer digest differs from the pin"
        ;;
    lifecycle-post-reboot)
        require_reboot_since lifecycle-install
        assert_installed_state after-reboot
        ;;
    lifecycle-repeat-install)
        run_recorded repeat-install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile
        [ "$last_status" -eq 0 ] || die "repeat Determinate install failed"
        assert_installed_state after-repeat-install
        ;;
    lifecycle-repair)
        run_recorded repair 7200 /nix/nix-installer --diagnostic-endpoint "$diagnostic_endpoint" repair --no-confirm
        [ "$last_status" -eq 0 ] || die "default repair failed"
        run_recorded repair-sequoia 7200 /nix/nix-installer --diagnostic-endpoint "$diagnostic_endpoint" repair sequoia --no-confirm
        [ "$last_status" -eq 0 ] || die "Sequoia repair failed"
        assert_installed_state after-repair
        ;;
    lifecycle-daemon)
        daemon=/usr/local/bin/determinate-nixd
        [ -x "$daemon" ] && [ ! -L "$daemon" ] || die "determinate-nixd is unsafe or absent"
        stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' "$daemon" >"$phase_dir/determinate-nixd.stat"
        [ "$(stat -f '%Lp:%Su:%Sg' "$daemon")" = 555:root:wheel ] || die "determinate-nixd mode or ownership is unexpected"
        run_recorded daemon-version 60 "$daemon" version
        [ "$last_status" -eq 0 ] || die "determinate-nixd version failed"
        run_recorded daemon-status 60 "$daemon" status
        [ "$last_status" -eq 0 ] || die "determinate-nixd status failed"
        run_recorded daemon-upgrade-help 60 "$daemon" upgrade --help
        [ "$last_status" -eq 0 ] || die "determinate-nixd upgrade help failed"
        run_recorded daemon-upgrade 7200 "$daemon" upgrade --version v3.22.1
        [ "$last_status" -eq 0 ] || die "pinned determinate-nixd upgrade failed"
        for absent_command in update upgrade self-update; do
            run_recorded "installer-$absent_command" 60 "$staged" "$absent_command" --help
            [ "$last_status" -ne 0 ] || die "installer unexpectedly accepts $absent_command"
            grep -Ei '(unrecognized|unknown|invalid).*(subcommand|command)|unexpected argument' "$phase_dir/installer-$absent_command.output" >/dev/null || die "installer $absent_command rejection was not identified as an unknown subcommand"
        done
        assert_installed_state after-daemon
        ;;
    lifecycle-uninstall)
        receipt_identity receipt-before-uninstall
        run_recorded uninstall 7200 /nix/nix-installer --diagnostic-endpoint "$diagnostic_endpoint" uninstall --no-confirm /nix/receipt.json
        [ "$last_status" -eq 0 ] || { printf '%s\n' FAIL >"$phase_dir/vendor-outcome"; die "uninstall failed"; }
        printf '%s\n' PASS >"$phase_dir/vendor-outcome"
        ;;
    lifecycle-repeat-uninstall)
        run_recorded repeat-uninstall 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" uninstall --no-confirm /nix/receipt.json
        [ "$last_status" -eq 1 ] || die "repeat uninstall did not return the pinned observed status 1"
        grep -F 'Reading receipt' "$phase_dir/repeat-uninstall.output" >/dev/null || die "repeat uninstall did not identify receipt reading"
        grep -F 'No such file or directory' "$phase_dir/repeat-uninstall.output" >/dev/null || die "repeat uninstall did not identify the absent receipt"
        printf '%s\n' PASS >"$phase_dir/vendor-outcome"
        ;;
    lifecycle-residue)
        require_reboot_since lifecycle-repeat-uninstall
        compare_residue_contract "$evidence/lifecycle-repeat-uninstall/after" "$phase_dir/before" "post-reboot residue pre-state differs from repeat-uninstall post-state"
        strict_residue "$phase_dir/before"
        ;;
    crash-kill)
        require_first_vendor_gates
        require_installer_version
        write_argv "$phase_dir/install.argv" "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile
        signals_hold
        (umask 022; exec "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile) </dev/null >"$phase_dir/install.output" 2>&1 &
        crash_pid=$!
        active_vendor_pid=$crash_pid
        signals_restore
        case $crash_pid in ''|*[!0-9]*) die "installer PID is invalid" ;; esac
        [ "$crash_pid" -gt 1 ] && [ "$crash_pid" -ne "$$" ] || die "installer PID is unsafe"
        crash_command=$(ps -p "$crash_pid" -o command=) || die "installer process exited before PID validation"
        case $crash_command in "$staged"|"$staged "*) ;; *) die "PID command does not start with the exact staged installer path" ;; esac
        printf '%s\n' "$crash_pid" >"$phase_dir/installer.pid"
        crash_elapsed=0 crash_ready=0
        while kill -0 "$crash_pid" 2>/dev/null && [ "$crash_elapsed" -lt 1800 ]; do
            : >"$phase_dir/crash-store-first"
            if [ -d /nix/store ]; then find /nix/store -mindepth 1 -maxdepth 1 -print -quit >"$phase_dir/crash-store-first" 2>&1 || die "could not inspect crash progress"; fi
            if [ -x /usr/local/bin/determinate-nixd ] && [ -s "$phase_dir/crash-store-first" ]; then crash_ready=1; break; fi
            sleep 1
            crash_elapsed=$((crash_elapsed + 1))
        done
        if [ "$crash_ready" -ne 1 ]; then
            kill -TERM "$crash_pid" 2>/dev/null || :
            set +e; wait_bounded 5 "$crash_pid"; set -e
            die "late crash marker was not reached while the installer remained alive"
        fi
        printf '%s\n' 'determinate-nixd executable and non-empty Nix store' >"$phase_dir/crash-marker"
        signals_hold
        crash_command_before_kill=$(ps -p "$crash_pid" -o command=) || { signals_restore; die "installer process exited before final PID validation"; }
        case $crash_command_before_kill in "$staged"|"$staged "*) ;; *) signals_restore; die "final PID command does not start with the exact staged installer path" ;; esac
        printf '%s\n' "$crash_command_before_kill" >"$phase_dir/installer-command-before-sigkill"
        kill -KILL "$crash_pid" || { signals_restore; die "could not SIGKILL the validated installer PID"; }
        set +e
        wait "$crash_pid"
        crash_status=$?
        active_vendor_pid=
        set -e
        signals_restore
        printf '%s\n' "$crash_status" >"$phase_dir/install.status"
        [ "$crash_status" -eq 137 ] || die "SIGKILL did not produce status 137"
        snapshot immediate-after-sigkill
        ;;
    crash-recover)
        require_reboot_since crash-kill
        run_recorded recover-install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile
        [ "$last_status" -eq 0 ] || die "install did not recover after the forced crash"
        [ "$(sha256 /nix/nix-installer)" = "$expected_sha" ] || die "recovered installed copy digest differs from the pin"
        assert_installed_state after-recovery
        ;;
    foreign-synthetic-prepare)
        path_exists /nix && die "foreign lane requires /nix to be absent before synthetic preparation"
        [ ! -e /etc/synthetic.conf ] && [ ! -L /etc/synthetic.conf ] || die "foreign fixture requires absent /etc/synthetic.conf"
        synthetic_temp=/etc/.pkg-s6-synthetic-$token
        [ ! -e "$synthetic_temp" ] && [ ! -L "$synthetic_temp" ] || die "synthetic fixture temporary path already exists"
        printf 'nix\n' >"$synthetic_temp"
        chown root:wheel "$synthetic_temp"
        chmod 0644 "$synthetic_temp"
        ln "$synthetic_temp" /etc/synthetic.conf || die "could not atomically create absent synthetic fixture"
        rm -f "$synthetic_temp"
        [ "$(stat -f '%Su:%Sg:%Lp' /etc/synthetic.conf)" = root:wheel:644 ] || die "synthetic fixture identity is invalid"
        synthetic_hex=$(content_hex /etc/synthetic.conf) || die "could not hash synthetic fixture content"
        synthetic_hash=$(sha256 /etc/synthetic.conf) || die "could not hash synthetic fixture"
        [ "$synthetic_hex" = 6e69780a ] || die "synthetic fixture content is not exact"
        printf 'token=%s\ncontent-hex=%s\nsha256=%s\n' "$token" "$synthetic_hex" "$synthetic_hash" >"$phase_dir/synthetic-ownership"
        stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' /etc/synthetic.conf >"$phase_dir/synthetic.stat"
        sync
        ;;
    foreign-post-reboot)
        require_reboot_since foreign-synthetic-prepare
        [ -f /etc/synthetic.conf ] && [ ! -L /etc/synthetic.conf ] || die "owned synthetic fixture is absent or unsafe"
        [ "$(stat -f '%Su:%Sg:%Lp' /etc/synthetic.conf)" = root:wheel:644 ] || die "owned synthetic fixture identity changed"
        [ "$(content_hex /etc/synthetic.conf)" = 6e69780a ] || die "owned synthetic fixture content changed"
        grep -Fx "token=$token" "$evidence/foreign-synthetic-prepare/synthetic-ownership" >/dev/null || die "synthetic fixture ownership token does not match"
        grep -Fx "sha256=$(sha256 /etc/synthetic.conf)" "$evidence/foreign-synthetic-prepare/synthetic-ownership" >/dev/null || die "synthetic fixture digest changed"
        [ -d /nix ] && [ ! -L /nix ] || die "synthetic /nix did not appear after reboot"
        mount >"$phase_dir/pre-sentinel.mounts" 2>&1 || die "could not inspect synthetic /nix mount state"
        grep -E '[[:space:]]on[[:space:]]/nix[[:space:]]' "$phase_dir/pre-sentinel.mounts" >/dev/null && die "synthetic /nix must be unmounted before the foreign fixture"
        find /nix -mindepth 1 -maxdepth 1 -print -quit >"$phase_dir/pre-sentinel.first-entry" 2>&1 || die "could not inspect synthetic /nix"
        [ ! -s "$phase_dir/pre-sentinel.first-entry" ] || die "synthetic /nix must be empty before the foreign fixture"
        sentinel=/nix/.pkg-s6-dn03c-foreign-$token
        [ ! -e "$sentinel" ] && [ ! -L "$sentinel" ] || die "foreign sentinel already exists"
        printf 'pkg-s6-foreign:%s\n' "$token" >"$sentinel"
        chown root:wheel "$sentinel"
        chmod 0600 "$sentinel"
        [ "$(stat -f '%Su:%Sg:%Lp' "$sentinel")" = root:wheel:600 ] || die "foreign sentinel identity is invalid"
        sentinel_hex=$(content_hex "$sentinel") || die "could not hash foreign sentinel content"
        sentinel_hash=$(sha256 "$sentinel") || die "could not hash foreign sentinel"
        printf 'token=%s\ncontent-hex=%s\nsha256=%s\n' "$token" "$sentinel_hex" "$sentinel_hash" >"$phase_dir/sentinel-ownership"
        stat -f 'type=%HT uid=%u gid=%g owner=%Su:%Sg mode=%Lp size=%z path=%N' "$sentinel" >"$phase_dir/sentinel.stat"
        sha256 "$sentinel" >"$phase_dir/sentinel.sha256"
        ;;
    foreign-refuse)
        sentinel=/nix/.pkg-s6-dn03c-foreign-$token
        [ -f "$sentinel" ] && [ ! -L "$sentinel" ] || die "foreign sentinel is absent or unsafe"
        [ "$(stat -f '%Su:%Sg:%Lp' "$sentinel")" = root:wheel:600 ] || die "foreign sentinel identity changed before refusal"
        sha256 "$sentinel" >"$phase_dir/sentinel.sha256"
        [ "$(cat "$phase_dir/sentinel.sha256")" = "$(cat "$evidence/foreign-post-reboot/sentinel.sha256")" ] || die "foreign sentinel changed before refusal"
        printf '%s\n' 'SECOND_APPROVAL_REQUIRED' >"$phase_dir/vendor-outcome"
        phase_exit=20
        ;;
    foreign-observe)
        require_first_vendor_gates
        sentinel=/nix/.pkg-s6-dn03c-foreign-$token
        [ -f "$sentinel" ] && [ ! -L "$sentinel" ] || die "foreign sentinel is absent or unsafe"
        sentinel_before=$(sha256 "$sentinel")
        [ "$sentinel_before" = "$(cat "$evidence/foreign-post-reboot/sentinel.sha256")" ] || die "foreign sentinel changed before observation"
        record_foreign_state before-observation
        require_installer_version
        run_recorded foreign-install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile
        printf 'status=%s\n' "$last_status" >"$phase_dir/vendor-outcome"
        record_foreign_state after-observation
        if [ "$(cat "$phase_dir/after-observation.sentinel-visibility")" = visible ]; then
            [ "$(cat "$phase_dir/after-observation.sentinel.sha256")" = "$sentinel_before" ] || die "vendor changed the visible foreign sentinel"
        fi
        printf '%s\n' 'No uninstall or cleanup was run after the foreign observation.' >"$phase_dir/cleanup-scope"
        ;;
    upstream-install)
        require_first_vendor_gates
        require_installer_version
        run_recorded upstream-install 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --prefer-upstream-nix --no-confirm --no-modify-profile
        [ "$last_status" -eq 0 ] || die "upstream Nix install failed"
        receipt_identity receipt
        upstream_nix=/nix/var/nix/profiles/default/bin/nix
        [ -x "$upstream_nix" ] || die "upstream Nix executable is missing"
        run_recorded upstream-version 60 "$upstream_nix" --version
        [ "$last_status" -eq 0 ] || die "upstream Nix version failed"
        [ "$(sed -n '1p' "$phase_dir/upstream-version.output")" = 'nix (Nix) 2.35.2' ] || die "upstream Nix is not exactly 2.35.2"
        ;;
    upstream-determinate-attempt)
        receipt_identity receipt-before
        run_recorded determinate-attempt 7200 "$staged" --diagnostic-endpoint "$diagnostic_endpoint" install --determinate --no-confirm --no-modify-profile
        [ "$last_status" -eq 1 ] || die "Determinate-on-upstream attempt did not return the pinned status 1"
        grep -F 'used different planner settings' "$phase_dir/determinate-attempt.output" >/dev/null || die "planner mismatch refusal was not observed"
        receipt_identity receipt-after
        [ "$(cat "$phase_dir/receipt-before.sha256")" = "$(cat "$phase_dir/receipt-after.sha256")" ] || die "refused attempt changed the opaque receipt"
        upstream_nix=/nix/var/nix/profiles/default/bin/nix
        run_recorded upstream-version-after 60 "$upstream_nix" --version
        [ "$last_status" -eq 0 ] || die "upstream Nix stopped working after refusal"
        [ "$(sed -n '1p' "$phase_dir/upstream-version-after.output")" = 'nix (Nix) 2.35.2' ] || die "refused attempt changed upstream Nix 2.35.2"
        ;;
esac

snapshot after
case $phase in
    baseline)
        compare_residue_contract "$phase_dir/before" "$phase_dir/after" "baseline changed during observation"
        require_clean_residue_contract "$phase_dir/after"
        ;;
    lifecycle-install) require_installed_residue_contract "$phase_dir/after" ;;
    lifecycle-repeat-uninstall) compare_residue_contract "$phase_dir/before" "$phase_dir/after" "repeat uninstall changed residue identity" ;;
    lifecycle-residue) compare_residue_contract "$phase_dir/before" "$phase_dir/after" "final post-reboot residue identity changed during observation" ;;
esac
[ "$strict_vendor_failed" -eq 0 ] || die "vendor residue remains"
[ "$strict_product_failed" -eq 0 ] || die "product residue remains"
cp "$ledger" "$phase_dir/phase-ledger"
printf '%s\n' PASS >"$phase_dir/phase-status"
record PASS "$phase completed with expected observations"
find "$evidence" -type d -exec chmod 0700 {} \;
find "$evidence" -type f -exec chmod 0600 {} \;
trap - EXIT HUP INT TERM
exit "$phase_exit"
