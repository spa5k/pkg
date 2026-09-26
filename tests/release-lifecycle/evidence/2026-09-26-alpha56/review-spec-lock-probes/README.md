# Native file-lock lifetime diagnosis

These standalone probes reproduce the macOS File lock behavior used by StateLease. They do not modify repository files or product state. Run `python3 run.py` with the pinned Rust toolchain on macOS. The spawn probe is timing-sensitive: a run can observe zero conflicts. The controlled fork probe proves retained-description lifetime independently.

The initial review used temporary source/binary directories that were removed automatically. Its output was retained in the review conversation: 100,040 cycles, 1,009 shell launches, 29 post-drop conflicts, longest 224.417 microseconds. This directory retains a later run of the same algorithms, with separate output and machine-readable results. It must not be presented as a capture of the original CI process.

StateLease owns one File opened with O_CLOEXEC. Closing the parent handle does not release the kernel lock while a forked child still holds the same open-file description. Concurrent CLI shell setup tests can create this short interval. The two reported CI fixture paths drop their own lease before reacquisition. This diagnosis found no retained Rust lease and no evidence of state-integrity failure. The unchanged CI rerun and repeated CLI suites are separate evidence.
