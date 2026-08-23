# Diagnostics contract for the macOS evidence rerun

## Decision

The rerun must set `DETSYS_IDS_TELEMETRY=disabled`.

The rerun must also pass this option to every state-changing installer invocation:

```text
--diagnostic-endpoint http://127.0.0.1:18080
```

No process listens on that address.

The environment variable is the diagnostics kill switch.

Every installer process inherits this variable.

The loopback endpoint is a second safety control.

The harness does not start a capture server.

The harness does not count diagnostic requests.

Two parse-only command shapes do not need the loopback option.

They are the staged installer `--version` probe and the unsupported-subcommand `--help` probes.

Clap exits during version or help parsing before the diagnostics builder path starts.

These processes still inherit `DETSYS_IDS_TELEMETRY=disabled`.

## Pinned source

This decision applies to Determinate Nix Installer 3.22.1.

The pinned installer commit is `4132ad07a15ee7d88c096ac7172b7afb2672866b`.

Its lock file selects `detsys-ids-client` 0.7.0.

The pinned client commit is `3d66088e42bb58f7f84acf4f4fb54417346bdd1b`.

The following links use complete commit IDs:

- The [installer CLI field](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/mod.rs#L68-L79) defines `--diagnostic-endpoint` and its installer environment variable.
- The [installer diagnostics builder](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/diagnostics.rs#L98-L154) sends the CLI value to the client builder.
- The [pinned installer lock file](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/Cargo.lock#L532-L559) records client version 0.7.0.
- The [telemetry environment switch](https://github.com/DeterminateSystems/detsys-ids-client/blob/3d66088e42bb58f7f84acf4f4fb54417346bdd1b/src/lib.rs#L30-L65) reads `DETSYS_IDS_TELEMETRY` and `DETSYS_IDS_TRANSPORT`.
- The [endpoint setter](https://github.com/DeterminateSystems/detsys-ids-client/blob/3d66088e42bb58f7f84acf4f4fb54417346bdd1b/src/builder.rs#L114-L122) replaces the transport value in the builder.
- The [`build_or_default` path](https://github.com/DeterminateSystems/detsys-ids-client/blob/3d66088e42bb58f7f84acf4f4fb54417346bdd1b/src/builder.rs#L198-L208) builds a transport with fallback behavior.
- The [client transport build](https://github.com/DeterminateSystems/detsys-ids-client/blob/3d66088e42bb58f7f84acf4f4fb54417346bdd1b/src/builder.rs#L252-L295) applies the telemetry switch and falls back to a default transport after a construction error.
- The [transport parser](https://github.com/DeterminateSystems/detsys-ids-client/blob/3d66088e42bb58f7f84acf4f4fb54417346bdd1b/src/transport/mod.rs#L58-L100) first parses a URL and then tries a file transport.
- The [empty file transport construction](https://github.com/DeterminateSystems/detsys-ids-client/blob/3d66088e42bb58f7f84acf4f4fb54417346bdd1b/src/transport/file.rs#L20-L49) tries to create the selected file.
- The [default transport](https://github.com/DeterminateSystems/detsys-ids-client/blob/3d66088e42bb58f7f84acf4f4fb54417346bdd1b/src/transport/mod.rs#L15-L74) selects the public Determinate service when no endpoint is set.

## What the official text says

The installer help text says that an empty endpoint disables diagnostics.

The [pinned CLI source](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/mod.rs#L68-L79) contains this statement.

The [pinned README diagnostics section](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/README.md#L453-L471) gives the same instruction.

The client implementation does not give the empty string a special disabled state.

This is a documentation and implementation conflict.

## What the implementation does

The order is important.

1. Clap reads `NIX_INSTALLER_DIAGNOSTIC_ENDPOINT` for the installer option.
2. An explicit `--diagnostic-endpoint` value has priority over that installer environment value.
3. The client `builder!` macro reads `DETSYS_IDS_TRANSPORT` as its ambient transport.
4. The installer then calls the client endpoint setter with the installer option.
5. That call replaces the ambient value even when the installer option is `None`.
6. `DETSYS_IDS_TELEMETRY=disabled` makes the client build without a reporting transport.

An empty installer endpoint is still a present value.

The endpoint setter receives the empty string.

The transport parser can treat a value that is not an HTTP URL as a file value.

An empty file value can fail during transport construction.

`build_or_default` handles that error by building a default client.

The default client can use the public Determinate endpoint.

Therefore, an empty endpoint is not a safe network control for this pinned implementation.

This result follows from the cited control flow.

It does not depend on an observed external request.

## Why the telemetry variable is the kill switch

The client checks `DETSYS_IDS_TELEMETRY` during its build.

The exact value `disabled` turns telemetry off.

This check occurs before the client gets a reporting transport.

The harness exports the value once at the start of every guest phase.

All install, repair, uninstall, crash, recovery, foreign, and upstream commands inherit it.

The harness does not set `DETSYS_IDS_TRANSPORT`.

This removes the ambiguous ambient transport path.

## Why the loopback endpoint remains useful

The explicit endpoint has the value `http://127.0.0.1:18080`.

The URL is valid.

No listener is started.

The normal telemetry-disabled path does not make a request.

If the telemetry environment value is lost while the pinned binary is used, the explicit endpoint still points to the guest itself.

A request then fails at the local connection.

It does not use the public default endpoint.

This makes the endpoint a fail-safe canary.

It is not a request-count test.

It is not proof that the process opened no socket.

## R3 evidence limit

The R3 evidence recorded the staged installer hash and the exact installer arguments.

The controlled listener recorded one unattributed loopback connection in its limited observation window.

That count does not prove that the connection contained a valid HTTP request.

It does not prove a method, path, or body.

It does not identify the source process.

It does not prove external egress to Determinate.

The result only described that one listener and that one window.

It did not prove that the installer could not select another transport.

It did not prove that the installer made no connection attempt.

It did not prove that any prior connection reached Determinate.

The new contract does not keep or extend that weak request-count evidence.

The source pin, staged hash, recorded arguments, and disabled telemetry environment are the useful controls.

## Exact rerun

Run the existing destructive macOS lifecycle lane again with the updated guest script.

Do not change the installer binary, Tart version, base image, capacity gates, receipt handling, or strict installed-state gate.

For the first install, use this order:

1. Run the installer.
2. Save its status immediately.
3. Record the `install-preassert` snapshot.
4. Run the non-fatal `determinate-nixd status` evidence probe.
5. Run the non-fatal Nix daemon store-ping evidence probe.
6. Apply the saved installer-status gate.
7. Apply the unchanged strict installed-state assertion.

This order preserves raw `/etc/fstab` evidence when the installer fails.

It also lets a successful install record the APFS `VolumeUUID` before the strict fstab check.

The rerun must record no receipt bytes.

The rerun must not start a diagnostics listener.

## Residual limit

This is a source-based control for the pinned client and installer.

It is not a packet capture.

A future installer can change the environment contract.

Any installer upgrade must repeat this source review before the pin changes.
