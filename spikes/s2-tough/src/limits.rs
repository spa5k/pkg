// Conservative metadata size limits for the pkg channel.
//
// `tough::Limits` caps the size of downloaded metadata to defeat endless-data
// attacks (`tough` lib docs / TUF spec §5). The library defaults are generous
// (1 MiB root/timestamp/snapshot, 10 MiB targets, 1024 root updates). pkg's
// channel metadata is tiny (a handful of targets), so we set much tighter,
// explicit, conservative caps. These are the values the spike validates against;
// PR-11 finalizes the exact production numbers.

use tough::Limits;

/// Conservative limits for pkg's small target set.
///
/// These are deliberately far below `tough`'s defaults while remaining amply
/// larger than any realistic pkg channel metadata file, so legitimate updates
/// are never refused but an endless-data attack is bounded.
pub const CONSERVATIVE_LIMITS: Limits = Limits {
    // root.json is a few KiB even with several threshold keys. 64 KiB is ~20x
    // headroom.
    max_root_size: 64 * 1024,
    // targets.json (incl. delegated index metadata) for pkg's small set is a
    // few KiB. 256 KiB headroom; far below the 10 MiB default.
    max_targets_size: 256 * 1024,
    // timestamp.json is < 1 KiB. 32 KiB cap.
    max_timestamp_size: 32 * 1024,
    // snapshot.json is < 2 KiB. 32 KiB cap.
    max_snapshot_size: 32 * 1024,
    // Bound the root-chain walk (TUF §5.3.3 "Y number of root metadata files").
    max_root_updates: 256,
};
