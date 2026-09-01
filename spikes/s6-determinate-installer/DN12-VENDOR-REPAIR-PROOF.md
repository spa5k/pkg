# DN-12 vendor repair and update proof

| | |
|---|---|
| **Question** | Can `pkg` safely route Base Nix repair and update through the pinned Determinate tools? |
| **Pinned installer** | `nix-installer` v3.22.1, revision `4132ad07a15ee7d88c096ac7172b7afb2672866b` |
| **Update pair tested** | Determinate Nix v3.22.1 to v3.22.2 |
| **Runtime target tested here** | Native Linux aarch64 in disposable systemd containers |
| **Result** | **NO-GO for DN-12 product activation on all targets.** Do not add a Base Nix repair or update route. |

## 1. Short decision

The installer does not have a general Base Nix repair command.

- Default `nix-installer repair` repairs shell hooks.
- On macOS, default repair also repairs remote-building hooks.
- `repair sequoia` can move macOS build users and can replace the Vendor Receipt.
- No installer repair command repairs the Nix store, daemon, service, or general receipt damage.

`determinate-nixd upgrade --version <version>` owns a real update.

The Linux aarch64 proof established these facts:

1. A same-version v3.22.1 update is not a no-op.
2. It can install a different Nix store closure for the same displayed version.
3. It replaces `/usr/local/bin/determinate-nixd` even when the new bytes are identical.
4. It changes the root profile and daemon identity state.
5. A v3.22.1 to v3.22.2 update succeeds.
6. An explicit v3.22.2 to v3.22.1 downgrade also succeeds.
7. No rollback command appears in the retained `upgrade` help and grammar.
8. The exact tested CLI-process disconnect left the daemon, Nix profile, and store unchanged after 180 seconds.
9. This one timing point is not a vendor cancellation contract.
10. Three ordinary connected operations required systemd to send `SIGKILL` to the daemon after its 90-second stop timeout.
11. The current Handoff cannot describe the changed daemon or Nix profile. It stores only the installer and receipt identities.

The public update CLI accepts a version and a profile path. It does not accept a
product digest, a product manifest, or a pre-authenticated local daemon binary.
The product can check the result after the update. It cannot bind the downloaded
daemon to fixed product evidence before that daemon becomes active.

The smallest safe product decision is to report Base Nix repair and update as
unsupported. Keep Package Repair separate and active. A new product release can
change the pinned Base Nix version only after the vendor supplies a fixed-input
update interface or after every target has a reviewed, product-owned pre-execution
authentication design.

## 2. Evidence labels and boundary

- **Observed** means an official source, official artifact, or retained runtime result contains the fact.
- **Source-derived** means the exact pinned public source contains the behavior.
- **Inference** means the conclusion follows from observed facts.
- **Unproved** means this research does not establish the fact.
- **GO** means the evidence is sufficient for the narrow row.
- **NO-GO** means product activation must wait.

This report uses primary sources only.

- Official Determinate documentation was resolved through Context7 first.
- Public source claims use the exact v3.22.1 installer revision.
- Closed `determinate-nixd` claims use exact official binaries and runtime observations.
- No vendor mutation ran on the host.
- No host `/nix` path was used.
- Both disposable containers and the derived proof image were removed after the run.
- No new macOS mutation ran. This report makes no new macOS runtime claim.

The retained proof set is small and reviewable:

- [safe Linux aarch64 replay harness](./run-dn12-linux-aarch64-proof.sh);
- [normalized runtime evidence](./DN12-VENDOR-REPAIR-EVIDENCE.txt); and
- [cleanup result](./DN12-VENDOR-REPAIR-CLEANUP.txt).

The evidence contains fixed-state hashes, metadata, profile targets, store
inventory changes, selected systemd stop lines, and the disconnect result. It
does not contain binaries, receipt contents, secrets, or full logs.

The closed `determinate-nixd` source is not available in the pinned installer
repository. Binary strings and symbols were used only to find candidate behavior.
They are not treated as a complete contract.

## 3. Exact sources and artifacts

### 3.1 Pinned installer source

The public installer source is v3.22.1 at full revision:

```text
4132ad07a15ee7d88c096ac7172b7afb2672866b
```

Primary links:

- [release](https://github.com/DeterminateSystems/nix-installer/releases/tag/v3.22.1)
- [repair command](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/subcommand/repair.rs)
- [receipt writer](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/plan.rs#L351-L380)
- [Determinate daemon provisioning](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/provision_determinate_nixd.rs)
- [Linux systemd service and sockets](https://github.com/DeterminateSystems/nix-installer/tree/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/configure_determinate_nixd_init_service)

The installer lock fixes Determinate input v3.22.1 and these daemon URLs:

```text
https://install.determinate.systems/determinate-nixd/tag/v3.22.1/aarch64-linux
https://install.determinate.systems/determinate-nixd/tag/v3.22.1/x86_64-linux
https://install.determinate.systems/determinate-nixd/tag/v3.22.1/macOS
```

The official bytes downloaded during this research were:

| Target | Size | SHA-256 |
|---|---:|---|
| Linux aarch64 | 30,358,544 | `a808c0cb3a6216ba167c873c8866806114253bcaa90a5cd52eef4b384c27febc` |
| Linux x86_64 | 30,637,144 | `685f6fe67474a59506e4b5f86342d0bd05a3b651d699667b12acb1b85e7e41fe` |
| macOS aarch64 | 27,193,680 | `73b0d0de73683eb3a435f97d1b5319cd98f17bc4fc5980c925a44bfbe53e08a4` |

The macOS aarch64 pin is recorded by the existing S6 research. The Linux
aarch64 and Linux x86_64 pins are retained by this DN-12 evidence set.

### 3.2 Official update documentation

The official [Determinate Nixd documentation](https://docs.determinate.systems/determinate-nix/determinate-nixd/)
documents these commands:

```text
sudo determinate-nixd upgrade
sudo determinate-nixd upgrade --version v3.6.2
determinate-nixd version
```

The page does not document rollback, downgrade, interruption recovery, atomicity,
or a complete changed-file set.

The exact v3.22.1 Linux binaries expose this update interface:

```text
Usage: determinate-nixd upgrade [OPTIONS]

Options:
  --profile <PROFILE>  [default: /nix/var/nix/profiles/default]
  --version <VERSION>  [default: stable]
```

No digest, signature, manifest, local payload, rollback, or cancellation option
is present.

### 3.3 The N+1 official target

On 2026-08-25, `determinate-nixd version` reported v3.22.2 as the latest target.

The official [immutable FlakeHub source](https://api.flakehub.com/f/pinned/DeterminateSystems/determinate/3.22.2/01a025df-f652-7701-a0cd-4f1373154572/source.tar.gz)
resolved to:

```text
version: 3.22.2
immutable source id: 01a025df-f652-7701-a0cd-4f1373154572
revision: b484316129e0089e28077f4ede85ac4dbd4b842f
source tarball SHA-256: 6b763a72234aedecd88cf4cd13926e1455768a4e30870f75ef17491f9d5d858d
```

Its official `flake.lock` pins these daemon NAR hashes:

| Target | NAR hash |
|---|---|
| Linux aarch64 | `sha256-zr+VnNMCbERuf0mUOjJHBft/BwZdcjk4rKA7pXwRbSY=` |
| Linux x86_64 | `sha256-Rp0y7m4SQecV4R8wFjNLRIXcC73I4r4js+kAdAkoAbk=` |
| macOS aarch64 | `sha256-76KjZDujSBvk+weVMPngpcbbERAdCozOXni6LII6M/g=` |

The direct official v3.22.2 daemon bytes were:

| Target | Size | SHA-256 |
|---|---:|---|
| Linux aarch64 | 30,363,256 | `39876af59651a7c3ec3037c8ef796f3bbbe4855d418b9ef5f98202244620428f` |
| Linux x86_64 | 30,617,616 | `a15a3ddcb96886429c5cf2b35118c9d9747bacb064e9c6987e0ec9f24d488eaa` |
| macOS aarch64 | 27,182,400 | `102d7880a57af28289cbd43f46c1e8dac939cbbe7110ee11104e5f0f2b321eee` |

The installed Linux aarch64 daemon after the N+1 update matched the direct
official v3.22.2 bytes exactly.

## 4. Repair command proof

### 4.1 Complete public repair grammar

The pinned installer has exactly two repair kinds.

| Command | Platform | Source-defined work | General Base Nix repair? |
|---|---|---|---|
| `nix-installer repair` or `repair hooks` | Linux | Re-plan and execute shell-profile configuration. | No |
| `nix-installer repair` or `repair hooks` | macOS | Re-plan shell-profile and remote-building hook configuration. | No |
| `nix-installer repair sequoia` | macOS only | Move or recreate Nix build users in the Sequoia-compatible UID range. It can update the users-and-groups action in the receipt. | No |

The source has no repair variant for:

- the Nix store;
- the Nix daemon executable;
- systemd or launchd service definitions;
- the daemon socket;
- a damaged general Vendor Receipt;
- `/etc/nix/nix.conf`;
- `/etc/nix/nix.custom.conf`; or
- a missing Determinate Nix closure.

### 4.2 Default hooks are not an installed-state repair

Default repair ignores the install receipt's `--no-modify-profile` decision.
It plans the default shell-profile locations again.

Thus, the command can change shell exposure even when product install used
`--no-modify-profile`. This work is not required for `pkg`, because product
operations use absolute paths and do not depend on a login shell finding Nix.

Activating this route would add mutation without repairing the Base Nix state
that DN-12 needs to classify. The route should not exist in the product.

### 4.3 Sequoia repair changes receipt ownership

The macOS-only Sequoia branch:

1. reads the `_nixbld` group and users with `dscl`;
2. can move existing users to temporary UIDs;
3. creates the target users;
4. updates the users-and-groups receipt action when it can parse it;
5. copies the old receipt to
   `/nix/receipt.pre-repair.<timestamp-millis>.json`; and
6. writes a replacement `/nix/receipt.json` through a temporary file and rename.

If the source cannot parse the receipt, it can still change the users and leave
the receipt unchanged. The command warns about that state.

This report did not run the command in an Apple Silicon VM. It is **NO-GO**.

## 5. Disposable Linux aarch64 environment

The mutation proof used two disposable containers. One prepared the temporary
systemd image. One ran the isolated install and update sequence.

| Item | Value |
|---|---|
| Docker client and server | 29.4.1, from prior retained research |
| Docker server | Linux arm64 |
| Base image | `ubuntu@sha256:33ceb71981b602c1a7443a53469e4dba065f7503eab3078a2d7a57a2ab987517` |
| Kernel | LinuxKit 6.12.76, aarch64, from prior retained research |
| systemd | 255.4-1ubuntu8.17 |
| Container isolation | private cgroup namespace; temporary `/run`; removed after proof |
| Installer | exact pinned aarch64 v3.22.1 asset |
| Installer SHA-256 | `9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179` |

The Docker version and LinuxKit kernel values come from prior retained
research. They are environment context, not output retained by this DN-12
replay. The DN-12 evidence retains the target architecture, base image digest,
systemd package version, isolation settings, and installer digest.

The temporary image added systemd to the exact Ubuntu image. It existed only
for this proof. The proof container used the vendor service and socket
definitions. It did not mount host `/nix` or any host product state. Cleanup
used only the exact captured Docker object IDs. Per-run names were used for
collision isolation. A name never authorized cleanup.

The retained artifact hashes are:

| Artifact | SHA-256 |
|---|---|
| replay harness | `0f6f9bf622a69369493b460971d727fb205899bbdcf2b605a395689c977d245e` |
| normalized evidence | `52342cf26108c1c6b784be2f592a07558d1d3780ae55e6f4fada448ea0ae7983` |
| cleanup result | `5465ad32b811a93a855b25e1fb222512baf6839d9473ea776d7ba3ef2cd68c0a` |

The install command was:

```text
DETSYS_IDS_TELEMETRY=disabled \
  /proof/nix-installer \
  --diagnostic-endpoint http://127.0.0.1:18080 \
  install linux --determinate --no-confirm --no-modify-profile
```

The installed result reported:

```text
Determinate Nixd daemon version: 3.22.1
Determinate Nixd client version: 3.22.1
nix (Determinate Nix 3.22.1) 2.35.2
```

Before and after each update, the proof recorded:

- daemon, installed installer, and receipt metadata and SHA-256;
- Nix configuration metadata and SHA-256;
- root profile links and generations;
- `/nix/var/determinate` metadata and fixed-file hashes;
- systemd unit metadata and hashes; and
- the complete top-level Nix store path inventory.

The scan is exact for those named trees. It is not a whole-filesystem write trace.

## 6. Same-version update: v3.22.1 to v3.22.1

The command was:

```text
/usr/local/bin/determinate-nixd upgrade --version v3.22.1
```

The command selected this Nix path:

```text
/nix/store/chjfxsa4hl5id4yjklq359dww58y5pya-determinate-nix-3.22.1/bin/nix
```

This differed from the initially installed v3.22.1 path:

```text
/nix/store/7yxbjpbi73kl5qyxwm176sffyd2m3qyz-determinate-nix-3.22.1/bin/nix
```

Therefore, the version label does not fix one Nix closure.

### 6.1 Observed mutations

| State | Before | After |
|---|---|---|
| `/usr/local/bin/determinate-nixd` bytes | v3.22.1 SHA-256 `a808c0...febc` | Same SHA-256, but new inode and mtime |
| Root profile | generation 1 | generation 2 |
| Active Nix closure | installer-embedded v3.22.1 closure | different downloaded v3.22.1 closure |
| Nix store | original closure | original closure plus a second v3.22.1 closure and dependencies |
| `/nix/var/determinate/identity.json` | one SHA-256 | different SHA-256 |
| `/nix/var/determinate/netrc` | present | same bytes; metadata refreshed by daemon lifecycle |
| `/etc/nix/sentry-endpoint` | one mtime | same bytes; later mtime |
| daemon service | running v3.22.1 | restarted v3.22.1 |

### 6.2 Observed stable bytes

These bytes did not change in the named scan:

- `/nix/nix-installer`;
- `/nix/receipt.json`;
- `/etc/nix/nix.conf`;
- `/etc/nix/nix.custom.conf`;
- `nix-daemon.service`;
- `nix-daemon.socket`; and
- `determinate-nixd.socket`.

The result proves only this Linux aarch64 run. It is not a cross-platform promise.

## 7. N-to-N+1 update: v3.22.1 to v3.22.2

The command was:

```text
/usr/local/bin/determinate-nixd upgrade --version v3.22.2
```

The command selected:

```text
/nix/store/fdivm6r29dnvf74ws0fp0vfzc0p7a55d-determinate-nix-3.22.2/bin/nix
```

It completed with these visible versions:

```text
Determinate Nixd daemon version: 3.22.2
Determinate Nixd client version: 3.22.2
nix (Determinate Nix 3.22.2) 2.35.2
```

### 7.1 Observed mutations

| State | v3.22.1 | v3.22.2 |
|---|---|---|
| daemon size | 30,358,544 | 30,363,256 |
| daemon SHA-256 | `a808c0cb3a6216ba167c873c8866806114253bcaa90a5cd52eef4b384c27febc` | `39876af59651a7c3ec3037c8ef796f3bbbe4855d418b9ef5f98202244620428f` |
| Root profile | generation 2 | generation 3 |
| Active Nix | v3.22.1 | v3.22.2 |
| Nix store | v3.22.1 closures retained | v3.22.1 closures plus v3.22.2 closure |
| `identity.json` | one SHA-256 | different SHA-256 |
| `sentry-endpoint` | one mtime | same bytes; later mtime |

The installer, receipt, Nix configuration, and systemd unit bytes again stayed
unchanged in this named scan.

## 8. Downgrade and rollback behavior

The public command is named `upgrade`. It still accepted an older explicit
version:

```text
/usr/local/bin/determinate-nixd upgrade --version v3.22.1
```

From v3.22.2, this command returned status 0. It:

- replaced the v3.22.2 daemon with the exact v3.22.1 daemon bytes;
- moved Nix back to the downloaded v3.22.1 closure; and
- created root profile generation 4.

The older profile generations remained present. This is not a vendor rollback
contract. The daemon binary is outside the Nix profile and is replaced at the
same path. Rolling back only the Nix profile can produce a daemon and Nix
mismatch.

No rollback command appears in the retained v3.22.1 `upgrade` help and grammar.
Official documentation does not describe rollback. Automatic rollback after
partial failure is unproved.

**Product downgrade policy:** NO-GO. A version argument can downgrade, but the
product has no atomic pair rollback and no complete recovery proof.

## 9. Failure and interruption behavior

### 9.1 Missing target fails before daemon, profile, or store mutation

The proof ran:

```text
/usr/local/bin/determinate-nixd upgrade --version v0.0.0-pkg-proof
```

It returned status 1 with HTTP status 404 for the normalized target
`v0.0.0-pkg-proof`.

Before and after comparisons were equal for:

- daemon, installer, receipt, and Nix configuration hashes;
- active root profile target; and
- root profile generation inventory.

The identity files kept the same bytes. Their metadata did not stay fixed.
`identity.json` and `netrc` received new inodes and later mtimes.

This proves one early download failure only. It does not prove failure after the
daemon has been replaced or after a new Nix closure has been realized.

### 9.2 Connected operations required SIGKILL

The vendor-installed Linux service has:

```text
KillMode=process
TimeoutStopUSec=1min 30s
SendSIGKILL=yes
```

For the ordinary connected same-version update, N+1 update, and downgrade,
systemd logged this sequence:

```text
State 'stop-sigterm' timed out. Killing.
Killing process ... with signal SIGKILL.
```

The client commands still completed successfully after the service restart.

This occurred three times in the native Linux aarch64 container. It can be an
interaction between the update RPC and service shutdown. It is still the exact
vendor unit and binary behavior in this environment.

DN-12 requires recovery without `SIGKILL`. This result is a direct stop gate.

### 9.3 The exact CLI-process disconnect did not complete the target update

The proof started v3.22.1 to v3.22.2 again. After the CLI printed
`Upgrading Determinate Nixd...`, the harness read the exact in-container CLI
process ID and sent `SIGTERM` to that process only.

Observed results:

- the CLI process exited with status 143;
- the harness reconciled the named state for 180 seconds;
- the daemon stayed at the pinned v3.22.1 hash;
- the root profile stayed at generation 4 and v3.22.1;
- the top-level store inventory had no added or removed path;
- the receipt, configuration, and unit hashes and metadata stayed fixed;
- `identity.json` kept the same bytes but received a new inode and later mtime;
- `netrc` kept the same bytes and inode but received a later mtime; and
- `nix-daemon.service` was active at the final snapshot.

This one observation shows no target update after this exact disconnect timing.
It still shows a durable metadata write. It does not prove a vendor cancellation
contract. It does not cover a disconnect after daemon replacement or after
profile mutation begins.

The product must still mark any disconnected update as ambiguous and reclassify
the machine. Client status 143 does not prove rollback or success.

### 9.4 Unproved interruption cases

This report does not prove:

- host power loss during daemon replacement;
- reboot during Nix closure realization;
- disk-full behavior after daemon replacement;
- loss of network after daemon replacement;
- rollback after a partially changed root profile;
- repair of a daemon and Nix version mismatch; or
- repeatability outside this one Linux aarch64 environment.

These cases are not needed to reach NO-GO. The observed connected-operation
`SIGKILL` behavior and the missing interruption contract are already blocking.

## 10. Can the update be authenticated against product evidence?

### 10.1 What can be fixed by the product

A future product release can pin:

- one explicit target version;
- the immutable official FlakeHub source revision;
- the target daemon size and SHA-256;
- the target daemon NAR hash;
- the expected `nix --version` result; and
- the expected target architecture.

After update, the product can verify that the installed daemon bytes match that
fixed target. This proof did that for Linux aarch64 v3.22.2.

### 10.2 What the vendor CLI cannot bind before execution

The update CLI accepts only a version and profile path. It does not accept:

- the product's daemon digest;
- the product's immutable FlakeHub source id;
- a local authenticated daemon executable;
- a product-supplied update manifest; or
- an expected Nix closure path.

The daemon download is made by the privileged vendor process. The downloaded
daemon becomes active before `pkg` can post-verify it.

The Nix closure is also not fixed by version text alone. The same-version proof
installed a different v3.22.1 closure than the installer embedded.

The official FlakeHub lock provides strong evidence that the product could pin.
The public `determinate-nixd upgrade` command has no input that binds execution
to that evidence.

**Conclusion:** post-execution authentication is possible. Pre-execution binding
to the accepted Handoff and fixed product evidence is not possible through the
public CLI. This is NO-GO for the requested trust boundary.

## 11. Exact Handoff impact

The current v1 Handoff `Accepted` record stores only:

- `/nix/nix-installer` length and SHA-256; and
- `/nix/receipt.json` length and SHA-256.

The Linux updates did not change either identity. Therefore,
`replace_after_installed_state_proof()` would write the same two identities and
would not record that the daemon, Nix closure, root profile, or daemon state had
changed.

If update is designed again later, a replacement acceptance must not occur until
all of these facts pass one fresh installed-state proof:

1. The operation target was one exact product-pinned version and architecture.
2. `/usr/local/bin/determinate-nixd` is a no-follow root-owned regular file with exact expected mode, size, and SHA-256.
3. The daemon client and server both report the expected target version.
4. The fixed Nix executable reports the expected Determinate and upstream Nix versions.
5. The default and root profile links form the expected topology and point to the observed target closure.
6. The daemon and both sockets are active and functional.
7. The standard-daemon DN-09 package parity probe passes again.
8. The installer and opaque receipt identities still match the prior accepted identities.
9. The Nix configuration and vendor service files still match their accepted ownership policy.
10. No package state, Generation, Activation Forest, package root, or product service changed.

An interrupted or disconnected update must stay in a durable ambiguous state.
It can become accepted only after the same proof passes. It must never infer
success from the client exit status.

The current two-file Handoff is insufficient for this transition. Do not extend
it now, because the update route itself is NO-GO.

### Repair-specific Handoff rules

- Default hook repair should not run. It adds no required Base Nix recovery.
- A future Sequoia repair would require receipt re-fingerprinting and exact ownership of its timestamped backup residue.
- If Sequoia changes users but cannot update the receipt, acceptance must fail.
- Any future repair must rerun DN-09 installed-state proof before Handoff replacement.

## 12. Platform GO/NO-GO matrix

| Capability | Linux x86_64 | Linux aarch64 | macOS aarch64 |
|---|---|---|---|
| General daemon/store/receipt repair | **NO-GO.** No vendor command exists. | **NO-GO.** No vendor command exists. | **NO-GO.** No general command exists. |
| Default hook repair behavior | Source plus retained x86_64 R12 status 0. It does not repair Base Nix. **NO-GO for product route.** | Source plus retained narrow runtime proof. It does not repair Base Nix. **NO-GO for product route.** | Retained lifecycle status 0 only. Exact hook mutation is not a DN-12 repair contract. **NO-GO.** |
| `repair sequoia` | Not available. | Not available. | Source-defined only. No new disposable Apple Silicon VM proof. **NO-GO.** |
| Same-version daemon update | Retained R12 status 0, but no complete state diff, authentication proof, or interruption proof. **NO-GO.** | Detailed mutation proof completed. SIGKILL and trust boundary fail. **NO-GO.** | No runtime proof. **NO-GO.** |
| N-to-N+1 update | No runtime proof. **NO-GO.** | v3.22.1 to v3.22.2 completed. SIGKILL, interruption contract, and pre-authentication fail. **NO-GO.** | No runtime proof. **NO-GO.** |
| Downgrade | No runtime proof. **NO-GO.** | Explicit v3.22.2 to v3.22.1 completed, but no atomic rollback. **NO-GO.** | No runtime proof. **NO-GO.** |
| Handoff refresh | Current record cannot describe update. **NO-GO.** | Current record cannot describe observed update. **NO-GO.** | Current record cannot describe an unproved update. **NO-GO.** |

No behavior is inferred from Linux aarch64 to Linux x86_64 or macOS.

## 13. Smallest safe product recommendation

Do not implement a DN-12 Base Nix mutation route.

Implement only an inactive, closed classification result if the grouped PR needs
one:

```text
Base Nix repair: unsupported
Base Nix update: unsupported
Package repair: use the existing product route
```

Do not add:

- a new dependency;
- a generic updater interface;
- progress parsing;
- a systemd-specific cancellation workaround;
- a second vendor journal;
- a copied vendor updater; or
- a larger Handoff schema for a route that cannot activate.

Re-open update design only when all stop conditions below are cleared.

## 14. Stop conditions

DN-12 activation must stop if any item is true:

1. The vendor command can run code that is not bound to fixed product evidence before execution.
2. Client termination behavior is not documented and proved for each mutation phase.
3. An ordinary update needs SIGKILL on any supported target.
4. The complete changed-file set is not proved on that target.
5. The Handoff cannot atomically represent the new accepted installed state.
6. A failure can leave a daemon and Nix profile version mismatch without proved recovery.
7. The target downgrade policy is not explicit and tested.
8. Package state or product-owned assets can change.
9. The update is not proved twice in the exact DN-15 or DN-16 target environment.
10. macOS behavior is claimed without a disposable Apple Silicon VM.

All ten conditions remain active for at least one required target. Product
delivery remains NO-GO.

## 15. Reproducible command summary

Documentation discovery:

```text
Context7 resolve: Determinate Nix Installer
Context7 library: /websites/determinate_systems
Context7 query: repair, uninstall, determinate-nixd upgrade, rollback,
downgrade, interruption recovery, executable authentication, receipt behavior
```

Pinned artifact checks:

```text
curl --proto '=https' --tlsv1.2 -fsSL \
  https://install.determinate.systems/determinate-nixd/tag/v3.22.1/aarch64-linux
sha256: a808c0cb3a6216ba167c873c8866806114253bcaa90a5cd52eef4b384c27febc

curl --proto '=https' --tlsv1.2 -fsSL \
  https://install.determinate.systems/determinate-nixd/tag/v3.22.2/aarch64-linux
sha256: 39876af59651a7c3ec3037c8ef796f3bbbe4855d418b9ef5f98202244620428f
```

Runtime commands, in order:

```text
/proof/nix-installer --diagnostic-endpoint http://127.0.0.1:18080 \
  install linux --determinate --no-confirm --no-modify-profile

/usr/local/bin/determinate-nixd upgrade --version v3.22.1
/usr/local/bin/determinate-nixd upgrade --version v3.22.2
/usr/local/bin/determinate-nixd upgrade --version v3.22.1
/usr/local/bin/determinate-nixd upgrade --version v0.0.0-pkg-proof
```

Safe retained replay command:

```text
spikes/s6-determinate-installer/run-dn12-linux-aarch64-proof.sh \
  --disposable-linux-aarch64-container \
  /absolute/path/to/pinned/nix-installer-aarch64-linux \
  /absolute/path/to/DN12-VENDOR-REPAIR-EVIDENCE.txt \
  /absolute/path/to/DN12-VENDOR-REPAIR-CLEANUP.txt
```

The harness refuses a run with no explicit disposable-container lane. It pins
the Ubuntu image, installer, daemon downloads, and systemd package. It creates
no host mount and does not use host `/nix`. It also refuses to overwrite output.

Client-disconnect proof:

```text
start: /usr/local/bin/determinate-nixd upgrade --version v3.22.2
observe: Upgrading Determinate Nixd...
send: SIGTERM to the CLI process only
client result: 143
reconciliation: 180 seconds
daemon result: stayed at v3.22.1; service active
identity result: identity.json was replaced; netrc mtime advanced; bytes stayed fixed
```

The research compared fixed-state snapshots after every completed phase and
retained only normalized evidence. The report and retained artifacts were
reviewed twice before the amended commit.
