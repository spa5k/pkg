# Runtime evidence

This is a historical record. The executable experiment was removed.
Use the [original checkout](https://github.com/spa5k/pkg/tree/8fc06b3a0f57ecd189903c42ba5ca758c3913bbe/spikes/s6-determinate-installer/linux-vm/evidence) to reproduce it.

Do not commit destructive-run evidence.

Pass an absent absolute output directory to `../run.sh`. The runner makes it
private, copies guest evidence, records the QEMU serial log, and removes the
disposable overlay after collection.
