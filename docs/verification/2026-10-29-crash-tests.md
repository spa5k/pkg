# Power cut, lifecycle, and network fault verification

Date: 2026-10-29.
Scope: the self-update after a power cut, the profile changes (install,
remove, upgrade, rollback), and slow or failing release servers. This report
continues [the update stress report](2026-10-29-update-stress.md).

All builds and runs used E2B sandboxes: Debian 12 x86_64, kernel 6.1, 2 vCPU,
2 GB RAM, curl 7.88.1, Determinate Nix 3.22.5 (Nix 2.35.2), strace 6.1, and
LazyFS at commit `fa7d32e`.

## Methods

| Method | Tool | Status |
| --- | --- | --- |
| Power cut | `tools/stress/update.py test power`, with LazyFS | Done |
| Signals and failed system calls during profile changes | `tools/stress/lifecycle.py`, with real Nix | Done |
| Slow, late, and failing responses | `tools/stress/update.py test network` | Done |
| Nightly fuzzing | `.github/workflows/fuzz.yml` | Added |
| Exact replay with `rr record --chaos` | rr | Not possible: the VM has no hardware performance counters |
| Packet loss and delay with `tc netem` | tc | Not possible: the kernel has no `sch_netem` module |

LazyFS is a FUSE file system. It keeps written data in memory until an fsync.
The command `lazyfs::clear-cache` drops that data, like a power cut. LazyFS
keeps renames, new files, and other metadata changes. Thus it shows lost data,
but not a lost rename.

Without netem, the release server in `update.py` makes the network faults
itself: it waits before a response, sends at a fixed rate, closes early, or
returns an HTTP error.

## Power cut during the self-update

`~/.local` is on LazyFS. Each trial starts `pkg update`. One trial in three
waits until the update ends. The other trials stop the update with SIGKILL at a
random time. Then the trial drops all unsynced data, checks the client, and
runs the next update.

Before the fix, both "power cut after update" trials left three empty
completion files: `_pkg`, `pkg.bash`, and `pkg.fish`. pkg wrote them to a
temporary file and renamed it, but did not sync the data. The next update did
not write them again, because the client was already current.

After the fix, in two runs of 60 power cuts:

| Scenario | Trials | Result |
| --- | --- | --- |
| Power cut after the update | 40 | The new client starts. The completions are complete. |
| Next update | 40 | No files are left. |
| Power cut at a random time | 80 | The old or the new client starts. 74 trials left download files. |
| Next update | 80 | The new client starts. No files are left. |

The new client file was always complete, because pkg already syncs it before
the rename.

The second run also repeated the other parts with fewer trials: 60 signal
rounds, and faults in `openat`, `write`, `fsync`, `renameat`, and `unlinkat`
at up to 24 positions. It had 477 trials. The only bad trials were 9 panics
from finding 1. A failed sync of a completion file stops the update and keeps
the old client.

## Profile changes with real Nix

`lifecycle.py` uses a disposable state folder and a local flake with two
packages, `probe` and `spare`. Each trial starts one change and then checks
that `pkg list` works, that its entries match the commands in the profile, and
that the profile has the packages from before or after the change. When the
profile has the packages from before, the trial runs the same change again. It
must finish with the expected packages.

Final run (seed 2):

| Scenario | Trials | Result |
| --- | --- | --- |
| SIGKILL, SIGINT, or SIGTERM at a random time | 300 | 271 stopped a change in progress. Each profile was before or after the change, or, for a rollback, an older generation. All 188 repeated changes finished. |
| One failed system call in the pkg process | 161 | 102 changes finished. 48 failed with a message and kept the profile. 11 panicked, see finding 1. |
| Four equal changes at the same time | 10 | The lock kept one entry. The other commands said "Already installed" or "is not installed". |

A first run (seed 1) had 150 signal, 165 fault, and 10 parallel trials with the
same results. The failed calls were `openat`, `read`, `write`, `flock`,
`fsync`, `renameat2`, `unlinkat`, `mkdir`, `clone3`, `pipe2`, `wait4`, `poll`,
`readlink`, `getdents64`, and `statx`, at up to 8 positions each. The first run
also failed `getrandom` with `EAGAIN`. std then panics, but the kernel never
returns this error for the `GRND_INSECURE` flag that std uses. The final run
does not use it.

## Network faults

The archive was 6,592 KiB.

| Case | Time | Result |
| --- | --- | --- |
| `SHA256SUMS` 45 s after the request | 45.5 s | Updated. |
| Archive at 64 KiB/s | 103.8 s | Updated. |
| Archive that needs 400 s | 300.3 s | Stopped by the curl time limit. The old client stays. The message said "could not connect to the network", see the fixes. |
| Connection closes during the archive | 0.3 s | curl error 18. The old client stays. |
| HTTP 429 for `SHA256SUMS` | 0.2 s | curl error 22. The old client stays. |
| HTTP 503 for the archive | 0.3 s | curl error 22. The old client stays. |

## Fixes

| Problem | Fix |
| --- | --- |
| A power cut after an update left empty shell completions, and later updates did not repair them. | pkg syncs each completion file before the rename. |
| A download that curl stopped at its time limit gave "could not connect to the network". | curl messages with "bytes received" or "Operation too slow" give "the download was too slow and stopped". |
| A failed second `stat` of an XDG base folder gave "is not a folder". | One `stat` checks the folder. |
| Lock errors had no path, for example "pkg: Interrupted system call". | Lock errors name the lock file: "cannot lock …/pkg.pkg-lock: …". |

## Open findings

These findings need a decision. This branch does not change them.
[A later change fixes them](2026-10-30-open-findings.md).

1. A failed write to stdout or stderr panics with status 101. This report
   adds 11 cases in `install` and `remove`. In one case, the install was
   complete, but the final message panicked. The user sees a panic for a
   change that worked.
2. The update has a time limit of 300 s. With a 6.4 MiB archive, a connection
   below about 22 KiB/s can never finish an update. The speed limit (1 byte
   per second for 60 s) already stops a stalled server.

The open findings of the update stress report also stay open.

## Run again

```sh
# Power cut and network: a throwaway Linux machine with FUSE and LazyFS.
python3 tools/stress/update.py build
sudo PKG_STRESS_THROWAWAY=1 python3 tools/stress/update.py test power network --rounds 60

# Profile changes: Linux with Nix, strace, and a C compiler.
cargo build --release -p pkg-cli
python3 tools/stress/lifecycle.py --pkg target/release/pkg --rounds 300 --seed 2
```

Raw logs and trial results were downloaded to
`/tmp/pkg-crash-evidence-20261029/`.
