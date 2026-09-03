//! The injected wall-clock seam for the one expiry decision.
//!
//! A grounding audit of every ambient time read found the clock decides
//! exactly one security-relevant thing: channel descriptor freshness
//! (`expires_at <= now`, freeze-attack protection). Every other wall-clock
//! site is record-only and stays ambient. Production code therefore reads
//! the clock for that decision through the [`Clock`] seam, and hermetic
//! tests install a fixed clock so the verdict never depends on the ambient
//! system clock.

use jiff::Timestamp;

/// One source of wall-clock time.
///
/// Implementations return the current civil instant. The trait is object
/// safe, so owners store `Arc<dyn Clock>` and hermetic tests substitute a
/// fixed clock without changing production signatures.
pub trait Clock: Send + Sync {
    /// Returns the current instant.
    fn now(&self) -> Timestamp;
}

/// The production clock.
///
/// Reads the ambient system clock through `jiff`. With `PKG_HERMETIC` set,
/// every read panics: the tripwire proves that hermetic test runs never
/// reach an ambient wall-clock decision, so a new decision site cannot
/// appear unnoticed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        if hermetic_tripwire_armed(std::env::var_os("PKG_HERMETIC").as_deref()) {
            #[allow(
                clippy::panic,
                reason = "the hermetic tripwire must fail the run loudly"
            )]
            {
                panic!(
                    "PKG_HERMETIC forbids ambient SystemClock reads; \
                     inject a fixed clock at the wall-clock decision site"
                );
            }
        }
        // `Timestamp::now` reads `SystemTime` and refuses only an
        // out-of-range system clock, which no supported host can produce.
        Timestamp::now()
    }
}

/// Decides whether the hermetic tripwire is armed.
///
/// `PKG_HERMETIC` arms the tripwire unless it is empty or exactly `0`.
fn hermetic_tripwire_armed(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty() && value != std::ffi::OsStr::new("0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tripwire_arms_only_for_a_truthy_hermetic_setting() {
        let armed = |value: Option<&std::ffi::OsStr>| hermetic_tripwire_armed(value);
        assert!(!armed(None));
        assert!(!armed(Some(std::ffi::OsStr::new(""))));
        assert!(!armed(Some(std::ffi::OsStr::new("0"))));
        assert!(armed(Some(std::ffi::OsStr::new("1"))));
        assert!(armed(Some(std::ffi::OsStr::new("yes"))));
    }
}
