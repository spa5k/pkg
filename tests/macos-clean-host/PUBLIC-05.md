# PUBLIC-05: Nixpkgs catalog coverage

Date: 2026-09-23. Result: local gates and native macOS checks passed.
No public channel or release was changed by this proof.

## Source and environment

- Catalog and name-only derivation change: `e7972ddf783d0c4c6611a6039373f159edc21733`.
- User-environment metadata and unknown-version handling:
  `0d1a40d290cb358664ac18d201628f1c366f0707`.
- Disposable Apple Silicon Tart VM, macOS 15.7.7, Determinate Nix 2.35.2.
- Nixpkgs revision: `a62e6edd6d5e1fa0329b8653c801147986f8d446`.
- Nixpkgs content hash: `sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=`.
- Local signed test channels used sequences 48 and 49 with the existing alpha
  test root. They were served only inside the VM. These are not public releases.

The release builder used `--stage TARGETS SYSTEM GENERATED_AT` for both systems.
It read the pin and sequence from the descriptor and generated every record
through the fixed projection. No package list was supplied.

| Target | Indexed records | Marked available | Compressed bytes |
|---|---:|---:|---:|
| Apple Silicon macOS | 64,412 | 54,685 | 2,781,997 |
| x86-64 Linux | 69,517 | 68,106 | 2,973,636 |

Counts include aliases and nested package sets. Availability is upstream
metadata, not a claim that every package was installed or tested.
Both indexes were evaluated on macOS. This does not prove native Linux installs.

## Observed results

| Check | Result |
|---|---|
| Nix projection fixture | Nested discovery, skipped evaluation errors, platform exclusion, broken flag, and whitespace normalization passed |
| Alpha.47 client catalog refresh | Accepted the larger signed index through `pkg update` |
| Search and info | Found `jq` variants and inspected `python311Packages.requests` |
| Cached install | `jq` 1.7.1 ran and checked a JSON expression |
| Local compilation | `figlet` 2.2.5 built with Clang and printed the expected text |
| Name-only derivation | Candidate broker resolved `unixtools.watch`; candidate CLI installed it and `watch --version` ran |
| Nested package and metadata | `python311Packages.httpie` installed with default collision policy and printed 3.2.2 |
| Removal and rollback | Removed HTTPie, verified its command was absent, then restored and ran it |
| Unknown version | Install, list, and history diff emitted JSON `null` without a false failure |
| Upgrade at the same pin | `pkg outdated` reported zero; `pkg upgrade jq` reported no eligible change |
| Health | Final `pkg doctor` reported healthy |

The figlet output was absent before the build. The VM HTTPS proxy returned 404
only for its exact output narinfo. Other cache requests were forwarded with
normal upstream TLS verification. The retained Nix log contains Clang compile
and link commands. Build dependencies were partly warm; this is not another
cold-toolchain proof. PUBLIC-04 retains the earlier cold compiled-package test.

The expanded tests found and fixed three issues: missing `pname`, false
collisions on `nix-support/propagated-build-inputs`, and rejection of empty
versions after a successful commit. Nix's own
[buildEnv excludes the top-level nix-support directory](https://github.com/NixOS/nixpkgs/blob/a62e6edd6d5e1fa0329b8653c801147986f8d446/pkgs/build-support/buildenv/builder.pl#L125).
The fix keeps those bytes in their store outputs. Real file collisions still
fail under the default policy. Existing generation verification is unchanged.

## Validation and retained evidence

Local G-LINT and G-QUALITY passed. Index, adapter, release, CLI, and store tests
passed. All 44 Python release tests passed. CLI integration tests passed; the
missing-broker case ran with network access denied because the host has a real
broker. Documentation links passed.

Evidence is retained outside the repository in `pkg-catalog-validation` on the
development host: generated indexes and descriptors, signed test channels,
catalog counts, generation logs, package operation logs, the compiler log,
candidate binaries, and local gate output. No TLS private key is retained there.

The CLI candidate SHA-256 is
`0c13a1a8e06d22b458f4b04cd60266fc984c12305ae69285434bcd96fb73b0d6`.
The broker candidate SHA-256 is
`3db33444e80ab036caa91feac772ded1a4026d572e4e856a7c2600ebeb037172`.
The helper candidate SHA-256 is
`4cb0b90e70c17fea39067a7e25251a36dfe0ddd705858816f09efa9613de1b5a`.

The existing foreground alpha.47 installer installed the candidate broker and
helper from the VM channel. The final CLI was invoked from its test directory.
This is candidate evidence, not a new signed release-binary claim.

The VM hosts file and certificate bundles were restored byte-for-byte. macOS
refused noninteractive removal of its temporary keychain trust entry, so the
disposable VM was stopped and deleted after logs were saved. No host trust
settings or installed product files were changed.
