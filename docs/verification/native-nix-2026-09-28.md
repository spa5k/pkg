# Native Nix verification — 2026-09-28

Status: implemented and verified locally. Client `0.2.0-alpha.1` is an
**unreleased candidate**. Nothing is published.

- Implementation base: `5256cfa5a052f64ae4219ac1aaf316a7a00d49a2` (plan base).
- Runtime: external Determinate Nix 3.22.1 / Nix 2.35.2, daemon
  `trusted:false`, ordinary users (Linux `uid=1001`, macOS `uid=501`).

## macOS 15.7.7, Apple silicon

Disposable Tart image pinned to `3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c`.
Homebrew was preinstalled in the image. It was moved aside to
`/opt/homebrew-disabled-for-pkg-proof` before the final archive checks. No
Brew command was invoked at any point.

Archive install and core journey: install, info, list, upgrade, history,
rollback, remove, prune — all as ordinary user.

Casks (initial support, three representatives): `iterm2` 3.6.11,
`chromedriver` 152.0.7977.54, `cursor` 3.17.19. All built with
import-from-derivation disabled. Codesign strict checks pass; app bundles are
intact store outputs. Launchers open via `open`. CLI versions work. Spotlight
`mdfind` returns both apps. The Dock helper sync moved an old Cursor pin from
the old store target to the new store target.

Launcher ownership: an unowned folder was refused and the user file inside it
was preserved. A profile mutation followed by a launcher failure returned a
nonzero partial result stating the package change completed; `pkg apps sync`
then retried successfully with an identical package inventory. Removing
`iterm2` removed its launcher; rollback restored it. `upgrade --all`
refreshed launchers. The helper profile was retained through user-package
operations. Doctor detected a missing launcher without repairing it;
`pkg apps sync` fixed it. Cache deletion and `pkg update` preserved the
profile. Client replace and client removal kept Nix, the profile, and
generations.

Interruption: SIGINT sent only to the pkg process during a real build. The
signal was forwarded, the child was reaped, exit code was 130, and the
profile re-read showed the build not applied.

Search timing (real pinned nixpkgs `567a49d…`): cold 72.42 s, warm 2.69 s.
Controlled three-package path source: 3.83 s cold, 2.98 s warm.

GC roots: checked on both systems. Linux full `nix store gc` deleted 968
paths (294.3 MiB); the retained generation still runs. macOS controlled GC
passed. Default profiles were untouched.

Known limit: if the external helper leaves Dock pins behind after app
removal, pkg does not remove them. pkg has no Dock engine and will not grow
one.

Initial Cask support is exactly three tokens. Raycast 2.0.5 declares
macOS 26 in its Info.plist and `open` fails on macOS 15, so it is excluded
(`minimum-os`, verified on device).

## Linux x86_64 (2 GB sandbox)

From `/tmp/pkg-linux-final-report.md` and `/tmp/pkg-linux-final.log`:
archive built by `tools/release/package_client.sh`, fresh-prefix install,
doctor, inventory, install/list/info/remove, plain GitHub nixpkgs fetch of
`hello` (23 s), fixed-reference `upgrade --all` explicit reporting, native
file-conflict output with state re-read, source-failure state re-read,
moving-source upgrade and rollback with store-path evidence, cache-delete and
`update` profile preservation, completions, client replace and client removal
with Nix, profile, and generations retained, and SIGINT-to-pkg-only during a
real build (exit 130, child reaped, profile re-read).

Real full nixpkgs search did not finish within 300 s on the 2 GB sandbox; no
OOM observed; no leftover processes. Controlled source: 0.105 s cold,
0.103 s warm. The Linux full-search timing number is unmeasured and needs a
larger host. macOS numbers above stand as the reference.

## Final Linux archive verification (fresh E2B, corrected code)

Fresh sandbox with external Determinate Nix 3.22.1 / Nix 2.35.2 as an
ordinary untrusted user. Final code including targeted info. `cargo fmt`,
`cargo clippy --all-targets -- -D warnings`, and 32 tests pass.

Final Linux client archive SHA256:
`420199e46a2b7751cc301761b4f2e7381cfce73982771432cd9f2c4aca437dcf`.
The parent independently matched the downloaded archive to this digest.
`package_client.sh` collected 46 dependency licenses into the archive.

The unchanged `install.sh` was exercised with a local curl transport fixture
that supplied the exact archive and checksum; the release itself stays
unpublished. It passed: fresh-prefix install, binaries, completions, and
checksum verification; `pkg doctor`; explicit pinned GitHub
`hello` (`567a49d1913ce81ac6e9582e3553dd90a955875f`) install, run, and list;
fixed-reference upgrade report; remove, rollback, and `prune --older-than 1d`;
`shellenv` evaluated twice; Linux `update` skipping the deliberately dead
Casks source; and client replace then client remove leaving the native
profile, Nix, and `hello` intact. Manifest decoding stayed strict version 3.

Exact info on Linux used the default moving `nixpkgs-unstable`, locked at
`419fe0f449b3fbe3bdd53d9840288db4509ec32e` — a different source from the
pinned install, as designed. Verbose output confirmed the targeted query
`nix search <locked>#hello .`; the warm run took 0.190 s and exited 0. The
first cold bounded run printed its output but did not cleanly return before
the 30 s watchdog; it is recorded as not a successful cold timing, not as a
pass. No full-catalog search ran in this fresh environment; the older 2 GB
sandbox 300 s full-search limit above remains the known Linux limit.

## Follow-up verification, same day (corrected code)

- Standing checks at the corrected revision: `cargo fmt` and
  `cargo clippy --all-targets` clean; 32 Rust tests (25 unit + 7 CLI) pass;
  installer checks 14 pass; docs unit checks 15 pass; `cargo deny`
  bans/licenses/sources pass (one duplicate `winnow` version warning only);
  docs checker 101 Markdown files / 330 local links pass; strict
  `openspec validate` passes.
- `pkg shellenv` idempotence re-tested in real Bash and Zsh with double
  evaluation including spaces, quotes, `$`, a backtick, and a decoy PATH
  entry: pass.
- Nested exact catalog matching and the Git locked-`url` vs top-level-`url`
  decode bug: fixed and verified.
- Strict profile manifest version 3 checks before mutation: added, test
  passes.
- GitHub `hello` install, fixed-reference upgrade report, and remove: also
  verified on macOS.
- Outer Cask source advance `130cdf780475f271c1e738b66732daba0f4f4a8f` to
  `c878dee14e1899a508ad992119dc7f676acd1011`: `pkg update` changed no
  profile; `upgrade --all` advanced the locked revision while keeping the
  original moving `git+file?ref=main` reference; rollback restored the old
  revision; launchers stayed healthy.
- Helper interruption, real run: SIGINT only to the pkg pid while
  `mac-app-util` was running gave exit 1 with a clear interrupted message,
  the child was reaped, the `.pkg-owned` marker was restored, `pkg apps
  sync` retried successfully, and the package inventory was unchanged.
- Finder `open` verified for both apps on macOS.
- Reboot and fresh login passed (boot UUID `0A244C4E…` to `DC535371…`):
  doctor healthy, CLI versions work, launchers open, Spotlight returns both
  apps.

## Targeted exact `info` (final macOS archive, corrected code)

The exact-info lookup now asks Nix for the selected output
(`nix search <locked>#<attribute> .`) instead of evaluating the whole
catalog. Verified on the real final macOS archive: `pkg info
nixpkgs:hello` 1.51 s, nested `nixpkgs:python312Packages.pip` 1.10 s,
`info cask:cursor` reports the installed entry correctly, and `info
cask:zoom` reports the `native-installer` exclusion reason. `pkg doctor`
healthy. Underlying native warm targeted query: 0.21 s. After the change:
fmt, clippy, 32 tests, and docs compile pass.

These exact-info timings are a different operation from full-catalog
search. Full search stays at 72.42 s cold / 2.69 s warm on macOS, and the
Linux 2 GB sandbox full-search run still hits the 300 s limit; neither
limit applies to exact `info`, which never evaluates the whole catalog. The
Linux warm exact-info timing (0.190 s) and the unfinished Linux cold run are
recorded above with the same separation.

Final macOS client archive SHA256:
`9bdc6f267c9791a0238079ece9d4b67d6a89b6a1273ef386949bfd7dc9703f2e`.
The final Linux archive SHA256 is recorded in the Linux section above. Both
final archive passes (8.3, 8.5) are complete.

## Review

Standards and spec findings are tracked on separate axes. Ranks are not
merged between the axes.

### Standards review

Eight initial findings. The concrete findings 1-5 and 7-8 are fixed in
code and verified. F6 (a Dock pin left by the external helper after an app
is removed) is an upstream helper limit, documented as a known limit above;
pkg adds no Dock engine for it.

### Spec review

The fresh-profile finding was retracted after a real-runtime disproof:
`nix profile list --json` on a missing profile returns the empty manifest
shape. The shellenv idempotence finding was fixed and re-tested with real
Bash and Zsh double evaluation. The final spec review before the
targeted-info change found no outstanding spec findings. The
targeted-info change itself was reviewed for semantic consistency: the
locked source, the installed-identity comparison, the exclusion gating,
and the verified search-ID matching are unchanged; only the native query
is narrower. The final Linux archive pass closed the last open spec items
with no new findings.

## Harness note (not a product issue)

Moving the whole Homebrew root aside disabled the Tart guest agent at the
next boot. The manager restored only the
`/opt/homebrew/bin/tart-guest-agent` symlink. Brew stays absent. The product
never needed Brew at any point.

## Known limits (not open tasks)

- Full nixpkgs search on a memory-sized Linux host stays unmeasured; the
  300 s timeout on the 2 GB sandbox is recorded above. Task 3.5's bounded
  measurement requirement is fulfilled by the recorded controlled-source
  timings on both systems.
- The first Linux cold exact-info run did not return before the 30 s
  watchdog. Warm exact info is verified (0.190 s). A larger host is needed
  for a clean cold number.
- If the external helper leaves Dock pins behind after app removal, pkg
does not remove them. pkg has no Dock engine and will not grow one.
