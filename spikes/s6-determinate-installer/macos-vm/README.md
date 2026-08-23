# macOS destructive lifecycle lanes

Run the non-destructive static contract from the repository root:

```sh
spikes/s6-determinate-installer/macos-vm/test-static.sh
```

Each destructive lane uses one cloned VM and runs all phases in sequence. Tart uses its default shared NAT. The scripts do not use SSH, shared folders, extra disks, or host `sudo`.

```sh
spikes/s6-determinate-installer/macos-vm/run.sh --approve-destructive-vm --lane lifecycle-diagnostics --installer /ABS/nix-installer-aarch64-darwin --evidence /ABS/NEW/lifecycle
spikes/s6-determinate-installer/macos-vm/run.sh --approve-destructive-vm --lane crash-recovery --installer /ABS/nix-installer-aarch64-darwin --evidence /ABS/NEW/crash
spikes/s6-determinate-installer/macos-vm/run.sh --approve-destructive-vm --lane foreign-nix --installer /ABS/nix-installer-aarch64-darwin --evidence /ABS/NEW/foreign --approve-observe-vendor-foreign-state
spikes/s6-determinate-installer/macos-vm/run.sh --approve-destructive-vm --lane upstream-input --installer /ABS/nix-installer-aarch64-darwin --evidence /ABS/NEW/upstream
```

`--approve-destructive-vm` approves disposable VM mutation. `--approve-observe-vendor-foreign-state` separately approves vendor execution against the owned foreign fixture. Without the second approval, the foreign lane stops after the required refusal proof.

Each evidence path must be absolute, canonical, absent, and on a volume with at least 32 GiB free. Evidence directories are mode `0700`. Evidence files are mode `0600`. Keep the evidence private.

Pinned inputs are Determinate Nix Installer 3.22.1 with SHA-256 `90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b`, vendor source revision `4132ad07a15ee7d88c096ac7172b7afb2672866b`, Tart `2.35.0`, and cached base `ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c`.

Hard blocker: never run the installer unless the host has at least 32 GiB free on both the evidence and Tart storage volumes, and the guest has at least 30 GiB free before its first vendor execution.

Each reboot starts with an outcome of `FAIL` and records the raw pre-reboot
`kern.boottime`. The shutdown call records its exit status and the host-timeout
flag in separate files. Only `0:0` and `124:1` can continue. Shutdown output is
evidence, but it is not reboot proof. The harness waits for a successful guest
`sysctl` result with a different raw boot time. Temporary guest unavailability
and an already-completed reboot are both valid. An equal boot time at the
deadline or any comparison error fails the run. After the boot time changes,
the harness checks passwordless `sudo`, the owner marker, both staged file
hashes, and the Tart process. It writes `PASS` and returns success only after
all checks pass.
