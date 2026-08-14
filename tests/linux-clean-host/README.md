# Linux product install checkpoint

Run this destructive host test only in Docker:

```sh
tests/linux-clean-host/run.sh
```

The staging image builds the actual x86-64 `pkg-install` binary and an ephemeral
signed release. A separate final image receives only the staged, versioned
artifact, its checksum-pinned bootstrap, and the proof release. That clean host
has no source tree or compiler.

The proof uses the official Nix 2.34.8 archive and the public `pkg` CLI. It proves
bootstrap verification, install, retry, service isolation, cached package
installs, one approved local build, an authenticated channel update, a real
package upgrade, rollback, cached repair, uninstall, and safe absence. The
retained CI artifact uses test keys and is not a production release.
