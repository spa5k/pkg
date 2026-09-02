//! DN-1 PR-1 regression test: the `PKG_HERMETIC` tripwire.
//!
//! This lives in its own integration-test binary so the two-phase check
//! (armed -> panic, disarmed -> valid read) cannot race any other test in
//! the same process that reads `SystemClock`. One test, one binary, one
//! reader: the env mutation is race-free by construction.

use pkg_core::{Clock, SystemClock};

#[test]
fn hermetic_tripwire_panics_when_armed_and_reads_when_disarmed() {
    // Phase 1: armed. The ambient read must panic with the documented
    // instruction, and the panic must be catchable so the run reports a
    // test failure instead of aborting the harness.
    //
    // SAFETY: this is the only test in this binary and nothing else reads
    // `PKG_HERMETIC` concurrently; the env mutation is race-free here.
    unsafe { std::env::set_var("PKG_HERMETIC", "1") };
    let armed = std::panic::catch_unwind(|| SystemClock.now());
    let payload = armed.expect_err("PKG_HERMETIC=1 must forbid ambient SystemClock reads");
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    assert!(
        message.contains("inject a fixed clock"),
        "the tripwire message must name the remedy, got: {message}"
    );

    // Phase 2: disarmed. The same clock reads ambient time normally.
    // SAFETY: as above — sole reader in this binary.
    unsafe { std::env::remove_var("PKG_HERMETIC") };
    let read = SystemClock.now();
    assert!(
        read.as_millisecond() > 1_700_000_000_000_i64,
        "an ambient read must produce a plausible unix timestamp, got {read:?}"
    );
}
