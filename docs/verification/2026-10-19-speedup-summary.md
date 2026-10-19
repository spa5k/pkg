# pkg speed record — 2026-10-19

Compares `feat/pkg-speedup` with `origin/main` at `c4c9f66`.
All times are median end-to-end milliseconds unless stated. Startup and output are included.
The machine-readable companion contains every retained median, identity, verdict, and limit.
Raw per-sample scratch files were intentionally removed; the benchmark tools remain in `tools/`.

## Principal gains

| Workflow | Mac before → after | Linux before → after |
| --- | ---: | ---: |
| Full installer and fresh full preparation | 331.239 s → 240.785 s | 436.933 s → 300.716 s |
| Final full preparation | 288.450 s → 191.612 s | 354.472 s → 252.835 s |
| Cold Formula metadata | 91.181 s → 9.405 s | 111.494 s → 11.038 s |
| Full native catalog setup | 265.304 s → 176.526 s | 359.321 s → 251.601 s |
| Repeat 100 installed packages | 16.849 s → 0.341 s | 11.136 s → 0.157 s |
| Repeat 1,000 installed packages | 174.769 s → 0.909 s | grouped control; see JSON |
| Real native install | 0.520 s → 0.308 s | 0.186 s → 0.132 s |
| Dense real search | 0.869 s → 0.464 s | 0.786 s → 0.427 s |
| Dense 5M-row search | 79.206 s → 43.496 s | 66.497 s → 43.516 s |

The Linux 1,000-package repeat covers all 1,000 packages in ten 100-package groups on the 2 GiB host.
The Mac repeat-all command improves from 174.769 s to 0.909 s.

## Final command matrix

46 cases per host use 16 balanced pairs after one excluded pair.

| Case | Mac | Linux |
| --- | ---: | ---: |
| `install_warm` | 519.750 → 307.532 ms | 186.491 → 132.175 ms |
| `upgrade_changed` | 374.347 → 249.321 ms | 139.439 → 114.704 ms |
| `rollback_previous` | 492.947 → 291.514 ms | 395.053 → 135.365 ms |
| `rollback_explicit` | 413.536 → 215.391 ms | 174.494 → 103.224 ms |
| `remove_warm` | 356.972 → 217.540 ms | 117.666 → 103.516 ms |
| `list_100` | 138.226 → 72.739 ms | 60.867 → 38.498 ms |
| `setup_warm` | 411.958 → 334.827 ms | 197.935 → 157.619 ms |
| `update_warm` | 443.303 → 364.967 ms | 196.817 → 153.543 ms |
| `search_all_sources` | 27.723 → 21.977 ms | 25.036 → 18.636 ms |

Small startup commands remain mixed. Slower medians remain in the JSON.

## Search and scale

The real native catalog is actually published by the final client and queried with an empty PATH.
Output bytes, totals, and complete database hashes match.

| Case | Mac | Linux |
| --- | ---: | ---: |
| `literal` | 232.704 → 125.463 ms | 209.974 → 124.506 ms |
| `literal_no_fuzzy` | 163.911 → 89.699 ms | 175.585 → 104.215 ms |
| `typo` | 206.803 → 108.975 ms | 210.800 → 130.666 ms |
| `broad` | 220.794 → 131.909 ms | 221.130 → 145.869 ms |
| `regex` | 159.964 → 87.149 ms | 128.925 → 76.782 ms |
| `dense` | 869.165 → 464.488 ms | 786.354 → 426.752 ms |
| `unicode` | 193.774 → 97.172 ms | 172.928 → 109.499 ms |

All-source dense output improves from 1.238 s to 0.740 s on Mac and 1.195 s to 0.683 s on Linux.
Larger catalogs use exact output hashes, immutable inputs, and separate resource passes.
The 10M comparison uses the selected ID-buffer client as baseline.
The Linux 10M recovery covers literal limited output; complete full-output coverage is at 5M.

## Installer, packages, profiles, apps, and taps

| Area | Result |
| --- | --- |
| Linux installer `client_only` | 756.231 → 317.443 ms |
| Linux installer `bash_paths` | 676.721 → 293.468 ms |
| Linux installer `zsh_paths` | 731.559 → 302.158 ms |
| Linux installer `custom_paths` | 807.668 → 326.949 ms |
| Linux installer `repeat_bash_paths` | 950.990 → 344.250 ms |
| Linux full preparation | 354,472.406 → 252,834.750 ms |
| Mac real ten-package repeat | 7.066 s → 1.117 s |
| Mac Caffeine DMG | 2.799 s → 2.193 s |
| Mac AltTab ZIP | 2.784 s → 2.175 s |
| Mac private signed Calibre | 4.565 s → 3.802 s |
| Mac public tap install | 1,042.646 → 918.043 ms |

The Mac tap payload checks executable version, compressed manpage, three completions, metadata, update, upgrade, removal, and cleanup.
One final baseline tap update returned GitHub HTTP 403 and is retained as a limit, not a speed result.
The complete Linux public lifecycle already covers revocation and expected refusals.

## Verification

- Full Rust format, Clippy with warnings denied, all-target build, all-feature tests, private docs, and normal release pass on both hosts.
- Native lifecycle passes 118 Mac and 101 Linux commands.
- Mac passes 780 exact search comparisons and 20 repeated-install behavior checks. Linux passes the retained 260-output exact matrix and 20 behavior checks.
- Installer syntax, ShellCheck, dependency policy, Formula/Cask checks, native discovery boundaries, docs links, and benchmark lint/format pass.
- The final 100 source and test hashes match across hosts.
- PR #120 required CI passed on Linux and macOS.

## Limits

- Raw per-sample evidence files were intentionally removed from Git. Use the retained benchmark tools to reproduce samples.
- Installer transport uses local HTTP; public CDN latency and external Nix provisioning are excluded.
- Repeated package tests use a warm shared Nix store; first fetches are excluded observations.
- The 10M comparison uses the selected ID-buffer client as baseline, not origin/main.
- The Linux 10M recovery covers the literal limited-output case; the complete 5M matrix covers full dense output.
- The Linux 1,000-package repeat uses ten 100-package groups because one 1000-argument command exceeded the 2 GiB host limit.
- The Mac tap payload measurement uses three balanced install pairs and two pairs for other cases after a retained HTTP 403; no general tap speed gain is claimed.
- Formula batches increase peak memory from 244 to 353 MiB on Mac and 303 to 419 MiB on Linux; Nix daemon memory is excluded.
- Synthetic catalogs can exceed page cache; no fully resident-memory claim is made.
- Vendor GUIs, fresh privileged Mac app storage, and public CDN latency are not timed.

[Machine-readable complete medians, identities, controls, and verdicts](2026-10-19-speedup-summary.json).
