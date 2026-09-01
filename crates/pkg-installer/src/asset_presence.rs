//! The shared fixed-object presence contract used by both platforms.

/// Whether one fixed platform object is exact-present or absent before a
/// write-ahead intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetPresence {
    /// The exact fixed object exists and matches the closed contract.
    ExactPresent,
    /// The fixed object is absent.
    Absent,
}
