# Fixes for the open stress findings

Date: 2026-10-30.
Scope: the four open findings of [the update stress report](2026-10-29-update-stress.md)
and [the crash test report](2026-10-29-crash-tests.md).

All builds and runs used an E2B sandbox: Debian 12 x86_64, 2 vCPU, Rust
1.99.1, curl 7.88.1, Nix 2.35.2, and strace 6.1. This curl is older than 8.4,
so it shows finding 3.

## Fixes

| Finding | Fix |
| --- | --- |
| 1. A failed write to stdout or stderr panics with status 101. | `main` catches this one panic. pkg prints "pkg: cannot write the output: CAUSE" and exits with status 1. Other panics stay panics. |
| 2. Each death of a child by a signal is a cancellation. | Only SIGINT, SIGTERM, SIGHUP, and exit code 130 are cancellations. Another signal, for example SIGSEGV or SIGKILL from the kernel for low memory, is a failure: "terminated by signal: 11 (SIGSEGV)". |
| 3. Before curl 8.4, `--max-filesize` applies only to a declared length, so a server can fill the disk. | curl runs with a file size limit (`RLIMIT_FSIZE`) equal to the download limit. The kernel stops curl with SIGXFSZ, and pkg says "a file is larger than N bytes". The update and the channel package list use it. |
| 4. The update stops after 300 s, so a link below about 22 KiB/s cannot finish. | The limit is now one hour. The speed limit still stops a server that sends nothing for 60 s. |

With fix 2, a failed profile change after a crash of `nix` says "Failed" or
"Partial", not "Interrupted". pkg reads the profile again in both cases.

## Results

`tools/stress/update.py test hostile fault network --calls write --positions 24`:

| Case | Before | After |
| --- | --- | --- |
| `SHA256SUMS` of 400 MiB, no length | Refused after the download. | curl stopped at 64 KiB: "a file is larger than 65536 bytes". |
| Archive of 400 MiB, no length | The server sent 400 MiB, and curl wrote all of it. | curl stopped at 256 MiB: "a file is larger than 268435456 bytes". The server sent 264 MiB. |
| New client crashes | "the new client was cancelled by signal 11", status 139 | "the new client failed: terminated by signal: 11 (SIGSEGV)", status 1 |
| Archive that needs 400 s | Stopped at 300 s. The old client stays. | Updated in 402 s. |
| Failed `write` in the update | 9 panics | 48 trials, no panic. Each kept the old client. |

The other hostile and network cases gave the same results as before.

`tools/stress/lifecycle.py fault --positions 8 --seed 2` (profile changes with
real Nix):

| Case | Before | After |
| --- | --- | --- |
| Failed `write` during `install` or `remove` | 11 panics. One install was complete, but its last message panicked. | 11 trials, no panic. Each said "pkg: cannot write the output: No space left on device (os error 28)" and exited with status 1. In 2 trials the change was complete. Only the last message failed. |
| All 161 fault trials | 11 bad trials | No bad trial. 102 changes finished. 47 failed with a message and kept the profile. In 12, the dynamic loader could not read a library, before pkg started. |

## Run again

```sh
python3 tools/stress/update.py build
sudo PKG_STRESS_THROWAWAY=1 python3 tools/stress/update.py test hostile fault network --calls write
python3 tools/stress/lifecycle.py fault --pkg /var/tmp/pkg-stress/pkg-new --seed 2
```

Raw logs and trial results were downloaded to
`/tmp/pkg-findings-evidence-20261030/`.
