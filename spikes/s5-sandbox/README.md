# Spike S5 — managed-daemon sandbox evidence

This is the PR-8 evidence harness for DR-005. It is not production code and it
does not make DR-005 accepted by itself.

The Linux lane runs Nix 2.34.8 inside a disposable privileged container in
Docker Desktop's Linux VM. That is real Linux/Nix behavioral evidence, but it
is specifically LinuxKit/Docker evidence—not bare-metal Linux and not macOS
evidence.

Run:

```sh
./spikes/s5-sandbox/run-linux-docker.sh
```

That performs readiness checks only. The build probes require a deliberate,
single-run approval:

```sh
./spikes/s5-sandbox/run-linux-docker.sh --approve-build
```

The launcher creates `out/linux-docker/report.json` and `nix-daemon.log` with
mode `0600`. The readiness-only run is incomplete and leaves builds pending. The approved run
proves the regular-versus-fixed-output network distinction and observes Nix's
per-build cgroup without claiming resource caps.

Static platform configuration checks are separate and do not require Docker:

```sh
./spikes/s5-sandbox/test-static.sh
```

The real fail-closed negative control deliberately omits `--privileged` and
expects exit 69 plus an incomplete report:

```sh
./spikes/s5-sandbox/test-linux-unprivileged.sh
```

The native macOS lane requires the managed test installation, its dedicated
`pkg-nix-broker` account, and administrator authentication. Readiness is
non-building by default:

```sh
./spikes/s5-sandbox/run-native-macos.sh
```

The regular-network, fixed-output-network, and `_nixbld` build probes require
one explicit invocation approval:

```sh
./spikes/s5-sandbox/run-native-macos.sh --approve-build
```

That writes `out/native-macos/report.json` and `probe.log` with mode `0600`.
The observed host used an encrypted APFS `/nix` volume, Nix 2.34.8, the
dedicated broker-only socket boundary, and full Xcode 26.6. This is real native
macOS evidence, but the bootstrap still uses Nix's upstream launchd service
label rather than the planned product installer/service bundle.

`--privileged` grants broad control inside Docker Desktop's Linux VM. The
container mounts only this harness read-only plus the selected output directory;
it does not mount the Docker socket or the repository root.
