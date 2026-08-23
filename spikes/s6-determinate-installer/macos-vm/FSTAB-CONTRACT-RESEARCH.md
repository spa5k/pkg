# DN-03c research: Determinate macOS mount contract

| Item | Value |
|---|---|
| Installer release | `v3.22.1` |
| Pinned source revision | `4132ad07a15ee7d88c096ac7172b7afb2672866b` |
| Research date | 2026-08-24 |
| Scope | macOS encrypted APFS `/nix` mount, `/etc/fstab`, install self-test warnings, and residue identity |
| Evidence rule | Pinned primary-source analysis plus preserved R4, R5, R6, R7, and R8 observations. No private receipt contents were read. No private evidence was changed. |

In this report, **r2** means the reported first lifecycle attempt. **r3** means
the evidence-only harness revision. **R4** means the preserved run that used
that revision. **R5** means the preserved run that used the UUID comparison
fix. **R6** means the preserved run at product revision `4fb8c70`. **R7** means
the preserved run at product revision `23195d1`. **R8** means the preserved
run at product revision `650e205`. **R9** means the required fresh run with the
byte-safe residue identity contract described in this report.

## Short answer

The pinned installer creates the same fstab field template as the DN-03c
harness. It formats the UUID differently.

```text
UUID=<uuid> /nix apfs rw,noatime,noauto,nobrowse,nosuid,owners # Added by the Determinate Nix Installer
```

The installer also creates a root launchd service. The service runs
`/usr/local/bin/determinate-nixd init` at load time. Thus, the persistent mount
contract has more than one part:

1. macOS creates the synthetic `/nix` path.
2. The installer creates and encrypts the `Nix Store` APFS volume.
3. The installer writes the UUID-based `/etc/fstab` line.
4. A root launchd service runs `determinate-nixd init` to mount the store.

The R4 evidence proves that the failed exact comparison was a UUID letter-case
mismatch.

The installer parses `VolumeUUID` as a Rust `Uuid`. It formats `{uuid}` in the
fstab line. The `uuid` crate formats `Display` output as lower-case,
hyphenated text. The harness reads the raw `diskutil` value and requires
upper-case text. A correct lower-case vendor line does not equal the harness's
upper-case expected line.

Only the expected UUID text must change. The raw `diskutil` UUID evidence and
its upper-case format validation must not change. Every other fstab field and
strict count gate must remain exact.

## R4 observed evidence

The preserved evidence path is
`/private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-f65b6e4-r4`.

The following values are **Observed** in R4:

| Evidence | Observed value |
|---|---|
| `baseline` phase archive SHA-256 | `ad7a98cfc05668f918d4f9c029de4aeda3b410376781f268b1d9eab3b6a8e604` |
| `lifecycle-install` phase archive SHA-256 | `5d6c63b9d0a9865aed14556fa5398e57c5aa770174dd990e3974ffc93773c815` |
| Raw `diskutil` UUID | `4540405A-CCE8-4E05-9632-CB2A88D70667` |
| Vendor fstab UUID | `4540405a-cce8-4e05-9632-cb2a88d70667` |
| Installer status | `0` |
| `determinate-nixd status` probe | `0` |
| Nix daemon store-ping probe | `0` |
| Strict expected-line count | `0` |

All fstab fields other than UUID letter case matched. This is **Observed**.

R4 stopped at the strict comparison. It does not prove a complete lifecycle,
a reboot result, repair, uninstall, or residue cleanup.

## R5 observed evidence and limits

The preserved evidence path is
`/private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-7da72d9-r5`.

The following facts are **Observed** in R5:

- The product revision is
  `7da72d96a79139f77220e9490a5ad500e896425a`.
- The baseline phase passed.
- The install command returned status `0`.
- The `determinate-nixd status` probe returned status `0`.
- The Nix daemon store-ping probe returned status `0`.
- The strict fstab expected-line count is `1`. Thus, the UUID fix passed.
- The lifecycle-install phase then returned status `1` and recorded `FAIL`.
- Its exact error says that the identity of
  `/Library/LaunchDaemons/systems.determinate.nix-store.plist` is unexpected.
- The baseline and lifecycle-install archives passed validation. Their saved
  SHA-256 values equal the hashes of the preserved files. Their list and
  verbose manifests have equal entry counts. Their validation error files are
  empty.
- Both valid archives remain named `.tar.part`. Neither became `.tar`.
- No later lifecycle phase ran.
- Cleanup proved that the exact VM was absent. Both final local-VM lists are
  empty.

The following conclusions are **Source-based inferences**:

- The harness keeps `umask 077` for private evidence. Before this fix,
  `run_recorded` launched every child without changing that mask. The vendor
  installer therefore inherited `077`. This explains the later launchd
  identity failure. R5 did not archive the actual plist mode.
- POSIX shell function variables are global unless a shell provides and uses
  a non-standard local-variable feature. `validate_phase_archive` assigned
  its second argument to `archive`. This overwrote the `.tar` destination in
  `capture_phase` with the `.tar.part` source. The later move therefore used
  the partial path as both source and destination. This explains why both
  validated archives kept the `.tar.part` suffix.

R5 does not prove receipt metadata, reboot, repeat install, repair, daemon
phase behavior, uninstall, repeat uninstall, or residue cleanup.

## R6 observed evidence and limits

The preserved evidence path is
`/private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-4fb8c70-r6`.

**Observed:** Baseline and lifecycle-install passed with guest status `0`.
Shutdown returned `0`. The Guest Agent returned, identity revalidation passed,
and raw `kern.boottime` changed. Cleanup proved exact VM absence. No later
phase ran.

**Inference:** `reboot_guest` ended with `cmp ... && die`. The changed files
made `cmp` return `1`. That status became the function status and stopped the
lane under `set -e`.

**Unproved:** Repeat install, repair, daemon behavior, uninstall, repeat
uninstall, the second reboot, and residue cleanup. Receipt contents were not
read.

## R7 observed evidence and limits

The preserved evidence path is
`/private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-23195d1-r7`.

**Observed:** The product source is `23195d1`. All phases from baseline through
lifecycle-repeat-uninstall passed with guest status `0`. The run finalized and
validated eight archives. No `.tar.part` file remains. The first shutdown
returned raw status `0`. The Guest Agent became unavailable and then ready.
Identity revalidation passed, and raw `kern.boottime` changed. The second
shutdown printed the same normal text as the first shutdown. Its raw status was
`124`, and its status file appeared about 35 seconds later. The run did not
record a second post-reboot boot time. It did not run the second identity
revalidation, lifecycle-residue, or final reboot validation. Exact VM cleanup
passed, and no VM remains. The evidence tree is private and has no symlinks.
The tree and archives contain no `receipt.json`. Receipt evidence contains only
metadata, size, and SHA-256 identity. Receipt contents were not read.

**Inference:** The host deadline in `wait_pid` produced status `124`. The
shutdown connection did not return before that deadline. The normal shutdown
text does not prove that the reboot completed.

**Unproved:** The second reboot, post-second-reboot identity, and final residue
remain unproved.

## R8 Observed/Inference/Unproved

The preserved evidence path is
`/private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-650e205-r8`.
The runtime source is `650e205`.

**Observed:**

- The installer SHA-256 is
  `90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b`.
  Its size is 58,427,232 bytes. Its mode is `0700`.
- Both reboots passed. Each shutdown recorded status and timeout as `0:0`.
  Each raw boot time changed. The separate boot-time stderr files are empty.
  Installer and inside-script revalidation matched after each reboot.
- Baseline, install, post-reboot, repeat install, repair, daemon, uninstall,
  and repeat uninstall passed with guest status `0`.
- The lifecycle-residue phase returned guest status `1`. The old scanner
  reported `/etc/nix`. This was the only path that the old scanner reported.
  This result does not prove that it was the only residue.
- The baseline had no `/etc/nix`. Install created `/etc/nix` as
  `root:wheel`, mode `0755`, size 192. Uninstall left it as `root:wheel`, mode
  `0755`, size 128. That top-level identity stayed stable through repeat
  uninstall and the final reboot.
- The baseline had no `/etc/fstab`. The final state had a regular
  `/etc/fstab` owned by `root:wheel`, mode `0644`, size 0. The old residue
  scanner did not report this empty file because it searched for Nix text.
- Installed launchd evidence names `/var/log/determinate-nix-init.log` and
  `/var/log/determinate-nix-daemon.log`. The R8 scanner did not inspect these
  fixed paths.
- The receipt was `root:wheel`, mode `0644`, size 35,811, with SHA-256
  `d7c9336a64c6411395188e787f7a11327d4d3c40060ac4d99115c1d3fae96a4d`.
  It was absent after uninstall. Only receipt metadata, size, and SHA-256 were
  saved. Receipt contents were not read.
- All nine phase archives are valid and final. No `.tar.part` remains. The
  evidence is private. It has no symlink or non-regular archive entry. No
  archive contains `receipt.json`.
- Exact VM and Tart cleanup passed. The runtime source worktree is clean.

The nine archive SHA-256 values are:

| Phase | SHA-256 |
|---|---|
| baseline | `f027ad775254278daea0b61e9cb2a9a466795678b4b8af114e71a5a84094de3f` |
| lifecycle-install | `acb1331fc196b99b0ea2604e53bc6aa03186ba2e23ee2b2b4fa48a774f681085` |
| lifecycle-post-reboot | `15f86a0bf8654afb13f5bb42f77114ed48df65e80b333c4762dfb22c9a65acd5` |
| lifecycle-repeat-install | `5d323463165457d29f9504d46c4db51b449ed6a72d5e8d6718886417b3e303b7` |
| lifecycle-repair | `70394bb941f5c0166367216676bc409eb697c421d66871c9af6756a09c1562d5` |
| lifecycle-daemon | `eb5b7c1c397b5ef59c21c5f1d8a6f80b6675b80ec4a5a89c92308a96fb4787d9` |
| lifecycle-uninstall | `d177e14634fbd72e23c28cbbd11d9d7856aa079e659d0fe92d5c26028706e62d` |
| lifecycle-repeat-uninstall | `f4a2cf784e8d3959e32bb95099ea158f850a16279b455675bba635ea301f5965` |
| lifecycle-residue | `bf2ed6e92679e69527e8a851f5eed9a92b88f95b62a2abefa3113b877786878c` |

**Inference:**

- The vendor uninstall leaves at least the observed `/etc/nix` directory and
  empty `/etc/fstab` on this pinned guest.
- The old text-based fstab check was too narrow. An empty file differs from an
  absent baseline even though it contains no Nix text.
- The stable top-level `/etc/nix` metadata does not prove stable children.
  Different trees can have the same top-level mode, owner, and size.

**Unproved:**

- The exact `/etc/nix` child paths, types, modes, owners, sizes, hard-link
  counts, regular-file hashes, and symbolic-link targets are unproved.
- The presence and identity of the two Determinate log files are unproved.
- The exact complete vendor residue is unproved. `/etc/nix` must not be
  described as the only residue.
- R8 is a **NO-GO** for DN-03c completion. A fresh full R9 run is required.

## R9 required residue contract

R9 must run the complete lifecycle from a clean pinned guest. It must finalize
exactly nine phase archives. It must not copy fstab, log, or receipt contents.

Each snapshot must save these four files:

- `.etc-nix.inventory`
- `.fstab.identity`
- `.determinate-nix-init-log.identity`
- `.determinate-nix-daemon-log.identity`

The `/etc/nix` inventory uses `find -P` and `-xdev`. Found paths stay in argv;
they are not parsed as newline-delimited text. Path bytes and symbolic-link
target bytes use hexadecimal text. Regular files require one hard link and a
stable lstat-hash-lstat result. Symbolic links require one hard link and a
stable lstat-readlink-lstat result. Directories are allowed. Every other type
and every cross-device entry is rejected. Each complete sorted scan must match
a second scan byte for byte.

The baseline before and after identities must match, and all four paths must
be absent. Install must start from the baseline and must create `/etc/nix` and
`/etc/fstab`. Uninstall must start from the daemon post-state. Repeat uninstall
must start from the uninstall post-state and change no identity. After the
final reboot, the residue pre-state must equal the repeat-uninstall post-state.
The final residue after-state must equal its pre-state.

Strict residue compares the final identities with the baseline identities.
Thus, absent-to-present `/etc/nix`, fstab, or log identities are residue. Any
fstab identity difference is residue, including an empty file. Clean-baseline
R9 forbids pre-existing identities and requires all four paths to be absent.
The final residue failure, when present, must occur only after the final
`after` snapshot and all final comparisons.

DN-03c records this contract. It does not delete the residue. DN-13 must later
revalidate the complete exact identity before any fail-closed cleanup.

## Decision table

| Decision | Result | Reason |
|---|---|---|
| Is the exact vendor fstab contract known? | **GO** | The pinned source constructs the full line directly. |
| Is a persistent mount service part of the contract? | **GO** | The pinned source creates and loads `systems.determinate.nix-store`. |
| Can DN-03c change the UUID comparison now? | **GO** | R4 proves that only UUID letter case differs. Lower-case only the expected UUID. |
| Did r3 add evidence capture before the gate? | **DONE** | R4 preserved the raw UUID, raw fstab line, and probe results before the strict gate. |
| Should `nix: not found` make installer exit `0` fail? | **NO-GO** | The installer treats self-test failures as warnings. With `--no-modify-profile`, a bare `nix` command can be absent from shell `PATH`. |
| Should the harness keep its absolute-path Nix checks? | **GO** | They test the installed binary and daemon without depending on shell profile changes. |
| Is DN-03c complete after R8? | **NO-GO** | R8 completed the lifecycle, but its scanner did not prove exact `/etc/nix`, empty fstab, or Determinate log residue. A fresh R9 run is required. |

## 1. Source and release identity

The official [`v3.22.1` release](https://github.com/DeterminateSystems/nix-installer/releases/tag/v3.22.1)
points to commit `4132ad0`. The repository already pins the full revision
`4132ad07a15ee7d88c096ac7172b7afb2672866b` in the macOS harness
[README](./README.md).

All installer source links below use the full pinned revision. They do not use
`main` or a moving tag.

## 2. Exact macOS volume and mount sequence

### 2.1 Synthetic `/nix`

The Determinate volume plan inserts `nix\n` into `/etc/synthetic.conf`. It then
plans macOS synthetic-object creation. This makes `/nix` available as a system
path on macOS. See
[`create_determinate_nix_volume.rs` lines 61-80](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_determinate_nix_volume.rs#L61-L80).

### 2.2 APFS volume

The installer creates an APFS volume named `Nix Store`. The action uses
`diskutil apfs addVolume ... -nomount`. See
[`create_apfs_volume.rs` lines 91-109](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_apfs_volume.rs#L91-L109).

For the Determinate distribution, the macOS planner always uses the dedicated
encrypted-volume action. See
[`create_determinate_nix_volume.rs` lines 92-106](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_determinate_nix_volume.rs#L92-L106).

The encryption action stores the generated volume password in the System
Keychain. It grants `/usr/local/bin/determinate-nixd` access to that item. It
then runs `diskutil apfs encryptVolume` with the password on standard input.
See
[`encrypt_apfs_volume.rs` lines 204-276](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/encrypt_apfs_volume.rs#L204-L276).

### 2.3 `/etc/fstab`

The installer waits until `diskutil info -plist Nix Store` succeeds. It then
writes fstab before it encrypts and mounts the volume. See
[`create_determinate_nix_volume.rs` lines 201-254](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_determinate_nix_volume.rs#L201-L254).

The fstab action does these operations:

1. It reads the APFS `VolumeUUID`.
2. It reads `/etc/fstab`, or starts with an empty file if the file is absent.
3. It removes old installer prelude comments.
4. It removes entries that its simple parser identifies as `/nix` entries.
5. It appends exactly one new `/nix` line.
6. It writes the file atomically.

See
[`create_fstab_entry.rs` lines 60-112](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_fstab_entry.rs#L60-L112).

The exact formatter is:

```rust
format!("UUID={uuid} /nix apfs rw,noatime,noauto,nobrowse,nosuid,owners # Added by the Determinate Nix Installer")
```

See
[`create_fstab_entry.rs` lines 171-173](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_fstab_entry.rs#L171-L173).

The harness has the same fields, option order, spacing, and comment. See the
local [`inside.sh`](./inside.sh) contract.

### 2.4 Immediate mount and persistent service

After fstab and encryption, the installer runs:

```text
/usr/local/bin/determinate-nixd init --stop-after mount
```

It requires this command to succeed. It then waits for `/nix`. See
[`create_determinate_nix_volume.rs` lines 231-254](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_determinate_nix_volume.rs#L231-L254).

The installer also creates this launchd service:

| Field | Pinned value |
|---|---|
| Path | `/Library/LaunchDaemons/systems.determinate.nix-store.plist` |
| Label | `systems.determinate.nix-store` |
| `RunAtLoad` | `true` |
| `ProgramArguments[0]` | `/usr/local/bin/determinate-nixd` |
| `ProgramArguments[1]` | `init` |
| Log | `/var/log/determinate-nix-init.log` |

See
[`create_determinate_nix_volume.rs` lines 26-28](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_determinate_nix_volume.rs#L26-L28)
and
[`create_determinate_volume_service.rs` lines 186-204](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_determinate_volume_service.rs#L186-L204).

There is one `use_ec2_instance_store` variant. If the macOS planner option
`use_ec2_instance_store` is true, the service adds
`ProgramArguments[2] = --keep-mounted`. The planner default is false. The
DN-03c command does not select this option. Thus, the standard Tart lane must
have only the two arguments in the table. See
[`planner/macos/mod.rs` lines 90-94 and 135-144](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/planner/macos/mod.rs#L90-L144)
and
[`create_determinate_volume_service.rs` lines 187-204](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_determinate_volume_service.rs#L187-L204).

The installer bootstraps and kick-starts that service after it writes the plist.
See
[`create_determinate_nix_volume.rs` lines 256-274](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_determinate_nix_volume.rs#L256-L274).

### 2.5 What is proved and what is not

**Proved by the pinned installer source:**

- The installer intends to write the exact fstab fields and comment.
- It writes fstab before the immediate mount command.
- A write failure returns an action error. It is not ignored.
- It creates a `RunAtLoad` launchd service for persistent mount work.

**Not proved by this source review:**

- The exact fstab bytes that existed in the r2 guest after the installer ran.
- The exact service plist bytes that existed in that guest.
- The internal mount algorithm of the embedded `determinate-nixd` binary.
- Successful remount after reboot on the pinned Tart guest image.

## 3. Comparison with the DN-03c harness

The vendor and harness contracts are equal except for how they obtain and
format the UUID.

| Part | Vendor source | Current harness |
|---|---|---|
| UUID input | Parses `VolumeUUID` into `uuid::Uuid` | Reads raw `VolumeUUID` text with `plutil` |
| UUID validation | Rust UUID parser | Requires upper-case raw `diskutil` text |
| UUID output | Formats `{uuid}` | Lower-cases only the UUID used in the expected fstab line |
| Mount point | `/nix` | `/nix` |
| Filesystem | `apfs` | `apfs` |
| Options | `rw,noatime,noauto,nobrowse,nosuid,owners` | Same |
| Comment | `# Added by the Determinate Nix Installer` | Same |
| Count | Appends one after removing rows its parser identifies as `/nix` lines | Requires one exact line and one `/nix` entry |

The vendor deserializes `VolumeUUID` into `uuid::Uuid`. See
[`macos/mod.rs` lines 89-95](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/mod.rs#L89-L95).

The pinned
[`Cargo.lock` lines 3014-3020](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/Cargo.lock#L3014-L3020)
selects `uuid` `1.24.1`. The crate's official
[`Uuid` documentation](https://docs.rs/uuid/1.24.1/uuid/struct.Uuid.html#formatting)
shows that default `Display` is a lower-case, hyphenated value.

The harness still records and validates the raw upper-case value. It now
lower-cases only `A` through `F` while it builds the expected fstab line. See
[`inside.sh`](./inside.sh).

R4 reached the exact fstab string comparison. The raw upper-case UUID
validation passed. The installer and both evidence probes returned status `0`.
The raw vendor line used the same UUID in lower-case text. All other fstab
fields matched. These values are **Observed**.

### Source-backed example

If `diskutil` returns this value:

```text
936DA01F-9ABD-4D9D-80C7-02AF85C822A8
```

the installer can write:

```text
UUID=936da01f-9abd-4d9d-80c7-02af85c822a8 /nix apfs rw,noatime,noauto,nobrowse,nosuid,owners # Added by the Determinate Nix Installer
```

The pre-fix harness expected:

```text
UUID=936DA01F-9ABD-4D9D-80C7-02AF85C822A8 /nix apfs rw,noatime,noauto,nobrowse,nosuid,owners # Added by the Determinate Nix Installer
```

`grep -Fxc` writes count `0` and exits nonzero because the comparison is
case-sensitive. The harness then stops the phase. This grep runs after the
installer's immediate mount command. It does not change mount state. Thus, an
existing mount and a failed later text comparison can coexist.

R4 observed this combination:

- installer status `0`;
- both evidence probe statuses `0`;
- exact fstab count `0`.

The case-only cause is no longer an inference. R4 captured both UUID forms.

## 4. Other lifecycle risks

R4 closes the cause of this exact comparison failure. It does not close the
rest of the lifecycle:

| Possible cause | Source assessment | Required evidence |
|---|---|---|
| UUID case only | Observed cause of the strict count `0` | Lower-case only the expected UUID |
| Another process changes fstab later | Not closed by the stopped R4 run | Before/after fstab hashes and timestamps |
| A different installer binary ran | Reduced by existing version and SHA gates | Recorded installer SHA and version |
| The launchd service mounted without a valid fstab line | Not proved by installer source | Raw service plist, launchd job state, fstab line, and mount state from one phase |
| The fstab line is truly absent | Conflicts with a clean execution of this action sequence | Installer output, fstab metadata, service evidence, and exact source pin |

The installer removes entries that its simple parser identifies as `/nix`
fstab lines before it adds its own. This is a mutation risk. The existing
strict foreign-Nix preflight must remain. Do not use the writer on a guest that
has an unowned `/nix` entry.

## 5. `nix: not found` self-test warnings

### 5.1 The installer runs self-tests after install actions

After all actions finish, the installer writes the receipt. It then runs the
self-test. A self-test failure is sent to feedback and logged as a warning. The
install function continues and returns success. See
[`plan.rs` lines 180-207](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/plan.rs#L180-L207).

Thus, these two results can occur together by design:

- the install process exits `0`;
- the output contains self-test warnings.

### 5.2 What the self-test runs

The self-test discovers `sh`, `bash`, `fish`, and `zsh`. It starts each present
shell as a login or interactive shell. It then runs a bare `nix build` command.
It does not use `/nix/var/nix/profiles/default/bin/nix`. See
[`self_test.rs` lines 67-145](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/self_test.rs#L67-L145).

A `nix: not found` message proves that the tested shell did not find bare
`nix` in `PATH`. It does not prove that the absolute Nix binary, APFS store, or
daemon is broken.

### 5.3 Effect of `--no-modify-profile`

The flag sets `modify_profile` to `false`. See
[`settings.rs` lines 68-79](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/settings.rs#L68-L79).

When this value is false:

- the installer still creates the default Nix profile;
- it does not change shell profile files;
- on macOS, it does not add the `systems.determinate.nix-installer.nix-hook`
  launchd action.

See
[`configure_nix.rs` lines 27-45](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/configure_nix.rs#L27-L45)
and
[`planner/macos/mod.rs` lines 287-306](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/planner/macos/mod.rs#L287-L306).

The source does not skip self-tests when `modify_profile` is false. Therefore,
`nix: not found` warnings are source-consistent and expected on a clean shell
that does not already have Nix in `PATH`. They are not guaranteed on every
host, because a host can already have a suitable `PATH`.

Reboot alone is not proved to add `nix` to `PATH` when profile modification is
disabled. The installer success message tells the user to source the vendor
profile script directly. See
[`install/mod.rs` lines 328-340](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/subcommand/install/mod.rs#L328-L340).

### 5.4 Harness treatment

Keep the warnings as recorded evidence. Do not convert them into an install
failure when the vendor process returned `0`.

Keep the current functional checks that call the absolute executable:

```text
/nix/var/nix/profiles/default/bin/nix --version
/nix/var/nix/profiles/default/bin/nix store ping --store daemon
```

These checks test the installed product without depending on a shell profile.
If either absolute-path check fails, the installation is not accepted.

## 6. Smallest safe R4 follow-up

R3 added the evidence capture before the strict fstab assertion. R4 then
proved a case-only UUID difference.

The follow-up changes one expression in the expected line:

```sh
$(printf '%s\n' "$installed_uuid" | tr 'ABCDEF' 'abcdef')
```

This expression changes only hexadecimal letters in the comparison UUID. It
does not normalize the full fstab line.

Keep all of these controls unchanged:

- The raw UUID evidence keeps the upper-case `diskutil` value.
- The raw UUID must still match the upper-case UUID format.
- The mount point must be `/nix`.
- The filesystem must be `apfs`.
- The option order, spacing, and vendor comment must match exactly.
- The full vendor line must occur exactly once.
- `/etc/fstab` must contain exactly one `/nix` row.
- Fstab identity and SHA-256 evidence must remain. Fstab and receipt bytes must
  not be copied into phase evidence.

After this change, run the lifecycle lane again from a clean pinned guest. R4
does not supply the later reboot, repair, uninstall, and residue results.

## 7. Final conclusion

The Determinate installer is not using a hidden replacement for fstab. At the
pinned revision, it creates both the UUID fstab entry and a root launchd mount
service.

R4 proves that the pre-fix harness line matched the vendor line in all fields
other than UUID letter case. The harness now lower-cases only the expected
UUID.

The self-test warnings are separate. The installer intentionally logs
self-test failures as warnings and still succeeds. With
`--no-modify-profile`, a bare `nix` lookup can fail even when the absolute Nix
binary and daemon work.

R8 proved the complete phase and reboot sequence on the pinned guest. It also
proved that the old scanner was incomplete. The scanner reported `/etc/nix`,
but it did not inventory its children. It missed a new empty `/etc/fstab`. It
did not inspect the two Determinate logs. Thus, the exact complete residue is
still unproved.

DN-03c remains a **NO-GO** until a fresh R9 run produces all nine final
archives with the byte-safe identity contract. DN-03c must not clean these
paths. DN-13 can later clean only an exact identity that it revalidates.

## Primary sources

- [Determinate Nix Installer v3.22.1 release](https://github.com/DeterminateSystems/nix-installer/releases/tag/v3.22.1)
- [Pinned `Cargo.lock`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/Cargo.lock)
- [Pinned `create_apfs_volume.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_apfs_volume.rs)
- [Pinned `create_fstab_entry.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_fstab_entry.rs)
- [Pinned `create_determinate_nix_volume.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_determinate_nix_volume.rs)
- [Pinned `encrypt_apfs_volume.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/encrypt_apfs_volume.rs)
- [Pinned `create_determinate_volume_service.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_determinate_volume_service.rs)
- [Pinned macOS action module](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/mod.rs)
- [Pinned `configure_nix.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/configure_nix.rs)
- [Pinned macOS planner](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/planner/macos/mod.rs)
- [Pinned install command](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/subcommand/install/mod.rs)
- [Pinned `plan.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/plan.rs)
- [Pinned `self_test.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/self_test.rs)
- [Pinned `settings.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/settings.rs)
- [uuid 1.24.1 `Uuid` formatting documentation](https://docs.rs/uuid/1.24.1/uuid/struct.Uuid.html#formatting)
