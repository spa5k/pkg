# Runtime evidence

Do not commit destructive-run evidence.

Pass an absent absolute output directory to `../run.sh`. The runner makes it
private, copies guest evidence, records the QEMU serial log, and removes the
disposable overlay after collection.
