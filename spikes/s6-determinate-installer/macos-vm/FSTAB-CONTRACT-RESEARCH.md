# DN-03c research: Determinate macOS mount contract

| Item | Value |
|---|---|
| Installer release | `v3.22.1` |
| Pinned source revision | `4132ad07a15ee7d88c096ac7172b7afb2672866b` |
| Research date | 2026-08-23 |
| Scope | macOS encrypted APFS `/nix` mount, `/etc/fstab`, and install self-test warnings |
| Evidence rule | Primary sources only. No private receipt contents were read. No private evidence was changed. |

In this report, **r2** means the reported prior lifecycle attempt. **r3** means
the next evidence-only lifecycle attempt.

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

A strong source-backed hypothesis for installer exit `0`, a valid APFS mount,
and harness `fstab-count = 0` is a UUID letter-case mismatch.

The installer parses `VolumeUUID` as a Rust `Uuid`. It formats `{uuid}` in the
fstab line. The `uuid` crate formats `Display` output as lower-case,
hyphenated text. The harness reads the raw `diskutil` value and requires
upper-case text. A correct lower-case vendor line does not equal the harness's
upper-case expected line.

This is a source-backed explanation. It is not runtime proof. The next run must
capture the actual fstab line and launchd service evidence before any exact
comparison stops the phase.

## Decision table

| Decision | Result | Reason |
|---|---|---|
| Is the exact vendor fstab contract known? | **GO** | The pinned source constructs the full line directly. |
| Is a persistent mount service part of the contract? | **GO** | The pinned source creates and loads `systems.determinate.nix-store`. |
| Can DN-03c relax the fstab gate now? | **NO-GO** | The current runtime evidence has not been inspected in this research task. Source analysis alone does not prove the guest's actual line. |
| Should r3 add evidence capture before the gate? | **GO** | This is the smallest safe next step. It does not change acceptance. |
| Should `nix: not found` make installer exit `0` fail? | **NO-GO** | The installer treats self-test failures as warnings. With `--no-modify-profile`, a bare `nix` command can be absent from shell `PATH`. |
| Should the harness keep its absolute-path Nix checks? | **GO** | They test the installed binary and daemon without depending on shell profile changes. |
| Is DN-03c complete after this research? | **NO-GO** | A destructive install and reboot lane must still prove the contract on the pinned guest. |

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
| UUID validation | Rust UUID parser | Requires upper-case hexadecimal text |
| UUID output | Formats `{uuid}` | Reuses raw upper-case text |
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

The harness validates an upper-case value and uses it without normalization.
See [`inside.sh`](./inside.sh).

The reported r2 phase reached the exact fstab string comparison. In the
harness, the raw APFS UUID upper-case validation runs immediately before that
comparison. Thus, the upper-case UUID validation had already passed when the
exact fstab string comparison failed. The remaining source-backed hypothesis
is a difference between that accepted upper-case value and the vendor's
lower-case formatted value.

### Source-backed example

If `diskutil` returns this value:

```text
936DA01F-9ABD-4D9D-80C7-02AF85C822A8
```

the installer can write:

```text
UUID=936da01f-9abd-4d9d-80c7-02af85c822a8 /nix apfs rw,noatime,noauto,nobrowse,nosuid,owners # Added by the Determinate Nix Installer
```

The current harness expects:

```text
UUID=936DA01F-9ABD-4D9D-80C7-02AF85C822A8 /nix apfs rw,noatime,noauto,nobrowse,nosuid,owners # Added by the Determinate Nix Installer
```

`grep -Fxc` writes count `0` and exits nonzero because the comparison is
case-sensitive. The harness then stops the phase. This grep runs after the
installer's immediate mount command. It does not change mount state. Thus, an
existing mount and a failed later text comparison can coexist.

This is the best source-backed explanation for the reported combination:

- installer status `0`;
- encrypted APFS `Nix Store` mounted at `/nix`;
- exact fstab count `0`.

It is still an inference until the raw guest fstab line is captured.

## 4. Other possible explanations

The following explanations are less consistent with the pinned source, but the
next evidence run must keep them open:

| Possible cause | Source assessment | Required evidence |
|---|---|---|
| UUID case only | Strong candidate | Raw `/etc/fstab` `/nix` line and raw `VolumeUUID` |
| Another process changed fstab after install | Possible | Before/after fstab hashes and timestamps |
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

## 6. Smallest safe r3 change

Do not relax the current exact gate yet.

Make one evidence-only change to the macOS guest harness:

1. After installer status `0`, capture every raw `/etc/fstab` line whose second
   field is `/nix`.
2. Capture `/etc/fstab` metadata and SHA-256.
3. Capture `diskutil info -plist 'Nix Store'` and its raw `VolumeUUID`.
4. Capture the store plist metadata, SHA-256, program arguments, and
   `launchctl print system/systems.determinate.nix-store` result.
5. Capture the `/nix` mount line.
6. Only then run the current exact fstab and service acceptance checks.

Use the native commands that the harness already uses. Do not add a dependency
or a new parser. Do not read receipt contents.

The most minimal implementation is to move or duplicate the existing raw
fstab, plist, launchd, and mount captures so they run before the first exact
fstab assertion. Keep the exact assertion unchanged for this evidence run.

### r3 decision after evidence

| Observed evidence | Next decision |
|---|---|
| One vendor line exists and only UUID letter case differs | Canonicalize the expected UUID to lower-case, then keep the full exact line and uniqueness checks. |
| One line exists but options, spacing, or comment differ | Stop. Compare the binary pin and actual source path. Do not relax the gate. |
| No `/nix` fstab line exists | Stop. Treat the persistent contract as failed. Investigate the service and installer execution path. |
| Store plist or launchd job differs | Stop. Do not accept mount state alone. |
| Fstab, service, mount, and absolute Nix checks pass before and after reboot | Accept this contract row for the pinned guest and revision. |

If the evidence proves a case-only difference, the later code change can be
small:

```sh
installed_uuid_fstab=$(printf '%s\n' "$installed_uuid" | tr '[:upper:]' '[:lower:]')
```

Use this normalized value only to build the expected vendor fstab line. Keep
the raw upper-case `diskutil` UUID validation and evidence file. This preserves
the exact vendor contract. It does not change the option, comment, mount point,
or count gates.

## 7. Final conclusion

The Determinate installer is not using a hidden replacement for fstab. At the
pinned revision, it creates both the UUID fstab entry and a root launchd mount
service.

The current harness line matches the vendor source in all fields other than
UUID letter case. The probable error is that the harness expects an upper-case
UUID while the Rust formatter writes lower-case text.

The self-test warnings are separate. The installer intentionally logs
self-test failures as warnings and still succeeds. With
`--no-modify-profile`, a bare `nix` lookup can fail even when the absolute Nix
binary and daemon work.

The next step is evidence capture, not gate relaxation. DN-03c remains a
**NO-GO** until the actual fstab line, launchd service, mount state, absolute
Nix checks, and reboot behavior all pass on the pinned guest.

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
