# DN-13 residue ownership and safe cleanup research

| | |
|---|---|
| **Question** | Can `pkg` safely remove the exact residue left by Determinate Nix Installer v3.22.1 without reading or parsing the Vendor Receipt? |
| **Vendor revision** | `4132ad07a15ee7d88c096ac7172b7afb2672866b` |
| **Targets** | Linux x86_64, Linux aarch64, and macOS arm64 |
| **Product revision reviewed** | `16a6a2169c6d7757e5c6a9a01b42d85cbfa06eda` |
| **Result** | **NO-GO for activation on all three targets today.** The deletion design is feasible. The retained runs do not yet prove the durable ownership record that the product needs. macOS also lacks a safe identity rule and proof for mutable logs. |

## 1. Short decision

The Vendor Uninstaller does not return each tested host to its clean baseline.

- Linux leaves `/etc/nix/sentry-endpoint` and its parent directory.
- macOS leaves six paths.
- The current product adapter also makes the Vendor helper leave a random
  executable in the product TMPDIR.
- The official source explains some paths.
- The closed `determinate-nixd` binary creates or manages other paths.
- Source intent is not deletion authority.
- A file that looks like a Vendor file can still belong to another actor.

`pkg` can avoid parsing the Vendor Receipt.

It must use a durable product record instead.

That record must prove all of these facts:

1. Each path that cleanup can delete was absent before the Vendor attempt.
   A pre-existing system path is preservation-only.
2. The accepted Handoff identifies the exact pinned Vendor binary and target.
3. The candidate appeared during that accepted attempt.
4. The current leaf is the same installed leaf where content can change.
5. The full current residue set is exact before the first deletion.

If one fact is missing, `pkg` must leave all residue in place.

### 1.1 Critical TMPDIR self-copy blocker

The current inactive adapter calls `/nix/nix-installer` for uninstall at
`crates/pkg-installer/src/determinate.rs:65-68`.

The adapter fixes TMPDIR to:

```text
Linux: /var/lib/pkg-install/tmp
macOS: /private/var/db/pkg-install/tmp
```

See `crates/pkg-installer/src/determinate.rs:34-37` and
`crates/pkg-installer/src/determinate.rs:187-203`.

The pinned Vendor source detects execution from `/nix/nix-installer`. It
copies itself to
`std::env::temp_dir()/nix-installer-<16-alnum>` and then calls `execv` on the
copy. See
[`src/cli/subcommand/uninstall.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/subcommand/uninstall.rs#L72-L110).
Rust `std::env::temp_dir()` uses the platform temporary-directory environment;
see the official
[`std::env::temp_dir` documentation](https://doc.rust-lang.org/std/env/fn.temp_dir.html).

The Vendor code does not remove this random copy.

A safe direct proof used the exact aarch64 Linux asset with SHA-256
`9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179`.
It mounted that asset read-only at `/nix/nix-installer` in a disposable
`--rm`, `--network none`, `--platform linux/arm64` container. It set
`TMPDIR=/proof`. It called uninstall while `/nix/receipt.json` was absent.

The essential proof command was:

```sh
docker run --rm --network none --platform linux/arm64 \
  --entrypoint /bin/sh \
  -v "$PINNED_ASSET:/nix/nix-installer:ro" \
  ubuntu:24.04 \
  -eu -c '
    install -d -m 0700 /proof
    set +e
    output=$(TMPDIR=/proof DETSYS_IDS_TELEMETRY=disabled \
      /nix/nix-installer \
      --diagnostic-endpoint http://127.0.0.1:18080 \
      uninstall --no-confirm /nix/receipt.json 2>&1)
    status=$?
    set -e
    printf "status=%s\n%s\n" "$status" "$output"
    find /proof -mindepth 1 -maxdepth 1 -print
    find /proof -mindepth 1 -maxdepth 1 -type f \
      -exec stat -c "type=%F uid=%u gid=%g mode=0%a size=%s links=%h path=%n" {} +
  '
```

The command returned status `1` because the receipt was absent. It still left
exactly one random copy:

```text
/proof/nix-installer-<16-alnum>
type=regular uid=0 gid=0 mode=0700 size=69625424 nlink=1
```

This proves the copy happens before receipt parsing.

The proof used the locally available `ubuntu:24.04` image tag. It did not use
the digest-qualified retained Asset-proof image. The exact Vendor executable
was the pinned aarch64 asset. This direct run proves only the self-copy order
and residue. It is not a new target or release-environment proof.

The earlier VM and container residue scanners did not inspect this product
TMPDIR. Their Linux and macOS manifests are exact only for the paths that the
harness scanned. They are not a complete residue contract for the current C06
adapter.

The retained runs also did not trace every filesystem write by the Vendor
process tree. The public source review adds the random TMPDIR copy to the known
set. It cannot prove that the closed `determinate-nixd` binary wrote no other
path outside the scanner scope. Thus, this report identifies every retained
path that current primary evidence proves. It does not claim a complete
whole-host path set.

There is no safe exact cleanup for an unknown random basename. DN-13 must not
scan and remove `nix-installer-*` names.

The smallest fix is to avoid this Vendor branch.

DN-13 must run uninstall through the already-authenticated persistent staged
Vendor executable outside `/nix`. It must still pass the fixed
`/nix/receipt.json` argument. The staged executable identity must match the
accepted Handoff and release metadata. Since its current executable path is
not `/nix/nix-installer`, the Vendor code does not make the random self-copy.

This is a cross-module NO-GO blocker for every target.

## 2. Evidence labels

- **Observed** means a primary source or retained runtime artifact contains the fact.
- **Source-derived** means the pinned official source contains the behavior.
- **Inference** means the conclusion follows from observed facts.
- **Unproved** means the current evidence does not establish the fact.
- **GO** means the evidence is sufficient for the stated narrow decision.
- **NO-GO** means activation must wait.

## 3. Primary sources and integrity

### 3.1 Official source

The official release is [Determinate Nix Installer v3.22.1](https://github.com/DeterminateSystems/nix-installer/releases/tag/v3.22.1).

All source links in this report use full revision
`4132ad07a15ee7d88c096ac7172b7afb2672866b`.

The local extracted source used for the review was:

```text
/private/tmp/nix-installer-4132ad07-source
```

The checked-in S6 parent report records the source tarball SHA-256 as
`e946ce0920e1ac0a76281d1d0d24b5ddb0fa1807f5317d1545130fe8a04ff084`.

The pinned build does not contain public Rust source for all
`determinate-nixd` behavior. It embeds a target-specific static binary through
`include_bytes!` in
[`src/distribution.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/distribution.rs#L47-L56).
The build selects target binaries in
[`flake.nix`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/flake.nix#L15-L24)
and
[`flake.nix`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/flake.nix#L37-L53).
The exact v3.22.1 target downloads are pinned in
[`flake.lock`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/flake.lock#L19-L78).

Thus, the public installer source cannot prove the exact internal writer for
`sentry-endpoint` or `macos-keychain.crt`.

### 3.2 Official documentation

The current official documentation gives the built-in uninstall command:

```sh
sudo /nix/nix-installer uninstall
```

It also uses `/nix/nix-installer` and `/nix/receipt.json` as indicators of a
Determinate installation. See the official
[migration guide](https://docs.determinate.systems/guides/migrating-from-upstream-nix)
and
[macOS installation troubleshooting guide](https://docs.determinate.systems/troubleshooting/installation-failed-macos).

The two official guides reviewed do not promise that this command removes
every path in this report. They do not publish a residue ownership contract.

### 3.3 Retained runtime evidence

The retained primary runtime artifacts are:

```text
/private/var/tmp/pkg-s6-dn03b-evidence/lifecycle-33b386d-r12
/private/var/tmp/pkg-s6-dn03b-evidence/probe-7ff31c5-r11
/private/var/tmp/pkg-s6-dn03b-aarch64-evidence/probe-16f0bbe-r10
/private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-aa5d5be-r10
```

The checked-in summaries are:

- [S6 findings](./FINDINGS.md)
- [Linux findings](./linux-vm/LINUX-FINDINGS.md)
- [macOS findings](./macos-vm/FSTAB-CONTRACT-RESEARCH.md)

I recomputed all nine macOS R10 phase archive hashes. Each value matched its
saved sidecar. I also recomputed the selected Linux residue artifact hashes.
On each Linux target, the final sentry identity matched that target lane's
saved post-install identity and private digest record.

## 4. The Vendor Receipt boundary

The official source fixes the receipt location at `/nix/receipt.json` in
[`src/plan.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/plan.rs#L15).

The Vendor executable reads and parses the receipt itself in
[`src/cli/subcommand/uninstall.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/subcommand/uninstall.rs#L114-L142).
It then reverts the saved actions in reverse order in
[`src/plan.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/plan.rs#L287-L333).

When the installed helper is invoked from `/nix`, it copies itself out of
`/nix` before it reads the receipt or deletes the store. See
[`src/cli/subcommand/uninstall.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/subcommand/uninstall.rs#L60-L110).

This gives a narrow product boundary:

- `pkg` validates the installed helper and the persistent staged executable as
  exact pinned executables.
- `pkg` validates the receipt as an opaque regular file.
- `pkg` calls the staged executable outside `/nix` with fixed arguments.
- The Vendor executable alone parses the receipt.
- `pkg` proves realized post-uninstall state.
- `pkg` then applies its own exact residue policy.

This boundary is sufficient. `pkg` does not need a receipt parser.

It is not sufficient to authorize residue deletion.

The Vendor Receipt is removed by a successful uninstall. The residue policy
therefore needs a separate durable product ownership record that survives
Vendor Uninstall.

## 5. Exact Linux residue

### 5.1 Linux x86_64

The retained x86_64 container used pinned image:

```text
ubuntu@sha256:1e0a86e57d247923571b75e0aaf48a1449cf8c543d51fb3e07a4a7d7bfa79316
```

This was an amd64 container lane on an arm64 host. It is not a bare-metal
x86_64 proof.

The installed helper matched pinned asset SHA-256:

```text
9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c
```

Vendor Uninstall returned `0`.

The exact final manifest within the retained Linux residue scanner scope was:

```text
type=directory uid=0 gid=0 mode=0755 size=4096 links=2 path=/etc/nix
type=regular file uid=0 gid=0 mode=0600 size=95 links=1 path=/etc/nix/sentry-endpoint
sha256=<private pinned-target digest>
```

The full `/etc/nix` child inventory contained only:

```text
/etc/nix/sentry-endpoint
```

Primary artifacts:

```text
/private/var/tmp/pkg-s6-dn03b-evidence/probe-7ff31c5-r11/etc-nix.stat
/private/var/tmp/pkg-s6-dn03b-evidence/probe-7ff31c5-r11/etc-nix.entries
/private/var/tmp/pkg-s6-dn03b-evidence/probe-7ff31c5-r11/sentry-after-install.stat
/private/var/tmp/pkg-s6-dn03b-evidence/probe-7ff31c5-r11/sentry-after-install.sha256
/private/var/tmp/pkg-s6-dn03b-evidence/probe-7ff31c5-r11/sentry-after-uninstall.stat
/private/var/tmp/pkg-s6-dn03b-evidence/probe-7ff31c5-r11/sentry-after-uninstall.sha256
```

The broad x86_64 R12 QEMU lane adds lifecycle evidence.

- The sentry path was absent before the first install.
- The same recorded metadata, size, and digest existed after install.
- It stayed equal after the pinned same-version daemon upgrade.
- It stayed equal after Vendor Uninstall.
- Vendor Uninstall returned `0`.
- Repeat uninstall refused the missing receipt.

Primary artifacts:

```text
/private/var/tmp/pkg-s6-dn03b-evidence/lifecycle-33b386d-r12/guest-evidence/sentry-before-initial.kind
/private/var/tmp/pkg-s6-dn03b-evidence/lifecycle-33b386d-r12/guest-evidence/sentry-after-initial.stat
/private/var/tmp/pkg-s6-dn03b-evidence/lifecycle-33b386d-r12/guest-evidence/sentry-after-initial.sha256
/private/var/tmp/pkg-s6-dn03b-evidence/lifecycle-33b386d-r12/guest-evidence/sentry-after-determinate-nixd-upgrade.stat
/private/var/tmp/pkg-s6-dn03b-evidence/lifecycle-33b386d-r12/guest-evidence/sentry-after-determinate-nixd-upgrade.sha256
/private/var/tmp/pkg-s6-dn03b-evidence/lifecycle-33b386d-r12/guest-evidence/sentry-after-uninstall.stat
/private/var/tmp/pkg-s6-dn03b-evidence/lifecycle-33b386d-r12/guest-evidence/sentry-after-uninstall.sha256
```

R12 did not record a full final `/etc/nix` inventory. It recorded only the
first entry. It also did not record the sentry hard-link count. The full
inventory and hard-link proof come from the retained container R11.

### 5.2 Linux aarch64

The retained aarch64 container used pinned image:

```text
ubuntu@sha256:33ceb71981b602c1a7443a53469e4dba065f7503eab3078a2d7a57a2ab987517
```

The installed helper matched pinned asset SHA-256:

```text
9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179
```

The harness refused any pre-existing `/nix`, `/etc/nix`, or
`/usr/local/bin/determinate-nixd` before install. See
[`inside-aarch64-container.sh`](./linux-vm/inside-aarch64-container.sh#L45-L52).

Vendor Uninstall returned `0`.

The exact final manifest within the retained Linux residue scanner scope was
the same as x86_64:

```text
type=directory uid=0 gid=0 mode=0755 size=4096 links=2 path=/etc/nix
type=regular file uid=0 gid=0 mode=0600 size=95 links=1 path=/etc/nix/sentry-endpoint
sha256=<private pinned-target digest>
```

The full `/etc/nix` child inventory contained only `sentry-endpoint`.

Primary artifacts:

```text
/private/var/tmp/pkg-s6-dn03b-aarch64-evidence/probe-16f0bbe-r10/etc-nix.stat
/private/var/tmp/pkg-s6-dn03b-aarch64-evidence/probe-16f0bbe-r10/etc-nix.entries
/private/var/tmp/pkg-s6-dn03b-aarch64-evidence/probe-16f0bbe-r10/sentry-after-install.stat
/private/var/tmp/pkg-s6-dn03b-aarch64-evidence/probe-16f0bbe-r10/sentry-after-install.sha256
/private/var/tmp/pkg-s6-dn03b-aarch64-evidence/probe-16f0bbe-r10/sentry-after-uninstall.stat
/private/var/tmp/pkg-s6-dn03b-aarch64-evidence/probe-16f0bbe-r10/sentry-after-uninstall.sha256
/private/var/tmp/pkg-s6-dn03b-aarch64-evidence/probe-16f0bbe-r10/evidence.sha256
```

This was a Linux arm64 container on an arm64 Docker server. It was not a
bare-metal or full systemd lifecycle run.

### 5.3 Linux stability decision

The sentry leaf had stable target-specific metadata, size, and private digest:

- after install and after uninstall;
- within each Linux target lane;
- across the broader x86_64 same-version daemon upgrade.

This proves stability only for the tested v3.22.1 runs.

It does not prove stability for another Vendor version, image, filesystem, or
starting state.

The exact writer is not in the public installer source. The retained clean
baseline and later appearance support attribution to Vendor execution.

## 6. Exact macOS arm64 residue

The retained R10 run used:

```text
base image: ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c
macOS: 15.7.7
build: 24G720
architecture: arm64
```

Vendor Uninstall returned `0`.

The exact final manifest within the retained macOS residue scanner scope was:

```text
path=/etc/nix type=directory mode=0755 uid=0 gid=0 size=128 nlink=4
path=/etc/nix/macos-keychain.crt type=regular mode=0644 uid=0 gid=0 size=241049 nlink=1 sha256=ea4be6e77db3daf79e5804947a9376da53765ee6a7dfe03299400cd81d7d6e6e
path=/etc/nix/sentry-endpoint type=regular mode=0644 uid=0 gid=0 size=95 nlink=1 sha256=d21f6d21fc5cbf0da38bc72b9a9de8a0c6c1bae72a3727884fd1b84e1a901fc3
path=/etc/fstab type=regular mode=0644 uid=0 gid=0 size=0 nlink=1 sha256=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
path=/var/log/determinate-nix-init.log type=regular mode=0644 uid=0 gid=0 size=1078 nlink=1 sha256=6ca2ae1e2558d3f8a9cbaaf6d4fc367be2637a876078e00e7bcd2efde3960580
path=/var/log/determinate-nix-daemon.log type=regular mode=0644 uid=0 gid=0 size=11200 nlink=1 sha256=1e399520f40810e7cb108711ad4aeff768b156407eb9de0eaa72765e1ec443e3
```

The complete manifest stayed byte-equal across these boundaries:

- uninstall-after to repeat-uninstall-before;
- repeat-uninstall-before to repeat-uninstall-after;
- repeat-uninstall-after to final-reboot-before;
- final-reboot-before to final observation.

The receipt was absent. The launchd, account, socket, and keychain-item probes
were empty. Product residue passed.

Primary archive:

```text
/private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-aa5d5be-r10/phases/lifecycle-residue.tar
```

The exact records are:

```text
lifecycle-residue/after.etc-nix.inventory
lifecycle-residue/after.fstab.identity
lifecycle-residue/after.determinate-nix-init-log.identity
lifecycle-residue/after.determinate-nix-daemon-log.identity
lifecycle-residue/vendor-residue
lifecycle-residue/vendor-outcome
```

### 6.1 Stable and mutable macOS paths

| Path | Tested behavior | Cleanup class |
|---|---|---|
| `/etc/nix/macos-keychain.crt` | Exact bytes stayed equal through R10. | Fixed-content leaf for this pinned run. |
| `/etc/nix/sentry-endpoint` | Exact bytes stayed equal through R10. | Fixed-content leaf for this pinned run. |
| `/etc/fstab` | Vendor install created a nonempty file from an absent baseline. Vendor uninstall left an empty file. | Baseline-relative system file. Never delete when it existed before install. |
| `/var/log/determinate-nix-init.log` | Content stayed equal at the final tested boundary. | Mutable by design. A digest must not be the ownership key. |
| `/var/log/determinate-nix-daemon.log` | Size and digest changed while the daemon was active. It was stable after uninstall. | Mutable by design. A fixed digest is invalid. |
| `/etc/nix` | Child set changed during uninstall. Final child set was exact. | Remove with `rmdir` only after exact leaf removal and empty-directory revalidation. |

The daemon log changed from size 10,509 and SHA-256
`dbbe0e2d0ab271249b2e4bc148b5c4e61ca0594a25292926a2653ee0243a901d`
to size 11,200 and SHA-256
`1e399520f40810e7cb108711ad4aeff768b156407eb9de0eaa72765e1ec443e3`
at the last active boundary.

The path, type, mode, uid, gid, and link count stayed equal.

R10 used device and inode internally to detect a change during each single
capture. It did not write device or inode into the retained identity record.
See
[`macos-vm/inside.sh`](./macos-vm/inside.sh#L33-L46)
and
[`macos-vm/inside.sh`](./macos-vm/inside.sh#L220-L238).

Thus, R10 cannot prove that the final log is the same inode that Vendor
execution created.

## 7. Creator proof by path

### 7.1 `/etc/fstab`

The pinned source reads a missing fstab as an empty string. It then writes one
new `/nix` line with an atomic replacement. See
[`create_fstab_entry.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_fstab_entry.rs#L60-L112).

During revert, it removes matching `/nix` lines and atomically writes the
result. It does not delete the file. See
[`create_fstab_entry.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_fstab_entry.rs#L126-L167).

This explains the empty R10 residue from an absent baseline.

It does not authorize deletion on every host.

The source uses `lines()` and reconstructs the file. This can normalize line
endings and final newlines. A hash-only pre-install record cannot restore a
pre-existing file.

Safe rule:

- If pre-install no-follow inspection proved `/etc/fstab` absent, final exact
  empty root-owned mode-`0644`, `nlink=1` state can be deleted.
- If `/etc/fstab` existed before install, never delete it.
- For a pre-existing file, verify the final content and metadata against the
  recorded baseline where possible. Leave the file even on equality.
- If the final state differs from the allowed baseline-relative state, stop.

### 7.2 macOS logs

The pinned source names the init log in the generated mount-service plist:

[`create_determinate_volume_service.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/macos/create_determinate_volume_service.rs#L186-L203).

It names the daemon log in the daemon plist:

[`configure_determinate_nixd_init_service/mod.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/configure_determinate_nixd_init_service/mod.rs#L207-L214).

Launchd creates or opens those output files. Their content is expected to
change.

Safe future rule:

1. Prove each path absent before Vendor install.
2. After accepted install, record device, inode, type, uid, gid, mode, and
   link count in durable product state.
3. Before cleanup, stop all writers.
4. Open the fixed root-owned parent without following links.
5. Open the leaf without following links.
6. Revalidate device, inode, type, uid, gid, mode, and `nlink=1`.
7. Unlink only that parent-bound leaf.
8. Any inode replacement or missing field must refuse all residue cleanup.

The content digest is useful for evidence. It is not a safe ownership key for
a mutable log.

R10 does not prove this rule because it did not retain device and inode.

### 7.3 Certificate and sentry leaf

The public installer source does not contain their writer.

The installer embeds the closed target `determinate-nixd` binary. The clean
R10 baseline had no `/etc/nix`. Both leaves appeared after Vendor execution.
The leaves stayed exact in the tested run.

Safe rule:

- require durable pre-install absence;
- record the exact post-install identity;
- require accepted pinned Handoff;
- require exact content, metadata, and `nlink=1` before cleanup;
- stop on any difference.

The pinned file digest alone is not creator proof.

### 7.4 Linux sentry leaf

The same rule applies on Linux.

The same path, type, owner, mode, size, and link count appeared in both target
containers. Each private target digest matched across its own lane. The broad
x86_64 run also proves absence before install and equality across install,
daemon upgrade, and uninstall.

The exact writer remains unproved by public source.

## 8. Required durable ownership record

The smallest safe record is not a second Vendor Receipt.

It does not contain Vendor actions or receipt bytes.

It contains only product cleanup authority:

```text
schema version
operation ID
pinned Vendor version
target triple
accepted Vendor binary digest
accepted Handoff identity
authenticated persistent staged-uninstaller fixed path ID and identity
for each fixed candidate:
  fixed path ID
  pre-install state: absent or present
  allowed post-install identity
for each mutable candidate:
  fixed path ID
  pre-install state: absent
  post-install device and inode
  type, uid, gid, mode, and link count
```

The record must exist outside `/nix`.

It must be written before the Vendor attempt and updated only after accepted
install proof.

It must use fixed paths and a closed schema.

Unknown fields, duplicate entries, arbitrary paths, missing entries, or a
target mismatch must refuse cleanup.

The record must survive Vendor Uninstall.

## 9. Safe deletion without recursive removal

The exact manifests are small. Recursive deletion is not needed.

A safe operation can use this order:

1. Acquire the product lifecycle lock.
2. Load and authenticate the durable ownership record.
3. Prove accepted Handoff for the same operation and target.
4. Run Vendor Uninstall through the authenticated persistent staged executable
   outside `/nix`. Never execute `/nix/nix-installer` for this operation.
5. Prove the Vendor receipt, helper, store, services, users, and sockets are in
   the expected absent state.
6. Prove all possible residue writers are absent or stopped.
7. Open and bind every component of each fixed root-owned parent chain without
   following links. Revalidate device, inode, owner, group, mode, and type.
8. Inventory the complete allowed residue set.
9. Reject symbolic links, other file types, cross-device entries, hard-linked
   regular files, unexpected children, and unexpected parents.
10. Validate every candidate before the first deletion.
11. Durably write and fsync `Deleting(<fixed-path-ID>)` before one unlink.
12. Revalidate that leaf immediately before unlink.
13. Unlink the exact leaf through its already-open parent.
14. Fsync the parent directory.
15. Durably advance the progress record.
16. Revalidate `/etc/nix` as the same empty directory.
17. Apply the same intent, `rmdir`, parent-fsync, and advance sequence to
    `/etc/nix`.
18. Prove the final clean state.
19. Remove the exact staged-uninstaller product asset only after terminal
    proof, if the product lifecycle owns its removal.

On restart, `Deleting(<fixed-path-ID>)` has two allowed states.

- If the exact leaf still exists, revalidate it and continue that unlink.
- If only that exact leaf is absent, accept the absence as the completed
  in-progress unlink, fsync its parent, and advance.

A missing future leaf or a missing leaf without the exact durable intent must
stop cleanup.

Do not use `remove_dir_all`, `rm -r`, a glob, or a scan-and-delete loop.

The Vendor source itself calls `remove_dir_all` for `/etc/nix` only after an
empty-directory check in
[`provision_determinate_nixd.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/provision_determinate_nixd.rs#L93-L117).
DN-13 must not copy that recursive operation.

The product operation can use exact leaf unlink plus exact empty `rmdir`.

The host root user can replace root-owned paths and race the cleanup. A hostile
root user is outside this product threat model. The design must still prevent
unprivileged path replacement and ordinary time-of-check/time-of-use mistakes.

## 10. Platform decisions

| Target | Residue policy design | Activation today | Reason |
|---|---|---|---|
| Linux x86_64 | **GO** for a narrow exact `/etc/nix` design. | **NO-GO** | Current adapter creates an untracked random TMPDIR copy. Broad R12 lacks a full inventory and hard-link count. Full R11 identity comes from a container. No durable product ownership record or product cleanup run exists. |
| Linux aarch64 | **GO** for the same narrow exact `/etc/nix` design. | **NO-GO** | Direct proof confirms the current adapter's random TMPDIR copy. Exact target `/etc/nix` residue exists only in a container proof. No full system lifecycle or product cleanup run exists. No durable product ownership record exists. |
| macOS arm64 | **NO-GO** for the complete final rule. | **NO-GO** | The same source self-copy branch applies to the installed helper. R10 proves one scanned six-path result, but mutable logs lack retained device and inode. The pre-existing fstab case was not tested. No durable product ownership record or product cleanup run exists. |

All three targets are **NO-GO today**.

This result does not reject the architecture. It defines the missing proof.

## 11. Required next evidence

Before any platform proof, run uninstall through the authenticated persistent
staged executable outside `/nix`.

Prove that:

- success creates no random executable in the fixed product TMPDIR;
- missing-receipt refusal creates no random executable;
- interruption creates no random executable;
- the staged executable stays available until terminal proof;
- the installed `/nix/nix-installer` identity is validated but never executed.

For each supported target, also trace filesystem mutations from the exact
Vendor process tree in a disposable clean guest. Pair that trace with bounded,
no-follow baseline, post-install, and post-uninstall inventories for every
observed path and every source-declared path. Include the fixed product TMPDIR.
An unexplained write or retained path is a stop condition. The trace is
evidence discovery only. It must never become authority to scan and delete an
unknown path.

### 11.1 Linux x86_64

Run a fresh full VM lifecycle with DN-13 cleanup.

The run must record:

- pre-install absence of `/etc/nix` and the sentry leaf;
- the durable ownership record before Vendor execution;
- post-install device, inode, type, uid, gid, mode, size, link count, and hash;
- the full post-uninstall no-follow inventory;
- exact leaf unlink;
- exact empty-directory `rmdir`;
- final absence;
- interruption before and after each durable cleanup step;
- two complete clean runs.

### 11.2 Linux aarch64

Run the same proof on the accepted release environment.

A container Asset proof is not a full activation proof.

### 11.3 macOS arm64

Run a fresh supported macOS lifecycle with these added lanes:

1. Clean baseline with all six candidate paths absent.
2. Pre-existing `/etc/fstab` with unrelated content.
3. Pre-existing empty `/etc/fstab`.
4. Each log records device and inode after install.
5. Each log proves the same device and inode after all services stop.
6. A replaced log inode causes refusal.
7. A changed certificate or sentry digest causes refusal.
8. An extra `/etc/nix` child causes refusal.
9. Cleanup removes no path after any preflight mismatch.
10. Interruption after each exact leaf deletion resumes safely.
11. Two complete clean runs pass.

## 12. Stop conditions

DN-13 cleanup must stop before its first deletion when any condition applies:

- uninstall would execute `/nix/nix-installer` and trigger the random TMPDIR
  self-copy;
- no authenticated persistent staged uninstaller is available outside `/nix`;
- no durable pre-attempt ownership record;
- Handoff is not accepted;
- Vendor version or target differs;
- receipt or helper identity is unknown before Vendor Uninstall;
- realized Vendor state is mixed after Vendor Uninstall;
- a fixed path was present before install and policy does not preserve it;
- a mutable path has no post-install device and inode;
- a current inode differs from the accepted post-install inode;
- a leaf is missing, unless durable progress is exactly
  `Deleting(<that-fixed-path-ID>)` after prior full validation;
- a leaf is a symlink, a directory, another type, or hard-linked;
- `/etc/nix` has an extra child;
- a parent is a symlink or is not the expected root-owned directory;
- a parent-chain component changes device or inode after it is bound;
- the target evidence has an unexplained Vendor write or retained path outside
  the fixed cleanup schema;
- a content-fixed leaf has a different size or digest;
- a cleanup step cannot be recorded durably;
- a complete cleanup proof has not passed twice on the target.

## 13. Commands used for this review

The review used the pinned source and retained evidence directly.

Representative commands were:

```sh
git rev-parse HEAD
git status --short
rg -n -S 'sentry-endpoint|macos-keychain|determinate-nix-init.log|determinate-nix-daemon.log|/etc/fstab|receipt.json|uninstall' /private/tmp/nix-installer-4132ad07-source
tar -tf /private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-aa5d5be-r10/phases/lifecycle-residue.tar
tar -xOf /private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-aa5d5be-r10/phases/lifecycle-residue.tar lifecycle-residue/after.etc-nix.inventory
tar -xOf /private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-aa5d5be-r10/phases/lifecycle-residue.tar lifecycle-residue/after.fstab.identity
tar -xOf /private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-aa5d5be-r10/phases/lifecycle-residue.tar lifecycle-residue/after.determinate-nix-init-log.identity
tar -xOf /private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-aa5d5be-r10/phases/lifecycle-residue.tar lifecycle-residue/after.determinate-nix-daemon-log.identity
shasum -a 256 /private/var/tmp/pkg-s6-dn03c-evidence/lifecycle-diagnostics-aa5d5be-r10/phases/*.tar
docker run --rm --network none --platform linux/arm64 --entrypoint /bin/sh -v "$PINNED_ASSET:/nix/nix-installer:ro" ubuntu:24.04 -eu -c '<create /proof mode 0700; run telemetry-disabled uninstall with TMPDIR=/proof; capture status, listing, and stat>'
```

The TraceDecay planning query reduced the initial repository scan from about
99,081 tokens to 1,799 tokens.

## 14. Final answer

The safest simple design is a small fixed manifest plus a durable product
ownership record.

Invoke the authenticated persistent staged Vendor executable for uninstall.

Do not invoke `/nix/nix-installer` through the current adapter.

This avoids the Vendor's random TMPDIR self-copy.

Do not parse the Vendor Receipt.

Do not infer ownership from source intent.

Do not use a fixed digest for mutable logs.

Do not delete a pre-existing `/etc/fstab`.

Do not use recursive deletion.

Linux has a clear exact cleanup rule, but both Linux targets remain NO-GO
until the product records ownership and proves cleanup on the target.

macOS remains NO-GO until a new run proves inode continuity for both logs and
proves the pre-existing fstab cases.
