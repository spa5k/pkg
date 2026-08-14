# Linux product install checkpoint

Run this destructive host test only in Docker:

```sh
tests/linux-clean-host/run.sh
```

The image starts without Nix. The test uses the actual `pkg-install` binary,
an ephemeral signed release, the official Nix 2.34.8 archive, and the public
`pkg` CLI. It proves install, retry, service isolation, cached package installs,
one approved local build, an authenticated channel update, a real package
upgrade, rollback, cached repair, uninstall, and safe absence.
