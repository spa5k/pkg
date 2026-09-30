# Harden native package lifecycle

The reliability audit found defects in the client, cask metadata, and archive extraction paths.
The native Nix profile remains the authority for package state.

## Required behavior

- A cancelled native command must be reaped. Filtered output must report the same interruption as verbose output. A retry must be an explicit new user command.
- A local source remains mutable when its directory name contains 40 hexadecimal characters. Only source types with Git revision semantics can be fixed by a full commit.
- A qualified request reads only its selected saved tap. Bare resolution must refuse an unreadable saved tap because that tap can contain another match. Doctor must report unreadable taps without changing them.
- Exact lookup must give distinct, usable choices when package namespaces have the same short name.
- The catalog must retain both declared macOS bounds. The client must compare these bounds with the actual host before an install or upgrade. Invalid bounds must fail closed. The existing catalog baseline policy remains in force.
- Archive extraction must remove materialized macOS metadata streams at every directory level. It must preserve vendor files, signed resources, and validated relative links.

## Verification

Use compiled client processes, disposable profiles, and real Nix. Check cancellation, package use, mutable upgrades, rollback, removal, name collisions, damaged taps, and host version refusal. Run cask archive and builder checks when their source changes. Keep these checks in the existing CI workflows.

Linux evidence does not establish macOS app launch behavior. Use a disposable macOS VM for platform checks. Record the source revision, system version, results, and limits in a dated verification report.
