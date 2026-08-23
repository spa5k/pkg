# DN-03c research: Determinate macOS mount contract

| Item | Value |
|---|---|
| Installer release | `v3.22.1` |
| Pinned source revision | `4132ad07a15ee7d88c096ac7172b7afb2672866b` |
| Research date | 2026-08-23 |
| Scope | macOS encrypted APFS `/nix` mount, `/etc/fstab`, and install self-test warnings |
| Evidence rule | Pinned primary-source analysis plus preserved R4, R5, and R6 observations. No private receipt contents were read. No private evidence was changed. |

In this report, **r2** means the reported first lifecycle attempt. **r3** means
the evidence-only harness revision. **R4** means the preserved run that used
that revision. **R5** means the preserved run that used the UUID comparison
fix. **R6** means the preserved run at product revision `4fb8c70`.

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

## Decision table

| Decision | Result | Reason |
|---|---|---|
| Is the exact vendor fstab contract known? | **GO** | The pinned source constructs the full line directly. |
| Is a persistent mount service part of the contract? | **GO** | The pinned source creates and loads `systems.determinate.nix-store`. |
| Can DN-03c change the UUID comparison now? | **GO** | R4 proves that only UUID letter case differs. Lower-case only the expected UUID. |
| Did r3 add evidence capture before the gate? | **DONE** | R4 preserved the raw UUID, raw fstab line, and probe results before the strict gate. |
| Should `nix: not found` make installer exit `0` fail? | **NO-GO** | The installer treats self-test failures as warnings. With `--no-modify-profile`, a bare `nix` command can be absent from shell `PATH`. |
| Should the harness keep its absolute-path Nix checks? | **GO** | They test the installed binary and daemon without depending on shell profile changes. |
| Is DN-03c complete after R4? | **NO-GO** | R4 stopped at the strict fstab comparison. A later run must prove the remaining lifecycle. |

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
- Raw fstab capture and receipt opacity must remain.

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

The next step is a new lifecycle run with the corrected exact gate. DN-03c
remains a **NO-GO** until the remaining launchd, mount, absolute Nix, reboot,
repair, uninstall, and residue checks pass on the pinned guest.

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
