# Self-update stress, fault, and fuzz verification

Date: 2026-10-29.
Scope: the self-update path and the parsers that read data from outside pkg.
All builds and runs used E2B sandboxes: Debian 12 x86_64, 2 vCPU, 2 GB RAM,
Rust 1.99.1, and curl 7.88.1.

Three methods were used:

- `tools/stress/update.py` runs real `pkg update` processes against a local
  HTTPS release server. Each run is in a network namespace with only loopback.
  The test clients trust only a new test release key.
- `fuzz/` has eight `cargo-fuzz` targets. Each target ran for 600 seconds.
- `cargo-mutants` 27.1.0 changed the security checks one at a time and ran
  the tests against each change.

## Update stress results

The table shows the run after the fixes below: 1,343 trials, then 8 trials
for the rename and unlink calls. The stub `nix` fails, so each update exits
with status 1 after the client step. The harness checks the installed client
and the files left behind, not this status.

| Scenario | Trials | Result |
| --- | --- | --- |
| Normal update | 3 | The new client is installed. |
| SIGKILL at a random time | 300 | The old or the new client starts. 214 trials left download files. |
| Next update after SIGKILL | 300 | The new client starts. No files are left. |
| SIGINT at a random time | 150 | The old or the new client starts. No files are left. |
| Next update after SIGINT | 150 | The new client starts. No files are left. |
| SIGTERM at a random time | 90 | The old or the new client starts. 23 trials left download files. |
| Next update after SIGTERM | 90 | The new client starts. No files are left. |
| One failed system call | 244 | The old or the new client starts. 9 trials panic, see below. |
| Full install, completion, or temporary folder | 3 | The old client stays when the install fails. No files are left. |
| Six updates at the same time, five rounds | 30 | The new client starts. No files are left. |
| Hostile release server or new client | 16 | Each case fails with its own message. The old client stays. |

The failed system calls were `openat`, `read`, `write`, `fsync`, `renameat`,
`unlinkat`, `execve`, `clone`, `clone3`, `fchmod`, `mkdir`, `wait4`, `poll`,
`pipe2`, `readlink`, and `getrandom`. strace failed each call at up to 24
positions in each process.

### Hostile cases

| Case | Result |
| --- | --- |
| `SHA256SUMS` of 400 MiB, no length | Refused: larger than 65536 bytes. |
| `SHA256SUMS` of 400 MiB, with length | curl refuses it (error 63). |
| Server sends no data | curl stops after 60 seconds. |
| Redirect to `http://` | curl refuses it. |
| Connection reset during the archive | curl error 56. |
| Archive of 400 MiB, no length | Refused: larger than 268435456 bytes. |
| Archive checksum mismatch | Refused. |
| No signature file | Refused: HTTP 404. |
| Signature of another message | Refused. |
| Older release | No update. |
| Client of 300 MiB in the archive | Refused: larger than 268435456 bytes. |
| Client is a symbolic link | Refused: the archive has no client. |
| New client prints a wrong version with terminal controls | Refused. The controls are removed from the message. |
| New client does not stop | Stopped after 60 seconds. |
| New client crashes | Refused, but the message says "cancelled", see below. |
| New client fails one completion | Refused. |

## Fixes

The first run used the code of PR #131. No trial broke the installed client.
The run found these problems, which this branch fixes:

| Problem | Fix |
| --- | --- |
| A stopped update left a temporary folder and a 16 MB `.pkg-new-*` file in `~/.local/bin`. Later updates did not remove them. | Downloads go to `~/.local/share/pkg/update`. Each update first removes the files of a stopped update. |
| A `SHA256SUMS` of 400 MiB without a length was read into memory (405 MiB). | Lists and signatures are limited to 64 KiB, archives to 256 MiB. pkg checks the size after curl. |
| A server that sent no data held each download for up to 300 seconds. | curl stops below 1 byte per second for 60 seconds, and after 30 seconds without a connection. The channel list download uses the same limits. |
| A new client that did not stop held `pkg update` without limit. | Each run of the new client has a 60 second and 16 MiB output limit. |
| A read error of the signature file was reported as a malformed signature. | The update stops with the read error. |
| `U+061C` was not removed from terminal text. | It is removed with the other direction marks. |

New unit tests cover the IPv4 special ranges, the response body limit, the
exact commit check, the longest branch name, the signature namespace, and the
removal of stopped update files. The body limit and the commit check moved to
small functions, so the tests can reach them. Their behavior did not change.

## Mutation testing

The first pass covered `pkg-cli` and `cask-catalog` in four shards. It found
47 changes that no test detected. The security-related ones were in the
IPv4 range check, the response body limit, the commit check, and the signature
namespace check. The new tests above cover them. The other changes are in the
update steps, which the stress harness covers.

The second pass covered only the changed checks: 67 changes, 53 detected,
13 that do not compile, and 1 not detected. The remaining change at
`tools/cask-catalog/src/net.rs:73` has no effect, because the `0.0.0.0/8` and
`240.0.0.0/4` checks already refuse the same addresses.

## Fuzzing

Each target ran for 600 seconds with AddressSanitizer, two targets at a time.
No target found a crash, a wrong accept, or a timeout.

| Target | Inputs |
| --- | --- |
| `release_signature` | 17,897,978 |
| `release_sums` | 3,881,364 |
| `release_archive` | 1,025,950 |
| `channel_list` | 331,184 |
| `catalog_id` | 28,082,567 |
| `path_reference` | 20,655,191 |
| `tap_archive` | 1,108,647 |
| `tap_export` | 4,211,500 |

## Open findings

These findings need a decision. This branch does not change them.

1. A failed write to stdout or stderr that is not `EPIPE` makes pkg panic
   with "failed printing to stderr" and exit with status 101. All 9 bad
   fault trials are this case (`write` with `ENOSPC`). The client stays
   correct. The code has about 155 print macros.
2. `pkg_process` reports each death by a signal as a cancellation. A new
   client that crashes with SIGSEGV gives "the new client was cancelled by
   signal 11" and exit status 139. A process that the kernel stops for low
   memory gives the same message.
3. curl before 8.4 applies `--max-filesize` only to a declared length. A
   hostile server can then fill the disk until the speed or time limit stops
   curl. The size check after the download protects memory, not disk.
   The server must be GitHub or must have a certificate for it.

## Run again

```sh
python3 tools/stress/update.py build
sudo PKG_STRESS_THROWAWAY=1 python3 tools/stress/update.py test
```

Run `test` only on a throwaway Linux machine. It changes `/etc/hosts`, the
system CA store, and mounts. See [`fuzz/README.md`](../../fuzz/README.md) for
the fuzz targets.

Raw logs and trial results were downloaded to
`/tmp/pkg-stress-evidence-20261029/`.
