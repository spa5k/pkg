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

## Residue identity evidence

Every guest snapshot records four identity files:

- `<snapshot>.etc-nix.inventory`
- `<snapshot>.fstab.identity`
- `<snapshot>.determinate-nix-init-log.identity`
- `<snapshot>.determinate-nix-daemon-log.identity`

The `/etc/nix` inventory does not follow symbolic links. It stays on one
device. It accepts only directories, regular files, and symbolic links.
Regular files and symbolic links must have one hard link. Paths and symbolic
link targets are hexadecimal bytes. Regular file contents are represented
only by SHA-256. The evidence does not contain fstab bytes, log bytes, or
receipt bytes.

Each snapshot scans `/etc/nix` and `/etc/fstab` twice. It sorts the `/etc/nix`
records with the C locale. It keeps the first scan only when both scans are
byte-for-byte equal. A changing path, an unsupported file type, a root
symbolic link, a file with multiple hard links, or a cross-device entry stops
the phase. Each Determinate log is captured once. Its one capture still uses
the stable lstat-hash-lstat, regular-file, and one-hard-link gates. The harness
does not retry, sleep, or pause a daemon to capture a live log.

The lifecycle lane compares these identities across the exact boundaries:

1. The clean baseline stays unchanged and all four paths are absent.
2. Install starts from the baseline and creates `/etc/nix` and `/etc/fstab`.
3. Uninstall starts from the daemon phase state. `/etc/nix` and `/etc/fstab`
   must match exactly. Each validated log must keep its state, path, type,
   mode, user, group, and hard-link count. Only log size and SHA-256 can drift
   at this active boundary.
4. Repeat uninstall starts from the uninstall state and changes nothing.
5. The final post-reboot state equals the repeat-uninstall state and stays
   unchanged during the final phase.

Both logs must be present after install. All clean-baseline and post-uninstall
boundaries compare all four identity files byte for byte.

The lifecycle produces nine phase archives. The final residue decision occurs
only after the final `after` snapshot and all final comparisons. DN-03c does
not delete `/etc/nix`, `/etc/fstab`, or the Determinate logs. DN-13 can later
remove only residue whose complete identity is proved again at cleanup time.

R8 did not record the contents of `/etc/nix`. It also missed an empty fstab
file and did not inspect the two Determinate log paths. R9 added the identity
contract, but it stopped during install. The active daemon log grew between
the two complete snapshot scans. Both vendor functional checks returned `0`.
R9 is a **NO-GO** and did not reach a reboot.

## R10 observed lifecycle result

R10 ran the full lifecycle once from signed source
`aa5d5beca51d77ae06a672a97c2b5ebfa050d248`. Its private evidence path is
`/private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-aa5d5be-r10`.
All phases from baseline through repeat uninstall passed. Both reboots passed.
The final residue phase returned the expected guest status `1` only because
proved vendor residue remained. Product residue passed.

The active uninstall boundary kept the init-log identity exact. The daemon log
grew, but its state, path, type, mode, user, group, and hard-link count stayed
unchanged. Every post-uninstall identity boundary was byte-for-byte equal.

The exact final vendor residue path set is:

- `/etc/nix`
- `/etc/nix/macos-keychain.crt`
- `/etc/nix/sentry-endpoint`
- `/etc/fstab`
- `/var/log/determinate-nix-init.log`
- `/var/log/determinate-nix-daemon.log`

R10 closes the DN-03c lifecycle and residue evidence gate. The expected
vendor-residue failure does not make that evidence incomplete. It proves the
residue contract. R10 does not complete the separate standalone SIGKILL and
reboot row. R10 also does not prove cleanup. DN-13 may remove only this
manifest, and only after it revalidates every live identity and fails closed
on a difference.

## Crash R1 observed negative result

**Observed.** Crash R1 ran from signed source
`1ad44acf6c7780fa5ed3e135c1fcdc734149402f`. Baseline and crash-kill passed.
The validated installer child ended with status `137` after SIGKILL. Reboot
passed with shutdown status and timeout `0:0`. The raw boot time changed, and
the staged hashes were revalidated. Recovery install returned `0`.

The final crash-recover phase returned guest status `1` and `FAIL`. The
`nixbld` group had GID `350`. It had 31 explicit members, from `_nixbld2`
through `_nixbld32`. `_nixbld1` was missing. The lane stopped before a
functional Nix recovery check.

**Decision.** This accepted negative observation completes the standalone
SIGKILL and reboot evidence row. The DN-03c evidence set is complete. Product
delivery remains **NO-GO**.

**Unproved.** Crash R1 does not prove successful recovery or functional Nix
recovery. DN-16 remains blocked until two crash and reboot lifecycles pass.
DN-13 does not own this failure.
