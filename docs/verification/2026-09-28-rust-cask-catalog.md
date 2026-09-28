# Rust Cask catalog verification

Date: 28 September 2026. GLM 5.3 implemented the change in E2B. The parent reviewed the patches and repeated the final Rust, macOS, and Linux lifecycle checks.

## Sources and systems

- Base: `f5c227032964990a8cb6833f64472f9925777c85` on `origin/main`.
- macOS: Apple silicon Tart VM, macOS 15.7.7, Determinate Nix 3.22.1, Nix 2.35.2. Nix was already installed in the VM.
- Linux: x86_64 E2B sandbox, Determinate Nix 3.22.1, Nix 2.35.2. AppImage checks were headless.
- Rust: 1.96.1, with the committed Cargo lockfile.
- Nixpkgs: `567a49d1913ce81ac6e9582e3553dd90a955875f`.
- brew-nix reference: `16131ae4126c54b1502aa7eaf6573d7fbf16b656`. The repository was cloned locally. The port retains its MIT license.
- Cask input: brew-api revision `245947c0b920cbe83f4003bb7c6be737352c8314`, 17,146,512 bytes. SHA-256: `1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251`. The catalog retains the Homebrew Cask data attribution.

## Catalog coverage

The input contains 7,709 records. Both `catalogIndex` and `catalogStatus` evaluate with import-from-derivation disabled. These counts measure metadata eligibility. They do not mean that every package was built.

| Result or reason | aarch64-darwin | x86_64-linux |
| --- | ---: | ---: |
| Eligible | 3144 | 160 |
| Excluded total | 4565 | 7549 |
| `missing-checksum` | 2124 | 1606 |
| `deprecated` | 689 | 690 |
| `disabled` | 332 | 332 |
| `unsupported-platform` | 36 | 3941 |
| `unsupported-download-spec` | 600 | 524 |
| `unsupported-artifact` | 553 | 446 |
| `installer-script` | 141 | 4 |
| `malformed-record` | 38 | 3 |
| `unsupported-container` | 12 | 0 |
| `formula-dependency` | 11 | 3 |
| `cask-dependency-integration` | 29 | 0 |

The final review excluded twelve records with installer choices and one record with an incompatible naked-container plan. No token-specific rule was added. Regeneration from the same input is byte-identical.

## Rust and data checks

All 81 Rust tests passed: 35 generator library tests, one generator CLI test, 36 client library tests, and nine client CLI tests. Formatting, Clippy with warnings denied, documentation with warnings denied, dependency policy, local documentation links, and strict OpenSpec validation passed.

The client fixture contains 18 real projected index rows. Every row was compared with the final catalog. A null target version remains null. Versioned and plus-suffixed token attributes were also evaluated by Nix.

The regeneration workflow is wired. Its local generate-and-compare check passed. GitHub CI status is recorded on the pull request.

## Native client journeys

Both systems used isolated XDG config, state, and cache directories. The home directory was unchanged. Cask queries used the full generated catalog. The Nixpkgs query source was a small fixture flake, to keep the checks focused on this change.

| Check | macOS | Linux |
| --- | --- | --- |
| Search and info | Cursor and 1Password CLI | KOReader and 1Password CLI |
| Install and list | Cursor 3.17.19 and op 2.39.0 | KOReader 2026.07.1 and op 2.39.0 |
| Execute | `cursor --version`, `op --version` | `KOReader --help`, `op --version` |
| Update and upgrade | Passed | Passed |
| Rollback and remove | Passed; final profile empty | Passed; final profile empty |
| macOS launcher sync | Passed | Not applicable |

On each system, a controlled catalog copy changed only the op version label from `2.39.0` to `2.39.0-profile-probe`. The vendor URL and bytes stayed the same. `upgrade --all` selected a different store output and kept the original source URL. Rollback restored the prior output. This proves source re-resolution; it is not a claim about a vendor upgrade.

An early delegated Linux script reported zero after command errors. Its lifecycle results were discarded. The parent repeated the full journey with `set -euo pipefail`, the correct `KOReader` launcher name, `upgrade --all`, and exact installed IDs. This strict run passed.

The client refused Zoom with `installer-script`, Beutl with `formula-dependency`, Acronis True Image with `cask-dependency-integration`, and an unknown token as unknown. The profile remained empty after these refusals.

## Payload and builder checks

- Cursor 3.17.19 and Raycast 1.104.25 built on macOS. Both intact app bundles passed `codesign --verify --deep --strict`. Cursor's CLI ran. Cursor also started an app process. GUI interaction was not checked.
- The selected Raycast Sequoia variation requires macOS 13.0. It fits the 15.7.7 baseline. A separate fixture whose plist requires macOS 26.0 was refused with the required version in the error.
- The final Nix builder suite passed 13 checks. It covers app renames, anchored CLI links, completions, manpages, relocatable package payloads, script and system-payload rejection, static ELF, and dynamic ELF repair.
- The dynamic ELF fixture started with a host loader path and no rpath. The generic builder changed them to Nix store paths. The program ran against zlib 1.3.2. A second fixture required `libpkg-intentionally-missing.so`; the build failed and named it.
- Archive regression checks cover traversal, composed symlink escapes, `./` path aliases, cycles, hard links, ambiguous artifact paths, and the exact inert `/Applications` shortcut. Unsafe fixtures fail before payload writes. Ordinary framework link chains pass.
- A gzip payload package built on macOS. A second package was created with native `pkgbuild --compression latest --min-os-version 15.0`. Its payload began with `pbzx`. The same generic Nix builder extracted it; its resource bytes matched and its fixture executable ran. No vendor installer was invoked.

## Client archive isolation

`tools/release/package_client.sh` passed on both systems with the full workspace present. The macOS archive contains 49 client dependency license sets. Linux contains 50. Each archive has exactly one client binary plus completions, notices, and licenses. Neither contains the catalog generator, catalog JSON, or generator-only `sha2` and `digest` license sets. The extracted Linux client ran.

These are packaging checks, not a new published release. The release version was not changed.

## Limits

- Only the listed packages and fixtures were built. Catalog eligibility is not a compatibility promise.
- Linux GUI interaction was not checked. KOReader's headless help path was checked. macOS app process launch was checked, not visual use.
- Formula dependencies, cask dependencies, installer actions, fonts, services, and drivers remain outside this version's supported scope.
- Catalog updates are reviewed maintainer commits. There is no hosted service, scheduler, or automatic update bot.
- The catalog protocol changes without a compatibility layer. Existing alpha clients need the matching client update.
