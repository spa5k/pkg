#!/bin/sh
set -eu

die() { printf '%s\n' "$*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
private_tree() {
    find "$1" -type d -exec chmod 0700 {} \;
    find "$1" -type f -exec chmod 0600 {} \;
}

[ "$#" -eq 5 ] || die "usage: $0 --approve-destructive-vm BASE INSTALLER LANE /absolute/new/output"
[ "$1" = "--approve-destructive-vm" ] || die "explicit destructive VM approval is required"
base=$2
installer=$3
lane=$4
out=$5
case $lane in
    lifecycle|diagnostics-disabled|crash-recovery|foreign-nix|upstream-input) ;;
    *) die "unsupported lane: $lane" ;;
esac
for input in "$base" "$installer"; do
    case $input in /*) ;; *) die "input paths must be absolute" ;; esac
    [ -f "$input" ] || die "input must be a regular file: $input"
    [ ! -L "$input" ] || die "input must not be a symlink: $input"
done
case $out in /*) ;; *) die "output path must be absolute" ;; esac
[ ! -L "$out" ] || die "output path must not be a symlink"
[ ! -e "$out" ] || die "output path already exists: $out"
[ ! -w "$base" ] || die "base image must be read-only (chmod 0444)"

script_dir=$(CDPATH= cd -P "$(dirname "$0")" && pwd)
pins=$script_dir/../assets.sha256
ubuntu_sha=6e40c07ae715f744f84af0bec76415cc1987dd115b4b8de437818561f01a3733
installer_sha=$(awk '$2 == "x86_64-linux" {print $1}' "$pins")
[ "$(sha256 "$base")" = "$ubuntu_sha" ] || die "Ubuntu base digest mismatch"
[ "$(sha256 "$installer")" = "$installer_sha" ] || die "installer digest mismatch"

out_parent=$(dirname "$out")
[ -d "$out_parent" ] || die "output parent does not exist"
available_kb=$(df -Pk "$out_parent" | awk 'NR == 2 {print $4}')
[ "$available_kb" -ge 16777216 ] || die "at least 16 GiB of free disk is required"
for tool in qemu-system-x86_64 qemu-img hdiutil ssh scp ssh-keygen nc tar git; do
    command -v "$tool" >/dev/null 2>&1 || die "required host tool is missing: $tool"
done
repo_root=$(git -C "$script_dir/../../.." rev-parse --show-toplevel) || die "runner is not in a Git worktree"
[ -z "$(git -C "$repo_root" status --porcelain=v1 --untracked-files=all)" ] || die "runner worktree must be clean"
product_revision=$(git -C "$repo_root" rev-parse HEAD)
vendor_revision=4132ad07a15ee7d88c096ac7172b7afb2672866b
port=22222
nc -z 127.0.0.1 "$port" >/dev/null 2>&1 && die "localhost SSH port $port is occupied"

umask 077
mkdir -m 0700 "$out"
printf '%s\n' "$ubuntu_sha" >"$out/base-image.sha256"
printf '%s\n' "$installer_sha" >"$out/installer.sha256"
printf '%s\n' "$lane" >"$out/lane"
printf '%s\n' "$product_revision" >"$out/product-git-revision"
printf '%s\n' "$vendor_revision" >"$out/vendor-full-revision"
seed=$out/seed
mkdir -m 0700 "$seed"
overlay=$out/guest.qcow2
cidata=$out/cidata.iso
serial=$out/qemu-serial.log
pidfile=$out/qemu.pid
key=$out/id_ed25519
guest_installer=/var/tmp/pkg-s6/nix-installer
guest_inside=/var/tmp/pkg-s6/inside.sh
guest_pins=/var/tmp/pkg-s6/assets.sha256
token=$(LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | dd bs=1 count=48 2>/dev/null)
[ "${#token}" -eq 48 ] || die "could not generate run token"
qemu_pid=
cleanup() {
    if [ -n "$qemu_pid" ] && kill -0 "$qemu_pid" 2>/dev/null; then
        kill "$qemu_pid" 2>/dev/null || :
        wait "$qemu_pid" 2>/dev/null || :
    fi
    rm -f "$overlay" "$cidata" "$key" "$key.pub"
    rm -rf "$seed"
    private_tree "$out"
}
trap cleanup EXIT HUP INT TERM

ssh-keygen -q -t ed25519 -N '' -f "$key"
pubkey=$(cat "$key.pub")
cat >"$seed/meta-data" <<EOF
instance-id: pkg-s6-$token
local-hostname: pkg-s6
EOF
cat >"$seed/user-data" <<EOF
#cloud-config
users:
  - name: pkgproof
    lock_passwd: true
    shell: /bin/sh
    sudo: ALL=(ALL) NOPASSWD:ALL
    ssh_authorized_keys:
      - $pubkey
ssh_pwauth: false
disable_root: true
EOF
hdiutil makehybrid -quiet -iso -joliet -default-volume-name cidata -o "$cidata" "$seed"
qemu-img create -q -f qcow2 -F qcow2 -b "$base" "$overlay" 30G

qemu-system-x86_64 \
    -machine q35,accel=tcg -cpu max -m 4096 -smp 2 \
    -drive "if=virtio,format=qcow2,file=$overlay" \
    -drive "if=virtio,format=raw,readonly=on,file=$cidata" \
    -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:$port-:22" \
    -device virtio-net-pci,netdev=net0 \
    -nographic -serial "file:$serial" -monitor none -pidfile "$pidfile" &
qemu_pid=$!

guest_ssh() {
    ssh -i "$key" -p "$port" -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR \
        pkgproof@127.0.0.1 "$@"
}
guest_scp() {
    source=$1
    destination=$2
    scp -i "$key" -P "$port" -o IdentitiesOnly=yes -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR \
        "$source" "pkgproof@127.0.0.1:$destination"
}
ready=0
i=0
while [ "$i" -lt 180 ]; do
    if guest_ssh true >/dev/null 2>&1; then ready=1; break; fi
    kill -0 "$qemu_pid" 2>/dev/null || die "QEMU exited before SSH became ready"
    i=$((i + 1))
    sleep 2
done
[ "$ready" -eq 1 ] || die "SSH did not become ready"
guest_ssh 'timeout 300 cloud-init status --wait' >"$out/cloud-init.log" 2>&1 || die "cloud-init failed or timed out"
guest_ssh 'mkdir -m 0700 /var/tmp/pkg-s6'
guest_scp "$script_dir/inside.sh" "$guest_inside" >/dev/null
guest_scp "$pins" "$guest_pins" >/dev/null
guest_scp "$installer" "$guest_installer" >/dev/null
guest_ssh "chmod 0700 '$guest_inside' '$guest_installer'; chmod 0600 '$guest_pins'; printf '%s\\n' '$token' | sudo tee /etc/pkg-s6-disposable-vm >/dev/null; sudo chmod 0600 /etc/pkg-s6-disposable-vm"

run_guest() {
    phase=$1
    guest_ssh "sudo timeout --kill-after=60 7200 env S6_PHASE='$phase' '$guest_inside' '$lane' '$token' '$guest_installer' '$guest_pins'"
}

set +e
run_guest initial >"$out/guest-run.log" 2>&1
guest_status=$?
set -e
if [ "$guest_status" -eq 194 ]; then
    guest_ssh 'sudo systemctl reboot' >/dev/null 2>&1 || :
    down=0
    i=0
    while [ "$i" -lt 60 ]; do
        if ! guest_ssh true >/dev/null 2>&1; then down=1; break; fi
        i=$((i + 1)); sleep 1
    done
    [ "$down" -eq 1 ] || die "guest did not start reboot"
    ready=0
    i=0
    while [ "$i" -lt 180 ]; do
        if guest_ssh true >/dev/null 2>&1; then ready=1; break; fi
        kill -0 "$qemu_pid" 2>/dev/null || die "QEMU exited during reboot"
        i=$((i + 1)); sleep 2
    done
    [ "$ready" -eq 1 ] || die "guest did not return after reboot"
    set +e
    run_guest resume >>"$out/guest-run.log" 2>&1
    guest_status=$?
    set -e
fi

mkdir -m 0700 "$out/guest-evidence"
archive=$out/guest-evidence.tar
if guest_ssh 'sudo tar -C /var/lib/pkg-s6-evidence -cf - .' >"$archive" 2>/dev/null; then
    tar -C "$out/guest-evidence" -xf "$archive" || guest_status=1
else
    guest_status=1
fi
rm -f "$archive"
[ -f "$out/guest-evidence/results" ] && [ -f "$out/guest-evidence/installer.sha256" ] || guest_status=1
printf '%s\n' "$guest_status" >"$out/guest-exit-status"
private_tree "$out"
rm -f "$overlay"
case $guest_status in
    0) printf '%s\n' "PASS: $lane" ;;
    2|124|137|143) printf '%s\n' "UNPROVED: $lane; see private evidence" >&2; exit 2 ;;
    *) die "FAIL: $lane; see private evidence" ;;
esac
