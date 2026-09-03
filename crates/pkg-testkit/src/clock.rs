//! The fixed clock test double for hermetic time control.

use std::sync::Mutex;
use std::time::Duration;

use jiff::Timestamp;
use pkg_core::Clock;

/// A [`Clock`] that always returns one caller-set instant.
///
/// Hermetic tests install a `FixedClock` wherever production constructors
/// install [`SystemClock`](pkg_core::SystemClock), so recorded timestamps,
/// audit rows, and expiry decisions never depend on the ambient system clock.
///
/// ```
/// use jiff::Timestamp;
/// use pkg_core::Clock;
/// use pkg_testkit::FixedClock;
///
/// let clock = FixedClock::new("2026-08-12T00:00:00Z".parse().unwrap());
/// assert_eq!(clock.now(), "2026-08-12T00:00:00Z".parse::<Timestamp>().unwrap());
/// ```
#[derive(Debug)]
pub struct FixedClock {
    instant: Mutex<Timestamp>,
}

impl FixedClock {
    /// Creates a clock frozen at `instant`.
    #[must_use]
    pub const fn new(instant: Timestamp) -> Self {
        Self {
            instant: Mutex::new(instant),
        }
    }

    /// Moves the frozen instant forward by one positive duration.
    ///
    /// # Panics
    /// Panics when the move leaves the supported timestamp range, which no
    /// bounded test duration can reach.
    pub fn advance(&self, duration: Duration) {
        #[allow(
            clippy::expect_used,
            reason = "no bounded test duration can leave the range"
        )]
        let next = self
            .instant
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .checked_add(duration)
            .expect("fixed-clock advance stays in range");
        *self
            .instant
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
    }

    /// Replaces the frozen instant.
    pub fn set(&self, instant: Timestamp) {
        *self
            .instant
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = instant;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        *self
            .instant
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "2026-08-12T00:00:00Z";

    #[test]
    fn fixed_clock_returns_the_set_instant_and_advances() {
        let clock = FixedClock::new(START.parse().unwrap());
        assert_eq!(clock.now().to_string(), START);
        clock.advance(Duration::from_secs(90));
        assert_eq!(clock.now().to_string(), "2026-08-12T00:01:30Z");
        clock.set("2026-09-01T12:00:00Z".parse().unwrap());
        assert_eq!(clock.now().to_string(), "2026-09-01T12:00:00Z");
    }
}
