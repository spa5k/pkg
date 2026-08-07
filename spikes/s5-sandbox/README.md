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

`--privileged` grants broad control inside Docker Desktop's Linux VM. The
container mounts only this harness read-only plus the selected output directory;
it does not mount the Docker socket or the repository root.
