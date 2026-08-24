# Spike S6 — Determinate Nix Installer v3.22.1 (research findings)

| | |
|---|---|
| **Spike** | S6 — Verify the facts `pkg` needs about the Determinate Nix Installer at release **v3.22.1**, against primary sources only. |
| **Scope** | This file records the S6 research and the final DN-03 parent decision. Raw downloads and extracted evidence were not committed. The checked-in S6 harness and the public child reports record the accepted Linux and macOS evidence. |
| **Research date** | 2026-08-22 (UTC). Evidence files were kept outside the repo (temp dir), not committed. |
| **Method** | GitHub tag/release API, release asset downloads with local SHA-256, source tree of the tag, LICENSE file, the shipped aarch64-darwin binary executed with `--help`, the embedded `determinate-nixd` binary extracted and executed, official docs at docs.determinate.systems, and the live install.determinate.systems CDN. |

**Evidence labels.** Every claim carries one label:

- **Observed** — seen directly in a primary source cited next to the claim, or executed locally.
- **Inferred** — a conclusion drawn from observed facts. Not stated by the source.
- **Unproved** — could not be verified from primary sources. Do not build on it.

---

## DN-03 parent decision

| Item | Decision |
|---|---|
| Status | COMPLETE — EVIDENCE COMPLETE; PRODUCT DELIVERY NO-GO. |
| Next step | DN-04 may document the proved contract. |
| Vendor | Determinate Nix Installer v3.22.1 at full revision `4132ad07a15ee7d88c096ac7172b7afb2672866b` |
| Accepted children | [DN-03b Linux report](linux-vm/LINUX-FINDINGS.md) and [DN-03c macOS report](macos-vm/FSTAB-CONTRACT-RESEARCH.md) |
| Tested product sources | Linux R12: `33b386dd473e66c7772c4392d7f56953e1398595`; macOS R10: `aa5d5beca51d77ae06a672a97c2b5ebfa050d248`; macOS Crash R1: `1ad44acf6c7780fa5ed3e135c1fcdc734149402f` |

### Observed runtime evidence

**Observed, Linux R12.** R12 ran on 2026-08-23 UTC. It recorded five broad
Linux x86_64 behavior lanes. The retained Linux x86_64 and aarch64 container
lanes record both target Asset proofs. These are the five R12 complete bundle
SHA-256 values:

| R12 lane | Complete bundle SHA-256 |
|---|---|
| lifecycle | `1b0128ba5f4a3e9c913c3778471734dcfe8031dd7539b965e2d1307ca4a6828a` |
| diagnostics-disabled | `b3a5cebc7975be8e04f409592228443afa52fa4428f9be34d5c3bf1b27ad8af0` |
| crash-recovery | `c2ead304a217c214805dee43f1cadbdb857225d4ad31ab434e96d5be4385a682` |
| foreign-nix | `c5deb44b9175110986ebb026ae7ce082b03f02fd043a1bca6234c3a16e8ab966` |
| upstream-input | `a2c92c9b4bab26f0be88b83ba83eba1ae73bf7af3d4083b905e9d8cfdeed9d42` |

**Observed, macOS R10.** R10 ran on 2026-08-24. It recorded one full Apple
Silicon lifecycle, all nine phase archives, and both reboot proofs. Its bundle
tar-stream SHA-256 is
`7002457bd64e15fa2bef620a91850b3d683407c4ede6468892593709fbf95435`.
Its canonical relative file-hash manifest SHA-256 is
`57653027291abd6602892c1be37cb52e80855c39261ebf768a74e895e803bb82`.

**Observed, macOS Crash R1.** The recorded run date is 2026-08-24. The run
used signed source `1ad44acf6c7780fa5ed3e135c1fcdc734149402f`.

| Crash R1 phase or check | Status | Result |
|---|---:|---|
| baseline | `0` | `PASS` |
| crash-kill | `0` | `PASS` |
| validated installer child after SIGKILL | `137` | expected killed child |
| reboot | `0:0` | `PASS`; raw boot time changed and staged hashes were revalidated |
| recovery install | `0` | vendor command returned success |
| crash-recover | `1` | `FAIL` |

The recovered `nixbld` group had GID `350`. It had 31 explicit members from
`_nixbld2` through `_nixbld32`. `_nixbld1` was missing. The lane stopped
before the functional Nix recovery check.

| Crash R1 archive | SHA-256 |
|---|---|
| baseline | `8a16ffa2906b8977e5f0ddbba8691d7297007e7df82ed467df4a4a1d9e7759d2` |
| crash-kill | `2897121be03af30bbfcc86f36e073f2c3323ed3ab59f8e8aefa3371e95c1fab7` |
| crash-recover | `a5afa9f432da4dbcebe69662467d5c437281870a2079d0b6e400f8228d1ff469` |

The Crash R1 bundle tar-stream SHA-256 is
`82e1d1a0291f2cbcade8d5e768433a0163ae2e066406335745f2045091e3a80d`.
Its canonical relative file-hash manifest SHA-256 is
`d17797f2394f037fdb145b24cbe3253cefb3b9af98eacd778fbeb4522f4011f3`.
All three private archives passed safe archive validation. No partial archive
remained. No archive contained protected content. The exact VM and matching
process were absent. The source was clean.

**Observed, vendor uninstall.** The vendor uninstall command completed on
Linux and macOS. Strict residue checks found paths on both platforms.

The exact public Linux final path set in both retained container Asset proofs
is:

- `/etc/nix`
- `/etc/nix/sentry-endpoint`

The inventory of entries under `/etc/nix` contains only `sentry-endpoint`.
The broad Linux x86_64 R12 lifecycle proves that this file remained. It did
not count every `/etc/nix` entry.

The exact public macOS R10 final path set is:

- `/etc/nix`
- `/etc/nix/macos-keychain.crt`
- `/etc/nix/sentry-endpoint`
- empty `/etc/fstab`
- `/var/log/determinate-nix-init.log`
- `/var/log/determinate-nix-daemon.log`

### Inference

- The clean starting state and the post-uninstall observations support
  attribution of the recorded Linux and macOS paths to vendor execution.
- Crash R1 shows that vendor exit status `0` is not sufficient proof of a
  valid recovered installed state. The group check failed before functional
  Nix recovery.

### Unproved

- Successful crash recovery is unproved.
- Functional Nix recovery is unproved because the functional check was not
  reached.

### Decision: platform scope

- Broad Linux behavior is accepted for x86_64.
- Linux Asset proofs are accepted for x86_64 and aarch64.
- The macOS lifecycle and crash observations cover Apple Silicon.
- Intel macOS is unsupported. Release v3.22.1 has no x86_64-darwin asset.

### Decision

- The parent accepts the broad Linux x86_64 behavior evidence and both Linux
  target Asset proofs.
- R10 completes the Apple Silicon lifecycle and residue evidence rows.
- The accepted Crash R1 negative observation completes the standalone
  SIGKILL and reboot evidence row.
- The DN-03 evidence set is complete. Product delivery remains **NO-GO**.
- There is no clean vendor-uninstall claim for Linux or macOS.
- DN-04 may document this proved contract and its negative result.
- DN-06 must not use SIGKILL. It must not accept vendor exit status `0` as
  sufficient proof of a valid installed state.
- DN-07 owns fail-closed Handoff and state validation.
- DN-12 may add an optional `repair sequoia` proof.
- DN-16 remains blocked until the full crash and reboot lifecycle passes
  twice.
- DN-13 owns only exact residue cleanup. It does not own crash recovery.

DN-13 must revalidate every live identity before it removes any path.
Any missing or different identity must stop all cleanup and keep the strict
result as `FAIL`. DN-13 must not use a recursive delete for these manifests.

### Decision limits

DN-03 does not prove:

- successful or functional crash recovery;
- Handoff behavior;
- package parity;
- product cleanup; or
- cutover readiness.

Those proofs remain with their later owners. This decision is evidence
complete. It is not a product-delivery approval or a clean vendor-uninstall
claim.

---

## 1. Tag revision

| Fact | Value | Label |
|---|---|---|
| Tag | `v3.22.1` | Observed |
| Tag type | lightweight (points at a commit, not an annotated/signed tag object) | Observed |
| Commit SHA | `4132ad07a15ee7d88c096ac7172b7afb2672866b` | Observed |
| Commit signature | The commit is GPG-signed. GitHub reports `verification.verified = true`, `reason = "valid"`, with a PGP signature (committer "GitHub", verified at `2026-08-19T20:38:55Z`) | Observed |
| Release id | `373337581`, `draft: false`, `prerelease: false` | Observed |
| Published | `2026-08-19T20:39:01Z` | Observed |
| Release body | lists "Release v3.22.0" (PR #1865) and "Release v3.22.1" (PR #1874); compare link `v3.21.9...v3.22.1` | Observed |

Sources:

- Tag ref: `https://api.github.com/repos/DeterminateSystems/nix-installer/git/refs/tags/v3.22.1` → `object.sha = 4132ad07a15ee7d88c096ac7172b7afb2672866b`, `object.type = "commit"`.
- Release: `https://api.github.com/repos/DeterminateSystems/nix-installer/releases/tags/v3.22.1`.
- Commit verification: `https://api.github.com/repos/DeterminateSystems/nix-installer/commits/4132ad07a15ee7d88c096ac7172b7afb2672866b` → `commit.verification = {"verified": true, "reason": "valid", ...}` with an embedded PGP signature.

Trust chain, stated precisely: the release commit is GPG-signed and GitHub-verified; the tag is lightweight and unsigned and points at that signed commit; the GitHub release attaches assets to that tag. This release has no separate checksum or signature asset, and the commit signature does not directly authenticate the release binaries. The release API exposes per-asset SHA-256 `digest` fields (§3), and our local measurements match them. pkg acceptance rests on the checked-in pinned digest plus a local hash comparison. Observed.

Related facts:

- The **latest** release is `v3.22.2` (published `2026-08-21T20:14:29Z`). Source: `https://api.github.com/repos/DeterminateSystems/nix-installer/releases/latest`. Observed.
- The **stable** channel `https://install.determinate.systems/nix` redirected to `https://install.determinate.systems/nix/tag/v3.22.2` at research time. So the stable one-liner is a **moving target**, not v3.22.1. The pinned form `https://install.determinate.systems/nix/tag/v3.22.1` still serves v3.22.1. Observed.
- The binary reports `nix-installer 3.22.1` from `--version`. Source: shipped aarch64-darwin asset, executed locally. Observed.

---

## 2. License

| Fact | Value | Label |
|---|---|---|
| License | **GNU LGPL v2.1** | Observed |
| `LICENSE` file at the tag | full LGPL-2.1 text, 504 lines, header "GNU LESSER GENERAL PUBLIC LICENSE Version 2.1, February 1999" | Observed |
| GitHub repo metadata | `license.spdx_id = "LGPL-2.1"` | Observed |
| `Cargo.toml` at the tag | `license = "LGPL-2.1"` (line 7) | Observed |

Sources:

- `https://raw.githubusercontent.com/DeterminateSystems/nix-installer/4132ad07a15ee7d88c096ac7172b7afb2672866b/LICENSE`
- `https://api.github.com/repos/DeterminateSystems/nix-installer` (license field)
- source tree of the tag, `Cargo.toml`.

Note for pkg: this is **LGPL-2.1, not LGPL-3.0**, and not a permissive license. See risk R-4.

---

## 3. Release assets: names, sizes, URLs, SHA-256

The release publishes **exactly four assets**: three binaries and one shell script. There are **no `.sha256` sidecar asset files** in the release. Observed (full asset list in the release JSON). The release **API** does expose a `digest` field per asset, formatted `sha256:<hex>`. Observed (release API refetched 2026-08-22).

| Asset | Size (bytes) | SHA-256 (computed locally; equals the release API `digest`) | Label |
|---|---:|---|---|
| `nix-installer-aarch64-darwin` | 58,427,232 | `90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b` | Observed |
| `nix-installer-aarch64-linux` | 69,625,424 | `9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179` | Observed |
| `nix-installer-x86_64-linux` | 74,918,096 | `9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c` | Observed |
| `nix-installer.sh` | 19,299 | `75812a5a4e3b0d5808508f8f4a9497b4ffdb075318347758f532cbbf40955686` | Observed |

Sizes match the GitHub API asset records exactly. SHA-256 values were computed with `shasum -a 256` on the downloaded files. Each locally measured hash **exactly equals** the `digest` field the release API exposes for that asset (verified 2026-08-22). The digest is GitHub-reported metadata for the uploaded release asset. It is not a signature. pkg acceptance rests on the checked-in pinned digest plus a local hash comparison: download the bytes, hash them, compare, and fail on any mismatch. Observed.

GitHub download URLs (Observed, from `browser_download_url`):

- `https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-aarch64-darwin`
- `https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-aarch64-linux`
- `https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-x86_64-linux`
- `https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer.sh`

CDN equivalents (Observed): the installer's own wrapper script pins the base URL `https://install.determinate.systems/nix/tag/v3.22.1` (source: `nix-installer.sh` release asset, line 33: `NIX_INSTALLER_BINARY_ROOT="${NIX_INSTALLER_BINARY_ROOT:-https://install.determinate.systems/nix/tag/v3.22.1}"`). Assets are then fetched from `{base}/nix-installer-{arch}`.

CDN/GitHub byte equality: for `nix-installer-aarch64-darwin`, the CDN copy and the GitHub release copy are **byte-identical** (same SHA-256, `cmp` clean). Observed. The Linux CDN copies were not byte-compared; only the URL scheme was checked. Inferred: same build artifacts.

Source tarball of the tag (Observed): `https://codeload.github.com/DeterminateSystems/nix-installer/tar.gz/refs/tags/v3.22.1`, SHA-256 `e946ce0920e1ac0a76281d1d0d24b5ddb0fa1807f5317d1545130fe8a04ff084`.

---

## 4. Absence of x86_64-darwin

No x86_64 (Intel) macOS build exists for v3.22.1. Evidence:

1. The release asset list contains no `nix-installer-x86_64-darwin`. Observed (Section 3).
2. The CDN returns **404 / S3 `NoSuchKey`** for `https://install.determinate.systems/nix/tag/v3.22.1/nix-installer-x86_64-darwin`. Observed (response body: `<Error><Code>NoSuchKey</Code>...<Key>v3.22.1/nix-installer-x86_64-darwin</Key>`).
3. `flake.nix` at the tag: `supportedSystems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];` and `systemsSupportedByDeterminateNixd = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];` (lines 38–39). Observed.
4. CI has exactly three build workflows: `build-aarch64-darwin.yml`, `build-aarch64-linux.yml`, `build-x86_64-linux.yml` under `.github/workflows/`. No x86_64-darwin workflow exists. Observed.
5. The README platform matrix lists macOS only as "Apple Silicon / `aarch64`". Observed (README at the tag).

The wrapper script still contains generic arch code that can produce the string `x86_64-darwin` (it is derived from rustup's script). On an Intel Mac it would then request a nonexistent file and fail. Inferred.

---

## 5. Architecture support

Supported targets at v3.22.1: **aarch64-darwin, aarch64-linux, x86_64-linux**. Observed (Sections 3 and 4: asset list, flake.nix, CI workflows).

README platform matrix at the tag (Observed):

| Platform | Multi-user | Maturity |
|---|---|---|
| Linux x86_64 and aarch64 | via systemd | Stable |
| macOS Apple Silicon / aarch64 | yes | Stable (with note) |
| Steam Deck (SteamOS) | yes | Stable |
| WSL2 x86_64 and aarch64 | via systemd | Stable |
| Podman/Docker containers | yes / root-only | Stable |

The installer ships planner subcommands to match: on the darwin binary, `install` offers the `macos` planner (other planners hidden); on Linux builds it offers `linux`, `steam-deck`, `ostree`. Observed (binary `--help` output and `src/planner/mod.rs:160-173`).

The wrapper script refuses macOS older than 10.13 (it returns failure for 10.x with minor < 13). Observed (`nix-installer.sh`, `check_help_for` darwin branch).

---

## 6. Exact install argv

### 6.1 The official one-liner

README at the tag (Observed):

```shell
curl -fsSL https://install.determinate.systems/nix | sh -s -- install
```

Source: `README.md`, section "Install Determinate Nix". The same README warns the upstream-Nix option "will be available, however, until January 1, 2026".

A pinned-version variant appears inside the installer's own strings (Observed, `src/cli/subcommand/install/mod.rs:111` and `uninstall.rs:135`):

```shell
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix/tag/v{VERSION} | sh -s -- uninstall
```

### 6.2 What the wrapper script does

Observed, from the shipped `nix-installer.sh` (release asset):

1. Detect architecture. Sets `_arch` to one of `aarch64-linux`, `x86_64-linux`, `aarch64-darwin`, `x86_64-darwin` (line ~232: `_arch="${_cputype}-${_ostype}"`). On Darwin it uses `sysctl hw.optional.arm64` to see through Rosetta.
2. Download `{NIX_INSTALLER_BINARY_ROOT}/nix-installer-${_arch}` to a temp dir. Root defaults to the pinned tag URL (Section 3).
3. `chmod u+x`, then exec the binary **with the script's own arguments**: `ignore "$_file" "$@"`.

So `sh -s -- install` runs `nix-installer install`. Observed. `sh -s -- install --no-confirm` runs `nix-installer install --no-confirm`; the script also special-cases `--no-confirm` and `NIX_INSTALLER_NO_CONFIRM` for tty handling. Observed.

### 6.3 The `install` command surface

From the shipped binary `install --help` (Observed):

```
Usage: nix-installer-aarch64-darwin install [OPTIONS] [PLAN]
       nix-installer-aarch64-darwin install <COMMAND>
```

Key flags (Observed, binary help + `src/cli/subcommand/install/mod.rs`, `src/settings.rs`):

- `[PLAN]` positional: path to a non-default installer plan JSON. Env `NIX_INSTALLER_PLAN`. Conflicts with passing a planner subcommand (`install/mod.rs:114`).
- Planner subcommands (`macos` on darwin builds; `linux`, `steam-deck`, `ostree` on linux builds). With no planner, one is chosen heuristically (`BuiltinPlanner::default`, `src/planner/mod.rs:177+`).
- `--no-confirm` (`NIX_INSTALLER_NO_CONFIRM`), `--explain` (`NIX_INSTALLER_EXPLAIN`).
- `--determinate` (`NIX_INSTALLER_DETERMINATE`): enable Determinate Nix explicitly. Default `false` — but see 6.4: the default distribution is Determinate Nix anyway.
- `--prefer-upstream-nix` (`NIX_INSTALLER_PREFER_UPSTREAM_NIX`): install upstream Nix instead.
- `--no-modify-profile`, `--nix-build-group-name/-id`, `--nix-build-user-prefix`, `--nix-build-user-count`, `--nix-package-url`, `--extra-conf`, `--force`, `--init`, `--proxy`, `--ssl-cert-file`, `--no-start-daemon`. Observed (README "Installer settings" table at the tag and binary help).

### 6.4 Default distribution is Determinate Nix

`CommonSettings::distribution()` at the tag (Observed, `src/settings.rs:324-333`): `--determinate` → Determinate Nix; else `--prefer-upstream-nix` → upstream Nix; **else Determinate Nix**. So a bare `install` installs Determinate Nix. The Determinate Nix tarball and the `determinate-nixd` binary are **embedded in the installer at build time** (`include_bytes!`, `src/distribution.rs:46-57`). Observed.

The tag's `flake.lock` pins the embedded pieces (Observed):

- Determinate Nix flake: `https://api.flakehub.com/f/pinned/DeterminateSystems/determinate/3.22.1/...` → version **3.22.1**, rev `6069a65ab09f683cfd9cb17b21a66a366c8077da`.
- Nix source: `https://api.flakehub.com/f/pinned/DeterminateSystems/nix-src/3.22.1/...` → Determinate Nix is built from `DeterminateSystems/nix-src` 3.22.1.
- `determinate-nixd` binaries per platform: `https://install.determinate.systems/determinate-nixd/tag/v3.22.1/macOS`, `.../aarch64-linux`, `.../x86_64-linux` (with narHash values in the lock file).

### 6.5 Root escalation

If not root, the installer re-execs itself through sudo (Observed, `src/cli/mod.rs` `ensure_root()`): argv becomes `sudo --set-home <current binary> <original args>`, and only selected env vars are preserved: `RUST_LOG`, `RUST_BACKTRACE`, `GITHUB_PATH`, `SHELL`, proxy vars, all `NIX_INSTALLER_*`, all `DETSYS_*`, plus `ORIG_HOME`. Inferred: other environment is dropped across escalation; automation must put config in `NIX_INSTALLER_*` vars or argv, not arbitrary env.

---

## 7. Diagnostics endpoint and the empty-string behavior

Flag name: **`--diagnostic-endpoint`** (singular "diagnostic"). Env: `NIX_INSTALLER_DIAGNOSTIC_ENDPOINT`. It is a global option on the top-level CLI. Observed (shipped binary `--help`; `src/cli/mod.rs:70-80`).

Verbatim help text from the shipped v3.22.1 binary (Observed):

> The URL or file path for an anonymous installation diagnostic to be sent
>
> To disable diagnostic reporting, unset the default with `--diagnostic-endpoint ""`, or `NIX_INSTALLER_DIAGNOSTIC_ENDPOINT=""`

The README says the same (Observed, README section "Diagnostics"):

> To disable diagnostic reporting, set the diagnostics URL to an empty string by passing `--diagnostic-endpoint=""` or setting `NIX_INSTALLER_DIAGNOSTIC_ENDPOINT=""`.

Mechanism, from source (Observed steps, Inferred end result):

- The CLI declares the flag with `num_args = 0..=1` and `default_value = None` (`src/cli/mod.rs:70-80`).
- With no value given, the feedback client is built by `detsys-ids-client` 0.7.0 (pinned in `Cargo.lock`). Its default transport is SRV-based: record `_detsys_ids._tcp.install.determinate.systems.` with fallback URL `https://install.determinate.systems` (`detsys-ids-client-0.7.0/src/transport/mod.rs`, `default_transport_backend`; host constant `IDS_HOST = "install.determinate.systems"`).
- An empty string fails URL parsing, is reparsed as `file://`, and `FileTransport::new("")` fails to open a file. The inner `detsys-ids-client::build_or_default` catches this transport construction error first. It then tries the public default transport. The outer `diagnostics()` dev-null fallback does not receive this error. Therefore, an empty endpoint can select the public default. This control flow is Observed in `src/diagnostics.rs:127-153` and `detsys-ids-client-0.7.0/src/{builder,transport/mod,transport/file}.rs`. Actual external egress is Unproved. See the pinned [macOS diagnostics contract](macos-vm/DIAGNOSTICS-CONTRACT-RESEARCH.md).
- Extra kill switch (Observed in `detsys-ids-client-0.7.0/src/lib.rs`): `DETSYS_IDS_TELEMETRY=disabled` turns reporting off and prints a note. `DETSYS_IDS_TRANSPORT` supplies an ambient endpoint, but the installer endpoint setter replaces it. The official telemetry doc confirms `DETSYS_IDS_TELEMETRY=disabled` as the opt-out. Source: `https://docs.determinate.systems/guides/telemetry/`. Observed.

The bare form `--diagnostic-endpoint` with **no value at all** was not executed. Unproved.

---

## 8. /nix/receipt.json

- Constant `pub const RECEIPT_LOCATION: &str = "/nix/receipt.json";` Observed (`src/plan.rs:15`).
- README at the tag (Observed): "an installation receipt (for uninstalling) is stored at `/nix/receipt.json` as well as a copy of the install binary at `/nix/nix-installer`".
- The receipt is the serialized install plan (`InstallPlan`), and it carries a `version` field of the installer that wrote it (`src/plan.rs:23,40`). Observed.
- On a new install, the installer reads any existing receipt first. If it cannot parse it, it says: "Unable to parse existing receipt `/nix/receipt.json`, it may be from an incompatible version of `nix-installer`. Try running `/nix/nix-installer uninstall`, then installing again." Observed (`src/cli/subcommand/install/mod.rs:94-102`).
- Version compatibility uses semver requirement parsing of the receipt's own version string (`check_compatible`, `src/plan.rs:336-347`). Receipts from a different major version, or newer than the running binary, are refused. Observed. Inferred: within the same major, an older receipt passes (caret semantics).
- v3.22.1 also has a **split-receipt** feature. The `split-receipt` subcommand writes `/nix/uninstall-phase1.json` and `/nix/uninstall-phase2.json`. Phase 1 cleans up everything except the Nix store root; phase 2 removes the store root. This exists to allow reinstalling with a newer installer version without deleting the store. Observed (`src/cli/subcommand/split_receipt.rs:16-47`, doc comment lines 21-27).

---

## 9. /nix/nix-installer (self-copy)

- The installer copies itself to `/nix/nix-installer` with mode `0755`. Function `copy_self_to_nix_dir`, `tokio::fs::copy(path, "/nix/nix-installer")`, then `set_permissions(..., 0o0755)`. Observed (`src/cli/subcommand/install/mod.rs:349-352`).
- This copy happens after a successful install, and best-effort after a failed install (`install/mod.rs:218-219,308-310`). Observed.
- On macOS, a launchd "nix hook" service runs at boot: `/bin/wait4path /nix/nix-installer && /nix/nix-installer repair`. Observed (`src/action/macos/create_nix_hook_service.rs:177`).
- The uninstall path depends on this file (Section 11).

---

## 10. Repair command limits

The shipped binary offers exactly (Observed, `repair --help` and `src/cli/subcommand/repair.rs`):

```
Usage: nix-installer repair [OPTIONS]
       nix-installer repair <COMMAND>

Commands:
  hooks    Update the shell profiles to make Nix usable after system upgrades
  sequoia  Recover from the macOS 15 Sequoia update taking over _nixbld users
```

Limits (Observed):

- **Default repair is `hooks`**: re-runs `ConfigureShellProfile` (and on macOS also `ConfigureRemoteBuilding`). It does not touch the store, the daemon, users, or the receipt. (`repair.rs`, `RepairKind::Hooks` branch, lines ~189-212.)
- **`sequoia` is macOS-only.** On Linux it errors: "The `sequoia` repair command is only available on macOS". It also refuses non-interactive terminals unless `--no-confirm` is passed. (`repair.rs` lines ~217-234.)
- `sequoia` moves the `_nixbld` users to the Sequoia-compatible UID range. It updates the receipt only if it can find the `create_group` action in it; otherwise it warns that `/nix/receipt.json` will not reflect the new UIDs but uninstall will still work. (`repair.rs` lines ~141-160, 250-257.)
- There are no other repair kinds. The enum has exactly two variants. Observed.

Inferred: `repair` is not a general recovery tool. It cannot rebuild a broken Nix install, re-create the store, or fix a damaged daemon.

---

## 11. Uninstall form and receipt argument behavior

### 11.1 The documented forms

- README at the tag, section "Uninstalling" (Observed):

  ```shell
  /nix/nix-installer uninstall
  ```

- README, section "Uninstalling (`nix-installer uninstall`)" (Observed):

  > You can also specify an installation receipt as the first argument (the default is `/nix/receipt.json`):
  >
  > ```shell
  > nix-installer uninstall /path/to/receipt.json
  > ```

- Official docs, troubleshooting page "Recovering from a failed installation on macOS" (Observed): "If `/nix/nix-installer` exists, run the built-in uninstaller: `sudo /nix/nix-installer uninstall`". Source: `https://docs.determinate.systems/troubleshooting/installation-failed-macos/`.

This confirms the three doc claims the main agent saw via Context7: `/nix/nix-installer uninstall` and `/nix/receipt.json` are documented officially (this section), and `sudo determinate-nixd upgrade` is documented officially (Section 13.1).

### 11.2 CLI surface

From the shipped binary `uninstall --help` (Observed):

```
Usage: nix-installer-aarch64-darwin uninstall [OPTIONS] [RECEIPT]

Arguments:
  [RECEIPT]   [default: /nix/receipt.json]
```

Flags: `--no-confirm` (`NIX_INSTALLER_NO_CONFIRM`), `--explain` (`NIX_INSTALLER_EXPLAIN`). Observed.

### 11.3 Behavior, from source (all Observed, `src/cli/subcommand/uninstall.rs`)

1. Requires root; self-escalates via `sudo --set-home` if needed (Section 6.5).
2. If the current directory is exactly `/nix`, it `chdir`s to `/` first (lines 60-70).
3. **Self-delete protection**: if the running executable is exactly `/nix/nix-installer`, it copies itself into a temp dir with a random name and `execv`s the copy with the same argv (lines 72-111).
4. Reads the receipt path given in argv (default `/nix/receipt.json`) and parses it as an `InstallPlan`.
5. On parse failure it reads only the `version` field and fails with guidance to use the matching version: "`/nix/nix-installer uninstall` or `curl ... https://install.determinate.systems/nix/tag/v{plan_version} | sh -s -- uninstall`" (lines 118-141).
6. Runs `check_compatible()`; on version mismatch it prints the same guidance and exits with failure (lines 144-160).
7. Then it prompts (unless `--no-confirm`) and reverts the plan's actions.

Inferred: `/nix/nix-installer uninstall` works because the copy at `/nix/nix-installer` is the **same version** that wrote `/nix/receipt.json`. A different-version binary pointed at that receipt will refuse. The split-receipt flow (Section 8) is the supported way to move to a newer installer while keeping the store.

---

## 12. Absence of installer update

There is **no update/upgrade/self-update mechanism in the installer**. Observed:

- Subcommand enum at the tag: `install, repair, uninstall, self-test, plan, split-receipt` (`src/cli/subcommand/mod.rs`). The shipped binary `--help` shows exactly these six (plus `help`). No update command exists.
- README at the tag, section "Upgrading Determinate Nix" (Observed):

  > If you've installed Determinate Nix, you can upgrade it using Determinate Nixd:
  >
  > ```shell
  > sudo determinate-nixd upgrade
  > ```
  >
  > Alternatively, you can uninstall and reinstall with a different version of Determinate Nix Installer.

  So: the **distribution** (Determinate Nix) is upgraded by `determinate-nixd`. The **installer** is replaced only by running a newer installer.
- The wrapper script always downloads the binary for its own pinned tag; a "newer installer" means a different tag URL or the moving stable URL. Observed (`nix-installer.sh` line 33).
- A fresh install overwrites `/nix/nix-installer` with the new binary (`copy_self_to_nix_dir`). Observed (`src/cli/subcommand/install/mod.rs:349-352`). Inferred: the copy in `/nix` reflects the last installer version run, not a self-updated one.

---

## 13. determinate-nixd: update ownership and absolute-path uncertainty

### 13.1 Official docs

Docs page "Determinate Nixd" (Observed, `https://docs.determinate.systems/determinate-nix/determinate-nixd/`), section "Upgrade Nix":

> To upgrade Nix to the most recent version of Nix advised by Determinate Systems:
>
> ```shell
> sudo determinate-nixd upgrade
> ```
>
> Additionally, you may specify a target version to be installed:
>
> ```shell
> sudo determinate-nixd upgrade --version v3.6.2
> ```
>
> You need to run this command with sudo.

Also documented there: `determinate-nixd version` ("If you're not on the latest version, Determinate Nixd provides upgrade instructions") and `determinate-nixd init`. Observed.

### 13.2 Placement by the installer

- The installer writes the embedded `determinate-nixd` bytes to **`/usr/local/bin/determinate-nixd`** with mode `0555`. Observed (`src/action/common/provision_determinate_nixd.rs:13`, `execute()` body).
- Init services call it: launchd/systemd units run `/usr/local/bin/determinate-nixd daemon`; the macOS volume actions call `/usr/local/bin/determinate-nixd init`. Observed (`src/action/common/configure_determinate_nixd_init_service/mod.rs:211`, `src/action/macos/create_determinate_volume_service.rs:191`, `create_determinate_nix_volume.rs:236`).
- Byte equality: the `determinate-nixd` binary embedded in the v3.22.1 aarch64-darwin installer is **byte-identical** to the CDN file at `https://install.determinate.systems/determinate-nixd/tag/v3.22.1/macOS` (same SHA-256 `73b0d0de73683eb3a435f97d1b5319cd98f17bc4fc5980c925a44bfbe53e08a4`, size 27,193,680). Observed (extracted from the installer's Mach-O segments, then downloaded and compared).

### 13.3 The shipped binary's own CLI

Extracted from the v3.22.1 installer and executed locally (Observed):

```
Commands:
  auth, status, init, upgrade, bug, login, fix, completion, version, help

Options:
  --nix-bin <NIX_BIN>  [default: /nix/var/nix/profiles/default/bin]
  --config-file <CONFIG_FILE>
```

```
Usage: determinate-nixd upgrade [OPTIONS]

Options:
  --profile <PROFILE>  The profile for which you'd like to upgrade Determinate Nix
                       [default: /nix/var/nix/profiles/default]
  --version <VERSION>  Target upgrade version [default: stable]
```

Strings in the binary also contain: "Upgrading Nix requires root privileges. Try running again with sudo." Observed.

So the **default upgrade target is the Nix profile `/nix/var/nix/profiles/default`**, with `--version` defaulting to the `stable` channel. Observed.

### 13.4 What is known and unknown about upgrade internals

`determinate-nixd` source is **not public**. The DeterminateSystems GitHub org has no repo for it; the `determinate` repo holds only Nix modules/config. Observed (org repo list via GitHub API). Strings inside the shipped binary show a module path `src/command/upgrade.rs`, event/span names `upgrade_nix`, `upgrade_dnixd`, `restart_macos`, field names `tools_url`, `daemon_url`, `profile`, and default address `https://install.determinate.systems` (overridable via `INSTALL_DETERMINATE_SYSTEMS_ADDR` / `--install-determinate-systems-addr`). Observed as strings.

Paths referenced by the shipped binary (Observed as strings): `/nix/var/nix/profiles/default`, `/nix/var/nix/profiles/per-user/root/profile` ("Normalizing ... to be linear"), `/nix/var/nix/daemon-socket/socket`, `/var/run/determinate-nixd.socket`, `/nix/var/determinate/determinate-nixd.socket`, `/nix/var/determinate`, `/etc/nix/macos-keychain.crt`, `/etc/nix/nix.conf` settings (`extra-substituters` cache.flakehub.com, `upgrade-nix-store-path-url https://install.determinate.systems/determinate-nix/stable/fallback-paths.nix`, netrc-file).

| Question | Status |
|---|---|
| Who owns Determinate Nix version upgrades? | `determinate-nixd` (`sudo determinate-nixd upgrade`). Observed (docs + binary help). |
| Which profile does upgrade target by default? | `/nix/var/nix/profiles/default`. Observed (binary `--help`). |
| Does upgrade also replace `/usr/local/bin/determinate-nixd`? | Unproved. The string `upgrade_dnixd` suggests a self-update phase, but semantics are not documented and source is closed. Inferred at best. |
| Does upgrade touch `/nix/nix-installer`? | No evidence either way. Unproved. Inferred: no (it is owned by the installer, not by the daemon). |
| Exact download URL(s), atomicity, rollback on failure | Unproved (closed source). |
| What `--version stable` resolves to | Unproved (channel semantics not documented; the installer README's `nix-upgrade` URL `https://install.determinate.systems/nix-upgrade/stable/universal` is set in `nix.conf` as `upgrade-nix-store-path-url` — observed in `src/action/common/place_nix_configuration.rs:176` — but its relation to `determinate-nixd upgrade` is Unproved). |

---

## 14. Cross-check of the main agent's Context7 claims

| Claim seen via Context7 `/websites/determinate_systems` | Verified here | Verdict |
|---|---|---|
| Docs show `/nix/nix-installer uninstall` | docs.determinate.systems troubleshooting page + README at tag + source strings | Confirmed. Observed. |
| Docs show `/nix/receipt.json` | README at tag; source constant; docs do not carry this exact path on the pages fetched (README is the citation) | Confirmed via primary source. Observed. |
| Docs show `sudo determinate-nixd upgrade` | docs.determinate.systems Determinate Nixd page, exact command, incl. `--version` variant | Confirmed. Observed. |

---

## 15. Risk table — what later pkg PRs must NOT assume

| # | Do not assume | Why | Evidence |
|---|---|---|---|
| R-1 | Checksum sidecars ship with the release, or an API digest is a signature | This release has no separate checksum or signature asset. The release API does expose per-asset `digest` (SHA-256) fields, but those are GitHub-reported metadata for the uploaded asset, not signatures. pkg acceptance rests on the checked-in pinned digest plus a local hash comparison | §3 |
| R-2 | An x86_64-darwin installer exists | 404 on CDN, absent from assets/flake/CI; Intel Macs cannot install v3.22.1 | §4 |
| R-3 | `https://install.determinate.systems/nix` is pinned to v3.22.1 | Stable channel already moved to v3.22.2 during research; use `/nix/tag/v3.22.1` for pinning | §1 |
| R-4 | A permissive or LGPL-3.0 license | License is LGPL-2.1; matters for bundling/redistribution of the binary | §2 |
| R-5 | The receipt is a stable, version-independent contract | `check_compatible()` rejects other-version receipts; parse errors direct users to the matching installer | §8, §11 |
| R-6 | `nix-installer repair` fixes a broken install | Only `hooks` and macOS `sequoia` exist; no store/daemon repair | §10 |
| R-7 | `pkg` owns Nix version upgrades on a Determinate install | `determinate-nixd upgrade` owns the profile `/nix/var/nix/profiles/default`; pkg must not race or overwrite it | §13 |
| R-8 | `determinate-nixd upgrade` leaves `/nix/nix-installer` or `/usr/local/bin/determinate-nixd` untouched | Closed source; path set of upgrade is Unproved | §13.4 |
| R-9 | The installer can self-update | No update command exists; replacement requires re-running an installer | §12 |
| R-10 | `/nix` and the receipt persist against user action | `/nix/nix-installer uninstall` (and docs recommending `sudo` for it) can remove them at any time; pkg state must survive or detect absence | §11, §14 |
| R-11 | Telemetry is off in automation | Diagnostics are on by default; use `DETSYS_IDS_TELEMETRY=disabled`, with a valid loopback endpoint as a fail-safe canary | §7 |
| R-12 | Environment survives the sudo re-exec | Only `NIX_INSTALLER_*`, `DETSYS_*`, proxies and a few vars are preserved | §6.5 |
| R-13 | `--prefer-upstream-nix` is gone or permanent | README says "until January 1, 2026" but the flag still exists in v3.22.1 with no date check in source; status is unstable | §6.3 |
| R-14 | Asset names/URLs follow one scheme forever | Two schemes exist today: GitHub `releases/download/...` and CDN `install.determinate.systems/nix/tag/...`; both served identical bytes at this tag | §3 |

---

## 16. Unresolved facts

1. **Unproved** — exact filesystem effects of `determinate-nixd upgrade` (paths written, self-update behavior, download URLs, rollback). Closed source.
2. **Unproved** — what `--version stable` resolves to, and how `upgrade-nix-store-path-url` (`nix-upgrade/stable/universal`) relates to `determinate-nixd upgrade`.
3. **Unproved** — bare `--diagnostic-endpoint` with no value (flag present, empty or missing value semantics under clap `num_args = 0..=1`).
4. **Not captured** — a full sample `/nix/receipt.json` for v3.22.1 (would require an actual install; not run here).
5. The tag is lightweight and unsigned: there is no signed tag object. The release commit itself is GPG-signed and GitHub-verified (`verified=true`, `reason=valid`), and the release API digests match our local measurements. This release has no separate checksum or signature asset, and the commit signature does not directly authenticate the release binaries. Trust in artifact bytes rests on the checked-in pinned digest plus a local hash comparison. Observed.

---

## Appendix A — evidence not committed

All raw downloads and extractions live in a temp directory outside the repo: binaries, source tree of the tag, `flake.lock`, docs HTML/text, release/tag/commit JSON, extracted `determinate-nixd`, hash outputs. None of that raw evidence was committed. The checked-in S6 harness is separate from the research evidence recorded here.

Key reproducible commands:

```shell
curl -sSf https://api.github.com/repos/DeterminateSystems/nix-installer/git/refs/tags/v3.22.1
curl -sSf https://api.github.com/repos/DeterminateSystems/nix-installer/releases/tags/v3.22.1
curl -sSfL -o bin https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-aarch64-darwin
shasum -a 256 bin
./bin --version && ./bin uninstall --help && ./bin repair --help
```

## Appendix B — cited sources

- GitHub refs API: `https://api.github.com/repos/DeterminateSystems/nix-installer/git/refs/tags/v3.22.1`
- Release API: `https://api.github.com/repos/DeterminateSystems/nix-installer/releases/tags/v3.22.1`
- Commit API (verification): `https://api.github.com/repos/DeterminateSystems/nix-installer/commits/4132ad07a15ee7d88c096ac7172b7afb2672866b`
- Latest release API: `https://api.github.com/repos/DeterminateSystems/nix-installer/releases/latest`
- Repo metadata API: `https://api.github.com/repos/DeterminateSystems/nix-installer`
- LICENSE at tag: `https://raw.githubusercontent.com/DeterminateSystems/nix-installer/4132ad07a15ee7d88c096ac7172b7afb2672866b/LICENSE`
- Source tarball: `https://codeload.github.com/DeterminateSystems/nix-installer/tar.gz/refs/tags/v3.22.1`
- Release assets: `https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/{nix-installer-aarch64-darwin,nix-installer-aarch64-linux,nix-installer-x86_64-linux,nix-installer.sh}`
- CDN: `https://install.determinate.systems/nix`, `https://install.determinate.systems/nix/tag/v3.22.1/...`, `https://install.determinate.systems/determinate-nixd/tag/v3.22.1/macOS`
- Dependency source: `detsys-ids-client` 0.7.0 from crates.io (`https://static.crates.io/crates/detsys-ids-client/detsys-ids-client-0.7.0.crate`)
- Official docs: `https://docs.determinate.systems/determinate-nix/determinate-nixd/`, `https://docs.determinate.systems/troubleshooting/installation-failed-macos/`, `https://docs.determinate.systems/guides/telemetry/`, `https://docs.determinate.systems/getting-started/individuals/`
- Shipped binaries executed: `nix-installer-aarch64-darwin` (release asset), `determinate-nixd` (extracted from it)
