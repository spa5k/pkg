# DN-03b research: `/etc/nix/sentry-endpoint`

## Question

Which vendor component creates `/etc/nix/sentry-endpoint`?

Does the install receipt own the file?

Why does uninstall keep the file?

Does `--diagnostic-endpoint ''` stop the file from being created?

## Fixed inputs

This research used only these inputs:

- The Determinate Nix Installer source at commit
  `4132ad07a15ee7d88c096ac7172b7afb2672866b`.
- The accepted private Linux lifecycle evidence at
  `/var/tmp/pkg-s6-dn03b-evidence/lifecycle-0d4809e`.

The receipt payload was not read, copied, searched, or quoted.

The sentry bytes and endpoint address stay private.

## Short answer

The file is vendor-owned residue.

The evidence proves that it is absent before vendor install.
It is present after vendor install.
It stays unchanged after the pinned daemon upgrade and vendor uninstall.
[E1][E2][E3]

The public installer source does not contain the path
`/etc/nix/sentry-endpoint`.
A search of the full fixed commit returned no literal match.[S0]
No dedicated sentry action is visible in the cited plan, receipt, and action
paths.[S3][S4][S6][S10][S11]
This is literal-search evidence only.
It is not structural proof that generated, constructed, or embedded code lacks
this behavior.

The exact internal writer is therefore **uncertain**.
The visible vendor execution boundaries are the diagnostics client, an
embedded `determinate-nixd` executable, and embedded Determinate Nix commands.
[S1][S2][S10][S11]
The allowed source does not expose enough implementation code to choose one.

The most precise public source location is
`PlaceNixConfiguration::execute`.
It runs `determinate-nixd init --stop-after nix-configuration` during the
recorded `ConfigureNix` action.[S10]
This is an exact source-side trigger.
The file-writing code remains inside the prebuilt executable and is not in the
allowed source.[S2]

The receipt does not directly describe this file.
It serializes the plan version, planner, and recorded actions.
None of the visible Linux actions stores the sentry path or its identity.
[S3][S4]

Uninstall has no direct sentry removal step.
It reverts the recorded actions in reverse order.
The daemon action removes `determinate-nixd` and `nix.conf`.
It removes `/etc/nix` only when the directory is empty.[S5][S6]
The sentry file keeps that directory non-empty.[E3]

The source says that an empty diagnostic endpoint disables diagnostic
reporting.[S7]
The same source still constructs the diagnostics provider and passes the empty
value to the diagnostics-client builder.[S1]
The source does not connect that option to the sentry file.
So source alone does **not** prove that `--diagnostic-endpoint ''` suppresses
creation of `/etc/nix/sentry-endpoint`.

## Evidence findings

### Lifecycle identity

| Stage | Private metadata result | Source |
|---|---|---|
| Before initial install | Absent | [E1] |
| After initial install | Regular file, `root:root`, mode `0600`, 95 bytes | [E2] |
| After `determinate-nixd upgrade --version v3.22.1` | Same type, owner, mode, size, and SHA-256 | [E2] |
| After vendor uninstall | Same type, owner, mode, size, and SHA-256 | [E3] |

The install, daemon-upgrade, and uninstall commands each returned status 0.
[E2][E3]

The three private captures are byte-identical.
This report does not publish their bytes, address, or hash.[E2][E3]

### Bytes and format

The fixed public source does not expose the sentry bytes or their format.[S0]

The accepted evidence proves only these safe properties:

- The object is a regular file.[E2]
- Its size is 95 bytes.[E2]
- Its private bytes did not change across the daemon upgrade or uninstall.
  [E2][E3]

No stronger format claim is supported by the allowed inputs.

### Owner and mode intent

The accepted runtime metadata proves `root:root` ownership and mode `0600`.
[E2]

The fixed public source has no sentry file action or sentry path.[S0]
It therefore states no intended owner or mode for this file.
Treat `root:root` and `0600` as observed vendor behavior, not as a documented
source contract.

## Source findings

### Diagnostics boundary

Diagnostics are a default feature.
That feature uses `detsys-ids-client` 0.7.0.[S1]

When the diagnostics feature is enabled, `main` calls `diagnostics(...)` after
command-line parsing.
The feature is enabled by default.[S1]
`DiagnosticData::new` passes the optional endpoint to
`detsys_ids_client::Builder::endpoint`.
It then calls `build_or_default`.[S1]

The `detsys-ids-client` implementation is not part of the fixed installer
source tree.
The lock file identifies the package version and checksum only.[S1]
So this source tree cannot prove whether that dependency creates the sentry
file.

### Embedded daemon boundary

The installer build downloads a fixed `determinate-nixd` executable and embeds
its bytes in the installer.[S2]

The public installer source writes those bytes to
`/usr/local/bin/determinate-nixd`.
During the `ConfigureNix` action, it directly runs
`determinate-nixd init --stop-after nix-configuration`.[S10]
It later configures the systemd service to run
`/usr/local/bin/determinate-nixd daemon`.[S2][S4]

The embedded executable implementation is not present in this source tree.
The lock file identifies the fixed x86_64 Linux download by URL and NAR hash.
[S2]
So this source tree cannot prove whether `determinate-nixd` creates the sentry
file.

### Embedded Determinate Nix boundary

The fixed build also embeds the Determinate Nix tarball.[S2]
The Linux install moves that tarball into `/nix` through the `ProvisionNix`
action.[S4][S11]

The `SetupDefaultProfile` action then directly runs the embedded
`nix-store --load-db` command and profile operations.[S11]
Those Determinate Nix command implementations are not present in the fixed
installer source tree.[S2]
So this source tree also cannot rule those commands in or out as the writer.

### Receipt and action ownership

`InstallPlan` serializes only these direct fields:

- `version`
- `actions`
- `planner`

[S3]

The Linux plan records daemon provisioning and daemon-service configuration as
actions.[S4]
It also records `ConfigureNix`, which contains the configuration-init and
default-profile command boundaries.[S10][S11]
The daemon-provisioning action stores only the daemon binary location.[S2]
The service action stores init and service configuration.[S4]

There is no direct sentry path, file identity, expected mode, or expected hash
in these visible action structures.[S0][S2][S4]

Therefore the receipt owns the vendor plan only at an indirect action level.
The public source does not show direct receipt ownership of the sentry file.
The private receipt was intentionally not opened to test its serialized form.

### Uninstall behavior

Uninstall reads the receipt as an `InstallPlan` and calls
`InstallPlan::uninstall`.[S5]
That method reverts recorded actions in reverse order.[S5]

The daemon-provisioning revert removes:

- `/usr/local/bin/determinate-nixd`
- `/etc/nix/nix.conf`
- `/etc/nix`, but only when it is empty

[S6]

It has no branch that removes `/etc/nix/sentry-endpoint`.[S0][S6]
The runtime evidence agrees with this source behavior.[E3]

## Ownership classification

| Question | Classification | Basis |
|---|---|---|
| Product boundary | **Vendor-owned** | It appears between the clean pre-install capture and the completed vendor install.[E1][E2] |
| Exact internal writer | **Uncertain** | The public source delegates to implementations that are not present: `detsys-ids-client`, embedded `determinate-nixd`, and embedded Determinate Nix commands.[S1][S2][S10][S11] |
| Direct receipt ownership | **Not shown** | The path and identity are absent from the public action model.[S0][S3][S4] |
| Uninstall removal | **Not implemented in the visible action path** | Reverse action reversion has no sentry removal, and runtime retention is proved.[S5][S6][E3] |
| Retention intent | **Uncertain** | The visible source has no comment, policy, or action that explains retention.[S0][S6] |
| DN-03 strict residue contract | **FAIL remains correct** | The surviving file keeps `/etc/nix` non-empty after a successful uninstall.[E3] |

The evidence supports the term **vendor residue**.
It does not support the stronger terms **intentional retention** or **vendor
defect**.

## Smallest remaining runtime probe

DN-03 does not need another runtime probe.
The current evidence is enough to keep the strict residue result at FAIL.

If a later policy decision needs the exact internal writer, use one fresh
disposable Linux guest.
Start a Linux audit watch on `/etc` before the vendor runs.
Run the normal diagnostics-disabled install once.
Then inspect only the audit records whose path is
`/etc/nix/sentry-endpoint`.

The audit record must identify the creating process, executable, PID, parent
PID, syscall, numeric UID, and time.
This one run distinguishes executable and process boundaries.
It does not identify a library call inside the installer process.

If the image has no working Linux audit facility, use the existing source
boundaries as checkpoints instead:

1. After `nix-installer --diagnostic-endpoint '' plan linux`.[S1][S8]
2. After install with `--skip-nix-conf`, `--no-start-daemon`, and
   `--no-modify-profile`.[S9][S12]
3. After an explicit
   `/usr/local/bin/determinate-nixd init --stop-after nix-configuration`.[S10]

These checkpoints require a fresh disposable guest.
`--no-start-daemon` sets the normal start request to false.
However, the init action can still enable a socket immediately when it found
that socket active before the action started.[S9]

The fallback checkpoints isolate component groups, not each command.
The first group includes feature-gated diagnostics construction, plan-time
feedback, and plan construction.[S1][S8]
The second group skips the visible daemon-init call, but still includes
`ProvisionNix`, `SetupDefaultProfile`, and the remaining install actions.
[S4][S11][S12]
The third group executes the visible daemon-init boundary directly.[S10]
An observation at one checkpoint still needs the earlier checkpoints to rule
out these confounders.

Keep the sentry contents private.
Record only no-follow type, numeric owner, mode, size, and SHA-256.
Do not read the receipt payload.

## Recommendation

Make no policy change in DN-03.

Do not whitelist the residue.

Do not add product cleanup for it.

Keep the lifecycle vendor contract at FAIL.
Route any ownership or cleanup policy to the later uninstall-policy work.
Require new proof before that policy changes.

## Source map

All source links use fixed commit
`4132ad07a15ee7d88c096ac7172b7afb2672866b`.

- **S0 — fixed commit-wide search.**
  `git grep -n -F 'sentry-endpoint' 4132ad07a15ee7d88c096ac7172b7afb2672866b --`
  returned no matches. This command searches the full pinned checkout for the
  exact literal. The cited-file subset below cannot reproduce that negative
  result, and the result is not structural proof.
- **S1 — diagnostics construction and dependency.**
  [`Cargo.toml`, lines 11–25](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/Cargo.toml#L11-L25),
  [`Cargo.lock`, `detsys-ids-client` package, lines 532–559](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/Cargo.lock#L532-L559),
  [`src/bin/nix-installer.rs`, `main`, lines 20–43](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/bin/nix-installer.rs#L20-L43), and
  [`src/diagnostics.rs`, `diagnostics` and `DiagnosticData::new`, lines 98–154](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/diagnostics.rs#L98-L154).
- **S2 — prebuilt daemon pin, embedding, and provisioning.**
  [`flake.nix`, lines 14–20 and 49–53](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/flake.nix#L14-L53),
  [`flake.nix`, build environment, lines 89–94](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/flake.nix#L89-L94),
  [`flake.lock`, x86_64 Linux daemon input, lines 68–78](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/flake.lock#L68-L78),
  [`flake.lock`, Determinate Nix source input, lines 137–156](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/flake.lock#L137-L156),
  [`src/distribution.rs`, embedded daemon bytes, lines 47–56](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/distribution.rs#L47-L56), and
  [`src/action/common/provision_determinate_nixd.rs`, `ProvisionDeterminateNixd`, lines 13–120](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/provision_determinate_nixd.rs#L13-L120).
- **S3 — receipt model and writer.**
  [`src/plan.rs`, `InstallPlan`, lines 15–28](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/plan.rs#L15-L28) and
  [`src/plan.rs`, `write_receipt`, lines 351–381](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/plan.rs#L351-L381).
- **S4 — Linux action list and daemon service.**
  [`src/planner/linux.rs`, `Linux::plan`, lines 49–135](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/planner/linux.rs#L49-L135),
  [`src/action/common/configure_determinate_nixd_init_service/mod.rs`, `ConfigureDeterminateNixdInitService`, lines 25–168](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/configure_determinate_nixd_init_service/mod.rs#L25-L168), and
  [`nix-daemon.determinate-nixd.service`, lines 1–17](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/configure_determinate_nixd_init_service/nix-daemon.determinate-nixd.service#L1-L17).
- **S5 — receipt-driven reverse uninstall.**
  [`src/cli/subcommand/uninstall.rs`, `Uninstall::execute`, lines 114–194](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/subcommand/uninstall.rs#L114-L194) and
  [`src/plan.rs`, `InstallPlan::uninstall`, lines 286–334](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/plan.rs#L286-L334).
- **S6 — daemon revert behavior.**
  [`src/action/common/provision_determinate_nixd.rs`, `ProvisionDeterminateNixd::revert`, lines 86–120](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/provision_determinate_nixd.rs#L86-L120).
- **S7 — empty diagnostic endpoint contract.**
  [`src/cli/mod.rs`, `NixInstallerCli::diagnostic_endpoint`, lines 68–79](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/mod.rs#L68-L79).
- **S8 — plan command behavior.**
  [`src/cli/subcommand/plan.rs`, `Plan::execute`, lines 29–69](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/subcommand/plan.rs#L29-L69).
- **S9 — daemon start controls.**
  [`src/settings.rs`, `InitSettings`, lines 357–385](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/settings.rs#L357-L385) and
  [`src/action/common/configure_init_service.rs`, prior socket state and systemd activation, lines 341–486](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/configure_init_service.rs#L341-L486).
- **S10 — direct daemon-init command.**
  [`src/action/common/configure_nix.rs`, `ConfigureNix::execute`, lines 163–188](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/configure_nix.rs#L163-L188) and
  [`src/action/common/place_nix_configuration.rs`, `PlaceNixConfiguration::execute`, lines 353–384](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/place_nix_configuration.rs#L353-L384).
- **S11 — embedded Nix provisioning and commands.**
  [`src/action/common/provision_nix.rs`, `ProvisionNix`, lines 16–165](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/provision_nix.rs#L16-L165) and
  [`src/action/base/setup_default_profile.rs`, `SetupDefaultProfile::execute`, lines 54–138](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/base/setup_default_profile.rs#L54-L138).
- **S12 — skip-configuration control.**
  [`src/settings.rs`, `CommonSettings::skip_nix_conf`, lines 177–189](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/settings.rs#L177-L189) and
  [`src/action/common/configure_nix.rs`, `ConfigureNix::plan`, lines 27–69](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/configure_nix.rs#L27-L69).

Private evidence references are relative to
`/var/tmp/pkg-s6-dn03b-evidence/lifecycle-0d4809e/guest-evidence`.

- **E1 — before state.** `sentry-before-initial.kind`.
- **E2 — installed and upgraded state.** `sentry-after-initial.kind`,
  `sentry-after-initial.stat`, `sentry-after-initial.sha256`,
  `sentry-after-determinate-nixd-upgrade.kind`,
  `sentry-after-determinate-nixd-upgrade.stat`,
  `sentry-after-determinate-nixd-upgrade.sha256`, `install.status`, and
  `determinate-nixd-upgrade.status`.
- **E3 — uninstall state.** `sentry-after-uninstall.kind`,
  `sentry-after-uninstall.stat`, `sentry-after-uninstall.sha256`,
  `uninstall.status`, `etc-nix.stat`, `etc-nix.first-entry`, and `results`.
