# pkg realistic use verification

Date: 1 October 2026. Client: `0.2.0-alpha.10`.
The release tree matches checkout `1f4903788d595a15293a29a29bdb1abd229d6b9d`.
The release tag resolves to `10831885ce579bf28e3fc76998d151fe81548846`.
Their file trees are identical.

The client was installed from the public release with its real downloader.
The work used real package downloads and real commands.
Two E2B sandboxes ran Debian 12 on x86_64 Linux.
Each sandbox had 2 GB RAM and external Determinate Nix 3.22.5 / Nix 2.35.2.
No model keys were supplied to the sandboxes.
Both sandboxes were stopped after their evidence was saved.

Real use found six product or runtime faults. Concurrent installs lost a
successful change. Two macOS casks conflicted on their private runtime files.
Repeated cask installs added duplicate entries. Conflict recovery instructions
targeted the wrong profile. Actual could not see system fonts. macOS tap setup
changed a daemon file without loading its new environment.
Default rollback also has a native generation-history risk.
Full catalog search made a 2 GB Linux sandbox unresponsive.
Production source files were not changed.

The same public release was installed in a disposable macOS VM.
Both E2B sandboxes were stopped. The task-owned VM and downloaded base image
were removed after the evidence was collected.
The [evidence archive](realistic-use-2026-10-01.tar.gz) contains real command
results, reproduction scripts, app screenshots, and original synthetic data.

## Linux package use

| Package | Real use | Result |
| --- | --- | --- |
| ripgrep 15.2.0 | Search a file for a known line | Pass |
| jq 1.8.2 | Extract a value from JSON | Pass |
| fd 10.5.0 | Find the created file | Pass |
| hello | Run the installed program | Pass |
| Python 3.12.14 | Calculate a SHA-256 value and emit JSON | Pass |
| Git | Create a repository and inspect its state | Pass |
| 1Password CLI 2.39.0 | Start the vendor CLI and read its help | Pass; no account sign-in |
| Sentry CLI 3.6.2 | Start the raw vendor binary | Pass; no Sentry service use |
| GoReleaser 2.18.2 | Import a public tap, validate a config, build a Go program, run the result | Pass; no release publication |
| Node.js 24.21.0 | Serve a local HTTP request and return JSON | Pass |
| SQLite | Create a database, remove and restore the package, read the saved row | Pass |
| tree | Install from a fixed GitHub reference and inspect the project | Pass |
| Actual 26.8.1 | Open the desktop, create an account, remove and restore the app, reopen its data | Pass after a user-font workaround; normal first start failed |

Actual ran under Xvfb. Its normal launcher used no FUSE mount and no
`--no-sandbox` option. The account contained synthetic local data.
No bank account or external synchronization service was connected.
The original vendor download was checked against the catalog's SHA-256:
`7c6998c8420c163eabe0985ad01309476849d9c2a3a3d0c7c87658ef6ce3ace6`.

## Linux command journeys

- The real downloader verified and installed the release archive.
- A new Bash login shell found `pkg`, Nix, and installed programs.
- A new Zsh login shell found the same programs after real shell setup.
- Repeated installation reported an unchanged package set.
- A second client location with spaces worked with shell setup disabled.
- Client replacement left the package inventory unchanged.
- Temporarily removing the client left Nix and the installed programs usable.
- A failed Python 3.13 install preserved the existing Python 3.12 entry.
- A batch with one valid and one missing package preserved the inventory.
- macOS-only Obsidian and deprecated ChromeDriver were refused on Linux.
- `info` resolved a nested Nixpkgs package without a full search.
- `upgrade --all` retained fixed references and reported them.
- `update` changed no installed package entry.
- Removing hello and restoring it worked.
- Pruning with a positive age kept the current package set usable.
- Runtime discovery worked with `PATH=/usr/bin:/bin`.
- Native garbage collection removed 1,089 store paths and freed 1.0 GiB.
  Retained generations still restored hello. The running Node server still answered.

The public tap journey used `goreleaser/tap` at revision
`77e442b6000587bf57e543081e95fd908b918bd7`.
The real terminal setup prompt was answered in the disposable sandbox.
The import produced six eligible packages.
Tap update preserved installed packages.
A nonexistent revision failed and preserved the last working catalog.
Tap removal kept the installed GoReleaser binary usable.
Explicit upgrade of its removed-source entry was refused.
`upgrade --all` skipped that source and processed the remaining entries.
Re-adding the source worked.

## macOS package use

The disposable Apple silicon VM ran macOS 15.7.7, build 24G720.
It had 6 CPUs, 16 GB RAM, and a 50 GB APFS disk.
Tart 2.38.0 ran the VM.
The base image was pinned to:

```text
ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c
```

The public downloader installed pkg before Nix was present.
`doctor` reported the missing runtime.
The official Determinate installer then installed Determinate Nix 3.22.5
and Nix 2.35.2. The real pkg downloader was run again.
A fresh interactive Zsh shell found pkg, Nix, and installed programs.
The same check passed after a VM shutdown and a new boot.
The host's package profiles were not used for these package operations.
The base image already had Homebrew shell setup.

| Package | Real use | Result |
| --- | --- | --- |
| ripgrep 15.2.0 | Start the installed command | Pass |
| jq 1.8.2 | Read the value 42 from JSON | Pass |
| hello | Run from the installed profile and the graphical VM terminal | Pass |
| SQLite | Store a row, remove and restore the package, read the row | Pass |
| 1Password CLI 2.39.0 | Start the vendor CLI | Pass; no account sign-in |
| Stats 3.0.13 | Install, sync, start its process, remove an exact entry, restore | Commands passed; duplicate entries limited the removal check |
| iTerm2 3.6.11 | Install, sync, start its process | Pass for startup; shell scripting was blocked by an OS permission prompt |
| Obsidian 1.13.7 | Open its window, create a local vault, write a note, remove all app entries, restore generation 13 | Pass for use and saved data; final reopened window was not checked |
| Calibre 9.13.0 | Install in a separate profile, verify its signature, convert a text file to EPUB | Pass in its own profile; blocked beside Obsidian |
| Visual Studio Code 1.134.0 | Install its app and CLI in a separate profile, run `code --version` | Pass |
| Cursor 3.17.19 | Interrupt a real vendor build, check the empty profile, retry installation | Pass |
| GoReleaser 2.18.2 | Import the public tap, install, start its vendor CLI | Pass after manual config work and a VM restart |
| tree | Install while an unowned launcher folder exists, recover sync, run the command | Correct partial result and successful recovery |

The Calibre workflow used the actual bundled command:

```text
/nix/var/pkg/cask-apps/501/37x5xl41bd56csw6fgbz0m64z361j85l-cask-calibre-34fcbb13d1c4-9.13.0/calibre.app/Contents/MacOS/ebook-convert
```

It converted `original-book.txt` to `converted-book.epub`.
The EPUB has valid ZIP members and contains the original text.
The console records each successful command.
A later driver reused the Calibre JSON log file.
The evidence retains its console results, source script, original file,
and converted book, rather than a complete Calibre stdout record.
The packaged app does not expose `ebook-convert` in the profile's `bin`.
The bundled command was used for the successful conversion.

## macOS command journeys

- Missing app storage produced the documented partial result for Obsidian.
  The package entry existed. `pkg apps setup` and `pkg apps sync` recovered it.
- Repeating `apps setup` succeeded without a second setup change.
- Deep strict signature checks passed for Obsidian, Stats, and iTerm2.
  The separate Calibre signature check also passed.
- A valid package and a missing package in one batch left the inventory unchanged.
- The deprecated ChromeDriver cask was refused.
- Runtime discovery worked with `PATH=/usr/bin:/bin`.
- `upgrade --all`, history, and pruning completed.
- An unowned `~/Applications/pkg` folder retained its user file.
  Installing tree returned an explicit partial result after the profile changed.
  Restoring the owned folder and running `apps sync` recovered the launchers.
  Sync changed no package entry. Tree then ran successfully.
- Full `pkg search ripgrep` completed in 13.07 seconds on the first full query.
  A different query, `pkg search jq`, completed in 0.37 seconds with the cache.
  Both used the native catalog. No smaller search fixture was substituted.
- SIGINT was sent to pkg while Cursor's vendor-payload build was running.
  The client exited 130 and reported unchanged installed entries.
  The isolated profile remained empty. A retry installed Cursor successfully.
- The final interactive Zsh check found pkg and Nix, reported a healthy profile,
  printed hello's output, and started the 1Password CLI after the VM reboot.

An Obsidian note was created through the VM's graphical console.
Its text was: `this note was written in the app installed with pkg`.
Both installed Obsidian entry IDs were removed in one command.
The launcher disappeared and no Obsidian entry remained.
An explicit restore of the recorded generation returned both entries.
The note's SHA-256 remained unchanged throughout those operations.
The host Mac locked during the final screen check.
The report does not claim visual confirmation of the reopened note after restore.

Early driver mistakes were separated from product failures.
A noninteractive Zsh shell did not read `.zshrc`.
The first SQLite statement used double quotes for a text literal.
The corrected statement created the row before package removal.
The first interruption target was already in the Nix store.
Blender was excluded by the catalog before a download began.
Neither attempt was counted as successful cancellation.
The Cursor interruption was observed during a real uncached vendor build.
The Tart guest agent also lost its transport during large transfers.
SSH recovered access to the same VM and collected the evidence.

## Two signed apps conflict on their private helper

Priority: P1. Calibre cannot be installed beside Obsidian at normal priority.
The install fails before the profile changes.
Both packages provide different files at the same profile-relative path:

```text
libexec/pkg-cask-app
```

Each file launches a different store-local `libexec/pkg/cask-app.py`.
These package-private paths are merged into the user profile.
The private manifest and runtime paths can also collide.
The normal user command sequence is:

```sh
pkg install cask:obsidian
pkg apps setup
pkg apps sync
pkg install cask:calibre
```

The last command failed with a native file conflict.
Repeating it failed again.
Calibre installed and converted a book in a separate profile on the same VM.
This confirms a package-combination failure, rather than a bad Calibre download.
Private cask runtime files need paths that do not collide during profile merging.

Evidence: `macos/commands.json`, `macos/calibre-launcher-sources.tsv`,
`macos/calibre-workflow-console.log`, and `macos/converted-book.epub`.

## Repeated cask installs add duplicate entries

Priority: P2. Repeating Stats, iTerm2, and Obsidian installation added active
entries for the same source, attribute, and store output.
The client printed `Installed` again.
The main profile eventually held four Stats entries and two Obsidian entries.
There was one launcher per app, so the duplicate entries were not clear in
normal desktop use.

Reproduce with two casks from the official source:

```sh
pkg install cask:1password-cli
pkg install cask:stats
pkg install cask:stats
pkg list
```

Removal accepts exact native entry IDs.
Removing one duplicate entry keeps the other entry and the app active.
All matching IDs must be removed to remove that app from the profile.
The note lifecycle check removed `nix/casks-5` and `nix/casks-6` together.

A direct Nix control kept one entry when the profile held only Stats.
After adding 1Password CLI from the same flake, repeating that second cask
created a duplicate entry. It used the same native flags as pkg.
The native runtime therefore has this fault for a profile with multiple
attributes from one source. Pkg exposes that fault.
The client needs to check the complete installed set before adding a duplicate.

Evidence: `macos/native-main-final.json`,
`macos/native-cask-repeat.json`, `macos/native-cask-repeat-flags.json`,
`macos/native-multi-cask-repeat.json`, and
`macos/note-removal-restore-final.json`.

## Tap setup changes a daemon file without loading its new environment

Priority: P2. A real terminal tap import offered three one-time setup fixes.
The prompt was accepted in the disposable VM.
It appended the sandbox settings, changed the daemon plist, and ran
`launchctl kickstart`.
The client reported that the sandbox settings were verified.

The import still refused the effective `/private/tmp` sandbox grant.
A manual change set the permitted sandbox paths to the standard system paths.
The next import reached the mandatory build probe.
That probe could read the fresh host canary, so it correctly refused the build.
Tap Ruby was not imported under this unsafe build setup.

The plist contained `TMPDIR=/nix/var/nix/builds`.
The live `launchctl print` output did not contain that environment variable.
The setup's kickstart had restarted the service with its old loaded definition.
A full VM restart loaded the changed plist.
The live daemon then showed the new TMPDIR.
The same pinned GoReleaser tap import passed and produced six packages.
Its installed binary ran successfully.

The setup needs to load its changed service definition and verify the live
daemon environment. The effective host-path grants also need clear handling.
This finding concerns setup recovery. The safety probe correctly stopped
both failed imports.

Evidence: `macos/tap-terminal.log`, `macos/tap-before-manual-config.json`,
`macos/commands.json` under `add-real-tap`, `macos/daemon-plist.txt`,
`macos/daemon-before-reboot.txt`, `macos/daemon-after-reboot.txt`,
and `macos/tap-after-reboot.log`.

## Concurrent installs lose a successful change

Priority: P1. Both installs return exit status 0, but one requested package
is absent from the final native profile. The displayed result can also name
the other command's package.

Ten Linux attempts used fresh profiles and real hello and jq packages.
Nine lost one change. Five macOS attempts lost one change in every attempt.
All 30 `pkg install` commands returned success.
Three attempts with direct native Nix profile commands also lost a change.
The underlying Nix runtime therefore has the same fault.
The `pkg` client exposes it without protecting its profile operations.

Reproduce in a disposable account with an empty pkg profile:

```sh
ref=github:NixOS/nixpkgs/567a49d1913ce81ac6e9582e3553dd90a955875f
pkg install "$ref#hello" &
a=$!
pkg install "$ref#jq" &
b=$!
wait "$a"
wait "$b"
pkg list
```

This is a race. Repeat it with a fresh profile to reproduce it reliably.
One combined `pkg install ID1 ID2` completed correctly during normal use.
Concurrent install, remove, upgrade, and rollback operations need a shared
profile lock before this can be treated as reliable package management.
The other concurrent mutation combinations were not tested in this run.

Evidence: `linux/live/concurrency.json`, `macos/concurrency.json`,
and `linux/live/commands.json`
in the evidence archive.

## Conflict instructions change the wrong profile

Priority: P2. With Python 3.12 in the pkg profile, installing Python 3.13
fails on a file conflict. The output includes this instruction:

```text
To remove the existing package:
  nix profile remove python312
```

That command targets the user's default Nix profile.
It does not target the dedicated pkg profile.
The printed priority commands have the same missing profile argument.

A control installed Python 3.12 in the default Nix profile as well.
Following the printed instruction removed that default-profile package.
Python 3.12 remained in the pkg profile.
Repeating the requested Python 3.13 install still failed.
The message should provide a `pkg remove` command with the exact pkg entry ID,
or include the explicit profile path in every native recovery command.

Evidence: `linux/python-conflict-repeated.log`,
`linux/wrong-profile-instruction.log`, `linux/default-profile-before.json`,
`linux/default-profile-after.json`, `linux/follow-conflict-instruction.log`,
and `linux/conflict-after-printed-instruction.log`.

## Actual cannot see system fonts

Priority: P2. Actual installed successfully and exposed `bin/Actual`.
Normal startup created a blank window.
Its renderer stopped with:

```text
SkFontMgr_FontConfigInterface.cpp:163] Not implemented.
```

DejaVu and URW fonts were present under `/usr/share/fonts`.
Pointing fontconfig at `/etc/fonts/fonts.conf` did not fix the packaged app.
The original vendor AppImage was extracted and started outside the Nix
FHS environment, in a fresh home without user fonts.
It rendered the interface correctly on the same X server setup.

Copying one DejaVu font to `~/.local/share/fonts` fixed the pkg-installed app.
The app then created a local account and reopened it after removal and an
explicit generation restore. The comparison points to missing system-font access in the package's FHS
environment. The shared AppImage builder needs a usable font path or a font
package in its runtime environment.

This proves one real AppImage failure.
It does not prove that every AppImage has the same failure.
The desktop had no hardware GPU. GPU initialization warnings also occurred
in this virtual display and were not counted as a pkg defect.
The vendor control also emitted its initial server-configuration warning.
That warning was not attributed to pkg.

Evidence: `linux/live/actual-gui.log`,
`linux/live/actual-gui-fontconfig.log`,
`linux/live/actual-gui-user-font.log`,
`linux/live/actual-vendor-gui.log`, and the screenshots in `linux/live/`.

## Default rollback follows old generation numbers

This is native Nix behavior exposed by pkg.
It is a practical recovery risk, rather than a change from the accepted
native-profile design.

The following real sequence reproduces it:

```sh
pkg install nixpkgs:hello
pkg install nixpkgs:jq
pkg remove jq
pkg rollback
pkg remove hello
pkg rollback
```

Before the second removal, both packages are installed.
The final default rollback restores hello but loses jq.
It selects generation 3, which had removed jq, rather than generation 2,
which was active immediately before the second removal.

Use `pkg history` and `pkg rollback N` to select the intended package set.
An explicit restore was used for the Actual app and preserved its data.
The command documentation should explain this history behavior.

Evidence: `linux/rollback-branch/commands.json` and
`linux/rollback-generation-detail.log`.

## Full search exhausts a small sandbox

The real default command `pkg search ripgrep` started the full native
Nixpkgs catalog query. It produced no result before the sandbox became
unresponsive to new shell commands.
The Nix process reached approximately 1.7 GiB resident memory.
Only about 43 MB remained available in the 2 GB sandbox.

The remote kill attempts also timed out under that load.
The saved final log reports that the native search ended with signal 9.
The evidence does not establish whether the requested kill or the OOM
killer delivered that signal. The sandbox was then stopped.
No successful cold-search timing is claimed.
Exact qualified `info` and installation commands worked on this host.
This extends the known small-host search limit recorded in the earlier
native Nix verification report.

Evidence: `linux-cold-search.log`, `linux-search-meminfo.txt`, and
`linux-search-nix-status.txt`.

## Scope of the results

These are real release-install and package-use results.
The work covers x86_64 Debian 12 and aarch64 macOS 15.7.7.
It does not cover every catalog entry, Linux distribution, macOS version,
hardware device, or authenticated vendor service.
No release, package, website, or external message was published.
The early unit and synthetic checks are supplemental evidence only.
The user-facing conclusions above come from live commands and app use.

The first driver checks also exercised the release build and the existing
required repository checks in E2B. They passed.
Those checks and synthetic catalog-plan replay are supplemental.
They are not the basis for the six live-use findings.

No product fix was applied in this task. The client should not be treated as
ready for concurrent profile mutations or for arbitrary combinations of
externally signed macOS casks until the two P1 findings are corrected.
