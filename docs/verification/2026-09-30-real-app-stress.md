# Real app stress test — 2026-09-30

GLM-5.3 ran through local Pi and E2B with provider `zai`. The macOS run
tested 15 real GUI apps. It found a signature-preservation limit in Calibre
and Obsidian. The repair refuses their unsupported signature streams before
extraction. The other 13 apps pass the repeated lifecycle checks.

This extends [PR #98](https://github.com/spa5k/pkg/pull/98), task NR-07.
The earlier [reliability report](2026-09-30-native-package-reliability-fixes.md)
records the six preceding repairs and the required helper base in PR #97.

## Confirmed finding and repair

Calibre 9.13.0 stores a vendor signature for
`Contents/Frameworks/plugins/python-lib.bypy.frozen` in `com.apple.cs.*`
extended attributes. Its original app passes deep, strict verification.
The installed app fails that check because those attributes are absent.
All 1446 regular-file hashes, link targets, and executable bits still match.

Obsidian 1.13.7 uses the same signature format for
`Contents/Resources/obsidian.asar`. Its main app starts and passes deep,
strict verification. Its separately signed data file loses its signature.
Verification of that file passes in the original app and fails in the Nix
store. A main-bundle check alone did not detect this loss.

A direct native `nix store add-path` of Calibre's original frozen file also
loses its signing attributes. This reproduces the storage limit without
the pkg extractor. No file was re-signed or changed to bypass verification.

The extractor now refuses `:com.apple.cs.` streams during the read-only
7zz listing. Metadata cleanup also refuses them instead of deleting them.
Ordinary provenance streams still get removed. Vendor resources, ordinary
colon names, and validated relative links still pass the archive tests.

Normal client output now selects the specific `cask-build:` cause before
the generic Nix build error. The final native refusal is:

```text
Failed: install did not complete.
Installed entries unchanged.
Cause: cannot preserve an external code signature in the Nix store: python-lib.bypy.frozen:com.apple.cs.CodeEntitlements
Details: nix log /nix/store/…-cask-calibre-…-9.13.0.drv
```

Both real casks are refused with the final client. The profile manifest and
generation remain unchanged. There is no app-name exclusion in the code.
The catalog's metadata eligibility remains unchanged; it does not prove
that a downloaded vendor archive can be installed safely.

## Fifteen real apps

Each initial cycle ran `info`, install, list, duplicate install, launcher
sync, signature checks, comparison with the original vendor app, startup,
exact-ID removal, and a final empty-profile check. Original downloads were
checked against the pinned SHA-256 values. Startup required a process at
the canonical installed app path. An `open` exit code alone was insufficient.

After the guard changed, the 14 initially supported apps were rebuilt and
tested again. Thirteen passed every check. Obsidian was correctly refused
because of its separate data-file signature. Calibre was tested separately
for refusal. The final rebuilt client also passed a complete Stats cycle.

| App | Darwin version | Final result |
| --- | --- | --- |
| Stats | 3.0.13 | Pass |
| Raycast | 1.104.25 | Pass |
| Rectangle | 0.99 | Pass |
| AltTab | 11.5.0 | Pass |
| Maccy | 2.7.1 | Pass |
| AppCleaner | 3.6.8 | Pass |
| IINA | 1.4.4 | Pass |
| noTunes | 3.5 | Pass |
| Amethyst | 0.24.3 | Pass |
| Transmission | 4.1.3 | Pass |
| Visual Studio Code | 1.134.0 | Pass |
| Cursor | 3.17.19, ae3a2b7231dd56194447fe4570dfdc61640b1e9a | Pass |
| Audacity | 3.7.8 | Pass |
| Obsidian | 1.13.7 | Unsupported external signature; refused |
| Calibre | 9.13.0 | Unsupported external signature; refused |

The test also checked all 24 declared command links in IINA, VS Code,
Cursor, and Calibre. Eight supported version/help commands ran through
those links. Calibre's command checks were controls from before refusal.
This is not a claim that every vendor command or GUI feature was tested.

## Real package edge checks

| Check | Result |
| --- | --- |
| Install Stats, Rectangle, and Maccy together | Three distinct native entry IDs |
| Remove Stats by its exact ID | Rectangle and Maccy entries remain identical; both start |
| Remove noTunes, then rollback | The native entry returns; its restored launcher starts |
| Install a valid cask with a missing token | Failure; unchanged manifest; no valid item installed |
| Install Claude Code stable and latest together | Real `bin/claude` conflict; first package remains unchanged |
| Add Nixpkgs hello, then install and remove Stats | Native hello entry remains identical and runs |
| Interrupt a cold UTM realization | Exit 130; unchanged profile |
| Retry UTM explicitly | Install and startup pass; removal leaves an empty profile |
| Reinstall UTM through `cask:utm` | Manifest remains byte-identical |

Cancellation was observed when Nix began building the install-plan
derivation. This does not establish interruption during the vendor download.
Nix can retain cached outputs after a cancelled operation; an unchanged
store-object count is not a requirement for profile atomicity.

## Source and test controls

- Base source: `1615bc99aebe377378e6d46c37019c04da49f709`.
- VM: disposable `pkg-real-apps-glm-retry-20260930`, macOS 15.7.7,
  Apple silicon, Determinate Nix 2.35.2.
- Before the initial cycles: all 919 source hashes match the base.
- Final VM changes: only `nix/casks/lib/plan.py` and
  `tools/cask-build-check/plan_tests.py`. Their hashes match the host.
- Final extractor SHA-256:
  `e56e93117061da0c080f6fd85ec69ffaee398cb444e649ba30e2068ef7f51be4`.
- Final rebuilt client SHA-256:
  `a409eb1d526ddf1c6ce7a09ef6dbef6d0a29034cd46cb37b6745555f0ddf434f`.
  It matches the host build and the client used for final refusal and Stats
  checks. The earlier broad cycles used the preceding verified client.
- Catalog input pin and payload hashes are unchanged.

The new real-7zz regression runs unchanged against the previous extractor.
That control extracts the payload and fails the refusal assertion. The
candidate refuses it and leaves the extraction directory empty. The complete
suite then passes all 58 checks on the host and in the VM with Nix Python
3.13.13. The macOS system Python 3.9 was too old for the existing tar filter
API; that attempted run was excluded.

The existing compiled CLI diagnostic test uses the native failure shape.
Before repair, it fails because output selects `Cannot build`. After repair,
it passes because output selects the specific signature cause. The real
Calibre and Obsidian refusals verify that same result with actual Nix.

Rust build, all 240 tests, format, Clippy, rustdoc, and dependency policy
checks pass. Documentation links, OpenSpec validation, and diff whitespace
checks pass. The existing CI owns the Linux builder checks.

## Discarded runs and limits

The first VM encountered host disk exhaustion. Late file reads returned
`Illegal byte sequence`. Its test runner also parsed the native manifest
incorrectly, used incomplete removal checks, and matched the wrong process
path. Those results are not counted. Generated Rust incremental files were
removed to recover disk space. A fresh VM repeated the complete test with
corrected checks.

The E2B pilot installed Duplicacy CLI. Its later runner used the wrong XDG
configuration path and therefore read main's older schema-3 catalog. Those
quick failures are test-setup failures, not completed real-app cycles.
Further model/shell calls reached their deadlines. Available logs were
downloaded and the sandbox was stopped. None of those attempts count toward
the 15 macOS apps.

Raw GLM notes are preserved as history. The parent review corrected their
interpretation of native IDs: `casks` can be the exact ID of one element.
Removing it must preserve other elements. It is not an ambiguous alias.

The guard detects explicit signing-stream names reported by 7zz. It does
not parse signing attributes inside AppleDouble bodies or tar/pax headers.
The tests do not cover all vendor versions, interactive app features,
Gatekeeper quarantine, concurrent mutations, or forced termination.
Supporting these externally signed apps needs a storage design that retains
their signature attributes. The current repair does not provide that design.

Both task-owned VMs were stopped and deleted after evidence collection.
Other VMs and the user's package profile were not changed.

The [evidence archive](real-app-stress-2026-09-30.tar.gz) contains the source
hashes, model identity, raw review notes, native command logs, app and
signature controls, failure controls, and project check results. It excludes
vendor downloads, credentials, and model reasoning traces.
