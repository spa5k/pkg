# S6 determinate installer harness

This is a historical record. The executable experiment was removed.
Use the [original checkout](https://github.com/spa5k/pkg/tree/8fc06b3a0f57ecd189903c42ba5ca758c3913bbe/spikes/s6-determinate-installer) to reproduce it.

> **Warning:** `run.sh` performs a real privileged Nix installation. Run it only on a disposable clean test host or VM.

This harness executes one verified local Determinate installer asset. It does not download an installer.

Run it with one absolute path:

```sh
./run.sh /absolute/path/to/installer
```

`run.sh` accepts only the three targets in `assets.sha256`. It checks the local bytes before it calls `/usr/bin/sudo`. `stage.sh` then copies the asset into a new private root-owned directory, checks the copied bytes, records private evidence, and executes only the staged absolute path.

Run the local checks with:

```sh
./test-static.sh
```
