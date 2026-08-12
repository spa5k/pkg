//! `pkg-core` — the shared domain vocabulary for `pkg`.
//!
//! This crate holds the **PR-2** domain types (see `plans/11-pr-roadmap.md`):
//! the user-intent-vs-exact-realization distinction, identity, supported
//! system triples, channel/policy vocabulary, and Nix-native version
//! comparison. Everything else in `pkg` builds on these types.
//!
//! # Design notes
//!
//! - **Intent vs realization** (`plans/00` D-13 / `plans/05` §6): a
//!   [`PackageSelector`] is what the user asked for; a [`Realization`] is the
//!   exact realized store object (input-addressed or content-addressed).
//!   `pname@version` is display metadata and is **never** an identity — the
//!   canonical identity is the store path ([`RealizationIdentity`]).
//! - **Versions** ([`version`]): a [`PackageVersion`] preserves the raw Nix
//!   string, with literal [`Eq`]/[`Hash`] and **no** [`Ord`]; ordering goes
//!   through [`compare_nix_versions`], which mirrors upstream Nix exactly.
//! - **Persistence is isolated** in [`state`]: private Serde DTOs validate into
//!   this crate's existing strong domain types.
//! - **No unsafe**; `proptest` is a dev-dependency for
//!   the roadmap-required property tests (`plans/09` §6.1).
//!
//! The intentional public surface is re-exported at the crate root below.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod channel;
pub mod generation;
pub mod history;
pub mod identity;
pub mod install;
pub mod lifecycle;
pub mod pin;
pub mod realization;
pub mod remove;
pub mod selector;
pub mod state;
pub mod system;
pub mod update;
pub mod upgrade;
pub mod version;

#[cfg(test)]
mod lifecycle_test_support;

// Re-export the intentional public surface at the crate root.
pub use channel::{
    ChannelError, ChannelName, ChannelSequence, NixpkgsRevision, PolicyVersion, SourceRevision,
};
pub use generation::{
    GenerationSnapshot, GenerationSnapshotError, RollbackError, RollbackPlan, RollbackTarget,
    plan_rollback,
};
pub use history::{
    ChangeCounts, ChangeKind, History, HistoryDiff, HistoryError, HistorySummary, PackageChange,
};
pub use identity::{
    DerivationPath, IdentityError, NarHash, OutputName, RealizationIdentity, StorePath,
};
pub use install::{InstallEditError, InstallPackage, InstallResult, install_packages};
pub use pin::{PinAction, PinError, PinResult, edit_pins};
pub use realization::{Realization, RealizationError};
pub use selector::{
    AttributePath, OutputSelection, PackageSelector, PinState, SelectorError, SelectorId,
    SelectorInput,
};
pub use system::{Architecture, Os, System, SystemError};
pub use update::{ChannelUpdateError, advance_channel};
pub use version::{
    PackageVersion, VersionBound, VersionError, VersionPreference, VersionRange,
    compare_nix_versions,
};
