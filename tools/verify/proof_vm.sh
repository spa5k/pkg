#!/usr/bin/env bash
# DN-1: disposable macOS proof-VM bring-up and dispatch-time operator actions.
#
# Subcommands:
#   clone <slot>        clone the pinned base image into the slot VM (100 GiB disk)
#   boot  <slot>        start the VM and wait for SSH
#   install-runner <slot>  install the GitHub Actions runner (named, labeled)
#   markers <slot> <run-id> <lifecycle>   write the disposable + instance markers
#   reboot <slot> <run-id> <lifecycle>    write the reboot marker and reboot the VM
#   status              show VM + runner state
#
# Slot 1 -> VM pkg-proof-vm-1, runner pkg-dn16-proof-runner-1, label pkg-disposable-macos-proof-1
# Slot 2 -> VM pkg-proof-vm-2, runner pkg-dn16-proof-runner-2, label pkg-disposable-macos-proof-2
#
# Requirements (tests/macos-clean-host/REPEAT.md): disposable Tart VM of the
# pinned sequoia base image, >=100 GiB disk, >=70 GiB free on /, passwordless
# sudo, self-hosted runner inside, never a production machine.
set -euo pipefail

IMAGE='ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c'
REPO=spa5k/pkg
RUNNER_VERSION=2.327.1
VM_USER=admin

slot_vm()   { echo "pkg-proof-vm-$1"; }
slot_runner(){ echo "pkg-dn16-proof-runner-$1"; }
slot_label(){ echo "pkg-disposable-macos-proof-$1"; }

vm_ssh() { # slot cmd...
  local slot=$1; shift
  local vm; vm=$(slot_vm "$slot")
  local ip; ip=$(tart ip "$vm")
  ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o ConnectTimeout=10 -o LogLevel=ERROR "${VM_USER}@${ip}" "$@"
}

case "${1:-}" in
clone)
  slot=$2; vm=$(slot_vm "$slot")
  tart clone "$IMAGE" "$vm"
  tart set --disk-size 100 "$vm"
  echo "cloned $vm (100 GiB)"
  ;;
boot)
  slot=$2; vm=$(slot_vm "$slot")
  setsid nohup tart run "$vm" >"/tmp/${vm}-tart.log" 2>nohup tart run "$vm" >"/tmp/${vm}-tart.log" 2>&1 &1 < /dev/null &
  echo "booting $vm (tart pid $!)"
  for _ in $(seq 1 60); do
    if ip=$(tart ip "$vm" 2>/dev/null) && ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR "${VM_USER}@${ip}" true 2>/dev/null; then
      echo "ssh ready at ${VM_USER}@${ip}"
      exit 0
    fi
    sleep 5
  done
  echo "ssh never became ready" >&2; exit 1
  ;;
install-runner)
  slot=$2; vm=$(slot_vm "$slot"); runner=$(slot_runner "$slot"); label=$(slot_label "$slot")
  token=$(gh api -X POST "repos/${REPO}/actions/runners/registration-token" -q .token)
  vm_ssh "$slot" "set -euo pipefail
    sudo -n true
    avail_kb=\$(df -Pk / | awk 'NR==2 {print \$4}')
    [ \$((avail_kb * 1024)) -ge 75161927680 ] || { echo 'root free < 70 GiB' >&2; exit 1; }
    cd ~
    [ -d actions-runner ] || {
      curl -fsSL -o runner.tar.gz \
        \"https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-osx-arm64-${RUNNER_VERSION}.tar.gz\"
      shasum -a 256 runner.tar.gz >/dev/null  # integrity via TLS + known host
      mkdir actions-runner && tar xzf runner.tar.gz -C actions-runner
      rm runner.tar.gz
    }
    cd actions-runner
    if [ ! -f .runner ]; then
      ./config.sh --unattended --url \"https://github.com/${REPO}\" --token \"$token\" \
        --name \"$runner\" --labels \"$label\" --replace
    fi
    ./svc.sh stop >/dev/null 2>&1 || true
    ./svc.sh install ${VM_USER}
    ./svc.sh start
    echo runner-installed"
  echo "runner $runner installed in $vm (label $label)"
  ;;
markers)
  slot=$2; run_id=$3; lifecycle=$4
  nonce=$(openssl rand -hex 32)
  vm_ssh "$slot" "set -euo pipefail
    printf 'PKG-DN16-DISPOSABLE-V1:${run_id}:${lifecycle}' | sudo -n tee /var/tmp/pkg-disposable-macos-proof >/dev/null
    sudo -n chown root:wheel /var/tmp/pkg-disposable-macos-proof
    sudo -n chmod 600 /var/tmp/pkg-disposable-macos-proof
    printf 'PKG-DN16-INSTANCE-V1:${nonce}' | sudo -n tee /var/tmp/pkg-disposable-macos-instance >/dev/null
    sudo -n chown root:wheel /var/tmp/pkg-disposable-macos-instance
    sudo -n chmod 600 /var/tmp/pkg-disposable-macos-instance
    sudo -n stat -f '%Su:%Sg:%Lp %N' /var/tmp/pkg-disposable-macos-proof /var/tmp/pkg-disposable-macos-instance"
  ;;
reboot)
  slot=$2; run_id=$3; lifecycle=$4
  runner=$(slot_runner "$slot")
  vm_ssh "$slot" "set -euo pipefail
    nonce=\$(sudo -n cat /var/tmp/pkg-disposable-macos-instance)
    nonce=\${nonce#PKG-DN16-INSTANCE-V1:}
    old_boot=\$(sysctl -n kern.bootsessionuuid)
    now=\$(date +%s)
    printf 'PKG-DN16-REBOOT-V2:${run_id}:${lifecycle}:${runner}:%s:%s:%s\n' \
      \"\$nonce\" \"\$old_boot\" \"\$now\" \
      | sudo -n tee /var/tmp/pkg-disposable-macos-reboot-v2 >/dev/null
    sudo -n chown root:wheel /var/tmp/pkg-disposable-macos-reboot-v2
    sudo -n chmod 600 /var/tmp/pkg-disposable-macos-reboot-v2
    echo 'reboot marker written; rebooting'
    sleep 1
    sudo -n shutdown -r now" || true
  echo "rebooting $(slot_vm "$slot"); the runner service reconnects after boot"
  ;;
status)
  tart list | grep pkg-proof-vm || echo "no proof VMs"
  gh api "repos/${REPO}/actions/runners" -q '.runners[]
    | select(.name | startswith("pkg-dn16-proof-runner"))
    | [.name, .status, (.labels | map(.name) | join(","))] | @tsv' 2>/dev/null || echo "no runners"
  ;;
*)
  echo "usage: proof_vm.sh clone|boot|install-runner|markers|reboot|status <slot> [args...]" >&2
  exit 64
  ;;
esac
