//! Spike S4 (PR-6 / DR-004) — CAPS slice: a bounded, streaming byte
//! collector/writer that retains at most a NONZERO caller cap, keeps draining
//! (counting, discarding) bytes AFTER the cap is reached, and reports the TOTAL
//! bytes seen with overflow-safe (saturating) accounting plus an EXPLICIT flag
//! that records whether the saturating total itself hit its ceiling.
//!
//! This is the primitive the command slice will use to enforce the manifest's
//! per-stream child-output caps. A cap exists so a runaway child cannot
//! unboundedly fill memory or stall on a full pipe: once the cap is hit the
//! collector stops RETAINING bytes but KEEPS READING them (drain/discard) so the
//! producer can never block on a full buffer and a partial capture is still
//! available for diagnostics. The retained buffer is the ONLY allocation and is
//! never larger than the cap, so discarded overflow never costs memory.
//!
//! Counting is wrap-free: every push adds to a [`u64`] total via
//! [`u64::checked_add`] (saturating to [`u64::MAX`] on overflow), and a
//! `total_saturated` flag is latched the moment that saturation occurs, so a
//! consumer can always tell a genuine `total == u64::MAX` from a saturated
//! lower bound. All reported values are pure functions of `(cap, bytes pushed)`,
//! so the same feed always reports identically regardless of how it was chunked.
//!
//! Pure, allocation-only, no I/O of its own (it does implement [`std::io::Write`]
//! so it can be used directly as a capture sink), no timing, no threads.
//! `#![forbid(unsafe_code)]` is inherited from the crate root.

use std::fmt;
use std::io;
use std::num::NonZeroU64;

/// A streaming byte collector that retains at most a nonzero caller cap and
/// keeps a saturating count of EVERY byte pushed, continuing to drain (discard)
/// past the cap so the producer never blocks on a full buffer.
///
/// The cap is required to be nonzero at construction ([`ByteCap::new`] takes a
/// [`NonZeroU64`]); a zero cap is a configuration error and is rejected by type.
#[derive(Debug, Clone)]
pub struct ByteCap {
    /// Maximum bytes retained. Guaranteed `> 0` (enforced by the constructor).
    cap: u64,
    /// The retained prefix of the stream; `len() <= cap` always.
    retained: Vec<u8>,
    /// Saturating total of ALL bytes ever pushed (retained + discarded). Never
    /// wraps; saturates at [`u64::MAX`].
    total: u64,
    /// Latched `true` the first time the saturating total reaches [`u64::MAX`],
    /// so a consumer can distinguish "exactly u64::MAX bytes" from "at least
    /// u64::MAX bytes". Once latched it never clears.
    total_saturated: bool,
    /// Latched `true` the first time any byte is discarded (real total exceeded
    /// the cap). Independent of `total_saturated`.
    cap_exceeded: bool,
}

/// A deterministic snapshot of a [`ByteCap`]'s accounting. Every field is a pure
/// function of `(cap, bytes pushed)`, so two collectors fed identical bytes —
/// even split at different boundaries — report identical values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// The configured retention cap in bytes (nonzero).
    pub cap: u64,
    /// Bytes actually retained (`<= cap`).
    pub retained: u64,
    /// Saturating total of all bytes ever pushed (`<= u64::MAX`).
    pub total: u64,
    /// `true` iff the saturating total hit [`u64::MAX`] (reported `total` is a
    /// lower bound, not exact).
    pub total_saturated: bool,
    /// `total - retained`, saturating-safe ([`u64::saturating_sub`]).
    pub discarded: u64,
    /// `true` iff more bytes were pushed than the cap allows.
    pub cap_exceeded: bool,
}

impl ByteCap {
    /// Create a collector with the given NONZERO byte cap. The [`NonZeroU64`
    /// argument makes the "nonzero caller cap" requirement unrepresentable to
    /// violate: construct it with `NonZeroU64::new(n).expect("cap > 0")` (or
    /// any other validated `u64` source).
    #[must_use]
    pub fn new(cap: NonZeroU64) -> Self {
        ByteCap {
            cap: cap.get(),
            retained: Vec::new(),
            total: 0,
            total_saturated: false,
            cap_exceeded: false,
        }
    }

    /// The configured retention cap (always `> 0`).
    #[must_use]
    pub fn cap(&self) -> u64 {
        self.cap
    }

    /// Number of bytes currently retained (`<= cap`).
    #[must_use]
    pub fn retained_len(&self) -> usize {
        self.retained.len()
    }

    /// Saturating total of every byte ever pushed (retained + discarded).
    #[must_use]
    pub fn total(&self) -> u64 {
        self.total
    }

    /// `true` iff the saturating total reached [`u64::MAX`] (so [`total`][Self::total]
    /// is a lower bound rather than an exact count).
    #[must_use]
    pub fn total_saturated(&self) -> bool {
        self.total_saturated
    }

    /// `true` iff more bytes were pushed than the cap allows.
    #[must_use]
    pub fn is_overflow(&self) -> bool {
        self.cap_exceeded
    }

    /// The retained prefix, as a byte slice (`len <= cap`).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.retained
    }

    /// Consume the collector and return the retained prefix.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.retained
    }

    /// Deterministic overflow/total accounting snapshot.
    #[must_use]
    pub fn stats(&self) -> Stats {
        // `retained.len() <= cap <= total` (total is the saturating count of
        // every byte, retained is a subset), so `saturating_sub` never
        // underflows in practice; it is used purely defensively.
        let retained = self.retained.len() as u64;
        let discarded = self.total.saturating_sub(retained);
        Stats {
            cap: self.cap,
            retained,
            total: self.total,
            total_saturated: self.total_saturated,
            discarded,
            cap_exceeded: self.cap_exceeded,
        }
    }

    /// Push a chunk: retain up to the remaining cap, count EVERY byte
    /// (saturating, with saturation flag), and silently discard the rest.
    /// Always "consumes" the whole chunk from the cap's perspective (counts it
    /// all, drains it all) so a reader loop can keep the upstream pipe empty
    /// even after overflow.
    ///
    /// Splitting the same logical feed at arbitrary boundaries yields identical
    /// `retained`/`total`/`stats` — only the count matters, never the chunking.
    pub fn push(&mut self, data: &[u8]) {
        let n = data.len();

        // Wrap-free total with explicit saturation flag.
        let (new_total, sat) = add_saturating(self.total, self.total_saturated, n as u64);
        self.total = new_total;
        self.total_saturated = sat;

        // Retain only while room remains. `cap` is a `u64` (nonzero); clamp to
        // `usize` without panicking on narrow targets (this harness is 64-bit,
        // so the clamp is a no-op in practice).
        let cap_usize = cap_to_usize(self.cap);
        let room = cap_usize.saturating_sub(self.retained.len());
        let take = room.min(n);
        if take > 0 {
            // Bounds: take <= room <= cap - retained.len(), and take <= n.
            self.retained.extend_from_slice(&data[..take]);
        }
        // If any byte of this chunk was not retained, the real total has
        // exceeded the cap (now or earlier) — latch once and keep.
        if take < n {
            self.cap_exceeded = true;
        }
    }
}

impl io::Write for ByteCap {
    /// Push the slice and report it as fully accepted (it is always fully
    /// drained/counted), so a [`ByteCap`] never errors or short-writes and can
    /// be used directly as a capture sink.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.push(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stats {
    /// `true` iff the cap was exceeded (bytes were discarded).
    #[must_use]
    pub fn is_overflow(&self) -> bool {
        self.cap_exceeded
    }
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cap={} retained={} total={} total_saturated={} discarded={} cap_exceeded={}",
            self.cap,
            self.retained,
            self.total,
            self.total_saturated,
            self.discarded,
            self.cap_exceeded,
        )
    }
}

/// Add `n` to `acc` without wrapping: returns the saturating sum and a flag that
/// is `true` iff the real sum exceeded [`u64::MAX`] (including when `saturated`
/// was already `true`). This is the "total overflow" helper — pure, total, and
/// directly unit-tested at the arithmetic boundary (see `tests::total_overflow_*`).
fn add_saturating(acc: u64, saturated: bool, n: u64) -> (u64, bool) {
    if saturated {
        return (u64::MAX, true);
    }
    match acc.checked_add(n) {
        Some(sum) => (sum, false),
        None => (u64::MAX, true),
    }
}

/// Clamp a `u64` cap into `usize` without panicking on narrow targets. On 64-bit
/// hosts `usize::MAX == u64::MAX`, so this is the identity for every real cap.
fn cap_to_usize(cap: u64) -> usize {
    if cap > usize::MAX as u64 {
        usize::MAX
    } else {
        cap as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Test helper: build a nonzero cap value, panicking if `n == 0` (which
    /// would be a bug in the test, not the unit under test).
    fn nz(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).unwrap_or_else(|| panic!("test cap must be nonzero, got {n}"))
    }

    // --- exact cap ------------------------------------------------------------

    #[test]
    fn exact_cap_retains_all_and_is_not_overflow() {
        let mut c = ByteCap::new(nz(5));
        c.push(b"abcde");
        assert_eq!(c.retained_len(), 5);
        assert_eq!(c.total(), 5);
        assert!(!c.total_saturated());
        assert!(!c.is_overflow());
        assert_eq!(c.as_bytes(), b"abcde");
        let s = c.stats();
        assert_eq!(
            s,
            Stats {
                cap: 5,
                retained: 5,
                total: 5,
                total_saturated: false,
                discarded: 0,
                cap_exceeded: false,
            }
        );
    }

    #[test]
    fn one_byte_over_cap_discards_exactly_one() {
        let mut c = ByteCap::new(nz(5));
        c.push(b"abcdef"); // 6 bytes
        assert_eq!(c.retained_len(), 5);
        assert_eq!(c.as_bytes(), b"abcde");
        assert_eq!(c.total(), 6);
        assert!(c.is_overflow());
        let s = c.stats();
        assert_eq!(s.discarded, 1);
        assert!(s.cap_exceeded);
        assert!(!s.total_saturated);
    }

    #[test]
    fn single_push_much_larger_than_cap_keeps_prefix_only() {
        let mut c = ByteCap::new(nz(4));
        c.push(b"abcdefgh"); // 8 bytes, cap 4
        assert_eq!(c.as_bytes(), b"abcd");
        assert_eq!(c.total(), 8);
        assert_eq!(c.stats().discarded, 4);
        assert!(c.is_overflow());
    }

    // --- continues draining after overflow -----------------------------------

    #[test]
    fn keeps_draining_and_counting_after_overflow() {
        let mut c = ByteCap::new(nz(4));
        c.push(b"abcdefgh"); // total 8 (overflow at 4)
        c.push(b"IJKL"); // total 12 — must keep counting, not stop
        c.push(b"MNOP"); // total 16
        assert_eq!(c.retained_len(), 4);
        assert_eq!(c.as_bytes(), b"abcd");
        assert_eq!(c.total(), 16);
        assert_eq!(c.stats().discarded, 12);
        assert!(c.is_overflow());
        assert!(!c.total_saturated());
    }

    // --- writes split at arbitrary boundaries --------------------------------

    #[test]
    fn split_invariance_same_feed_any_chunking() {
        // Reference: one push.
        let feed = b"abcdefghijklmnopqrstuvwxyz0123456789"; // 36 bytes
        let cap = nz(10);
        let mut reference = ByteCap::new(cap);
        reference.push(feed);

        // A zoo of alternative chunkings of the SAME feed.
        let chunkings: Vec<Vec<&[u8]>> = vec![
            vec![&feed[..]],                                              // single
            vec![&feed[..18], &feed[18..]],                               // two halves
            vec![&feed[..1], &feed[1..2], &feed[2..]],                    // 1,1,rest
            (0..feed.len()).map(|i| &feed[i..i + 1]).collect(),           // byte-by-byte
            vec![&feed[..10], &feed[10..20], &feed[20..30], &feed[30..]], // cap-aligned
            vec![&feed[..7], &feed[7..33], &feed[33..]],                  // awkward seams
            {
                // odd-sized 3-byte chunks plus a tail
                let mut v = Vec::new();
                let mut i = 0;
                while i < feed.len() {
                    let j = (i + 3).min(feed.len());
                    v.push(&feed[i..j]);
                    i = j;
                }
                v
            },
        ];

        for chunks in &chunkings {
            let mut c = ByteCap::new(cap);
            for ch in chunks {
                c.push(ch);
            }
            assert_eq!(c.as_bytes(), reference.as_bytes(), "retained differs");
            assert_eq!(c.total(), reference.total(), "total differs");
            assert_eq!(c.stats(), reference.stats(), "stats differ");
        }
        // Sanity: the reference itself overflowed and retained exactly the cap.
        assert_eq!(reference.retained_len(), 10);
        assert_eq!(reference.as_bytes(), b"abcdefghij");
        assert_eq!(reference.total(), 36);
        assert!(reference.is_overflow());
        assert_eq!(reference.stats().discarded, 26);
    }

    // --- large repeated writes / no proportional allocation -------------------

    #[test]
    fn large_repeated_writes_stay_bounded_and_exact() {
        // Cap is tiny; the feed is large and repeated. The retained buffer must
        // never exceed the cap (the ONLY allocation), while the total counts
        // every byte. This is the structural proof that discarded overflow costs
        // no memory proportional to the discarded amount.
        let mut c = ByteCap::new(nz(64));
        let big = vec![0xA5u8; 100_000]; // ~100 KiB
        for _ in 0..10 {
            c.push(&big);
        }
        assert_eq!(c.retained_len(), 64); // exactly the cap, never more
        assert_eq!(c.total(), 1_000_000);
        assert_eq!(c.stats().discarded, 1_000_000 - 64);
        assert!(c.is_overflow());
        assert!(!c.total_saturated());
        // The retained prefix is the first 64 bytes of the feed.
        assert_eq!(c.as_bytes(), &big[..64]);
    }

    #[test]
    fn empty_pushes_are_noops() {
        let mut c = ByteCap::new(nz(4));
        c.push(b"");
        c.push(b"");
        assert_eq!(c.retained_len(), 0);
        assert_eq!(c.total(), 0);
        assert!(!c.is_overflow());
    }

    #[test]
    fn retained_is_a_strict_prefix_never_beyond_cap() {
        // Cross the cap mid-chunk, mid-feed, repeatedly; retained must always be
        // exactly the first `cap` bytes of the stream.
        let mut c = ByteCap::new(nz(7));
        c.push(b"AAA"); // 3
        c.push(b"BBBBB"); // +5 = 8 (overflow by 1)
        c.push(b"C"); // +1 = 9
        // First 7 bytes: "AAA" + first 4 of "BBBBB" = 3 A's + 4 B's.
        assert_eq!(c.as_bytes(), b"AAABBBB");
        assert_eq!(c.retained_len(), 7);
        assert_eq!(c.total(), 9);
    }

    // --- total-overflow helper logic (saturation arithmetic) -----------------

    #[test]
    fn total_overflow_helper_no_overflow() {
        assert_eq!(add_saturating(0, false, 5), (5, false));
        assert_eq!(add_saturating(5, false, 5), (10, false));
        assert_eq!(add_saturating(1 << 20, false, 1 << 20), (2_097_152, false));
    }

    #[test]
    fn total_overflow_helper_saturates_at_ceiling() {
        let max = u64::MAX;
        // Crossing the ceiling saturates and latches the flag.
        assert_eq!(add_saturating(max - 5, false, 10), (max, true));
        // Exactly to the ceiling does NOT saturate (real sum == max).
        assert_eq!(add_saturating(max - 5, false, 5), (max, false));
        // Once saturated, any further add keeps the flag and the ceiling.
        assert_eq!(add_saturating(max, true, 1), (max, true));
        assert_eq!(add_saturating(max, true, 0), (max, true));
        assert_eq!(add_saturating(max, true, max), (max, true));
    }

    #[test]
    fn stats_reflects_total_saturation_flag_only_at_ceiling() {
        // A cap of 1 fed one byte: no saturation, no overflow.
        let mut c = ByteCap::new(nz(1));
        c.push(&[9]);
        assert!(!c.total_saturated());
        // We cannot physically push > u64::MAX bytes, but the helper above
        // proves the flag wiring; here we assert the public surface reports the
        // non-saturated case precisely.
        let s = c.stats();
        assert!(!s.total_saturated);
        assert_eq!(s.total, 1);
    }

    // --- writer/collector surface --------------------------------------------

    #[test]
    fn write_impl_acts_as_a_capture_sink() {
        let mut c = ByteCap::new(nz(3));
        // io::Write accepts everything and never short-writes.
        assert_eq!(c.write(b"ab").unwrap(), 2);
        c.write_all(b"cdefgh").unwrap(); // overflow past cap 3
        assert_eq!(c.as_bytes(), b"abc");
        assert_eq!(c.total(), 8);
        assert!(c.is_overflow());
        c.flush().unwrap();
        // Into-bytes returns the retained prefix only.
        assert_eq!(c.into_bytes(), b"abc");
    }

    #[test]
    fn overflow_display_is_bounded_and_deterministic() {
        let mut c = ByteCap::new(nz(2));
        c.push(b"abcdef");
        let s = c.stats().to_string();
        assert_eq!(
            s,
            "cap=2 retained=2 total=6 total_saturated=false discarded=4 cap_exceeded=true",
        );
    }

    #[test]
    fn identical_feeds_produce_identical_stats() {
        fn build() -> Stats {
            let mut c = ByteCap::new(nz(7));
            for _ in 0..3 {
                c.push(b"abcd");
            }
            c.stats()
        }
        assert_eq!(build(), build());
        let s = build();
        assert_eq!(s.total, 12);
        assert_eq!(s.retained, 7);
        assert_eq!(s.discarded, 5);
        assert!(s.cap_exceeded);
        assert!(!s.total_saturated);
    }

    #[test]
    fn clone_preserves_all_state() {
        let mut c = ByteCap::new(nz(4));
        c.push(b"abcde");
        let c2 = c.clone();
        assert_eq!(c2.as_bytes(), c.as_bytes());
        assert_eq!(c2.total(), c.total());
        assert_eq!(c2.total_saturated(), c.total_saturated());
        assert_eq!(c2.is_overflow(), c.is_overflow());
        assert_eq!(c2.stats(), c.stats());
        // Mutating the original does not leak into the clone.
        c.push(b"f");
        assert_eq!(c.total(), 6);
        assert_eq!(c2.total(), 5);
    }

    #[test]
    fn nonzero_cap_is_enforced_at_the_type_boundary() {
        // A zero cap cannot be expressed through the constructor at all.
        assert!(NonZeroU64::new(0).is_none());
        // Sanity: a nonzero cap is always representable and `cap()` echoes it.
        let c = ByteCap::new(nz(42));
        assert_eq!(c.cap(), 42);
    }
}
