# macOS 27 app helper verification

Verified on Apple silicon with macOS 27.0.1 (26A434) and Determinate Nix
2.35.2 on 2026-09-30.

## Cause

The pinned `mac-app-util` commit
`039f33deef21782d4db97087f426504951239887` locks Nixpkgs to
`80d591ed473cfc46329932c2aadac9b435342c7c`, which selects SBCL 2.6.4.
Running that SBCL binary directly fails before any Lisp code runs:

```text
failed to allocate 1048576 bytes at 0x300100000
```

The result is the same outside the Codex sandbox. Data and address-space
limits are unlimited. The failure is not a package installation failure.
[SBCL 2.6.6 release notes](https://www.sbcl.org/all-news.html) record the
ARM64 static-space address fix for macOS 27.

## Fix and native checks

Retain the exact helper commit. Override only its `nixpkgs` input with
`b6c8664de9b6cc07fe5666a29f91884ba81197c4`, which supplies SBCL 2.6.8.

- A temporary native helper profile builds successfully.
- `mac-app-util --help` starts and lists `sync-trampolines` and `sync-dock`.
- `nix build --no-link --profile DEST -- SOURCE` activates the built
  native profile and retains the helper's source identity.
- The fixed client runs `pkg apps sync` successfully on the affected host.
- `Stats.app` is present in the owned launcher folder.
- `pkg doctor` reports no problems.
- The package manifests captured before and after sync are byte-identical.
- The installed alpha.8 client can reuse the corrected helper and sync again.

## Regression checks

The tests cover a verified old helper with this address conflict, successful
replacement, failed replacement build, failed replacement startup,
unrelated startup errors, and reuse of a working helper. They assert that
activation occurs only after replacement verification. No native helper
remove or upgrade command is used.

The CLI checks replay Nix build-log prefixes, the Determinate failure
symbol, and the expected input-override lock diff. Normal output hides
these details. Warnings and trust prompts remain visible. Verbose output
retains the full lock diff.
