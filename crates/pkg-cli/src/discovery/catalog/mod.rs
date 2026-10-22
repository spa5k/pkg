//! Catalog name routing and disposable discovery data (design D5).
//!
//! The catalog maps names to installables. It owns no installed state. Discovery
//! uses native evaluation results and generated Homebrew support metadata;
//! repositories are never parsed.
//!
//! Native discovery records the moving source, locked reference, revision, and
//! system. Native installation passes the original moving reference to Nix.
//! Formula installation uses verified Nixpkgs equivalents or restricted bottles.
//!
//! Layout: `id` parses and normalizes user IDs, `search` queries sources
//! with provenance and looks up exact matches, `cask` reads the generated
//! catalog index, `brew` reads the generated Formula index, and `cache` holds
//! the disposable derived data.

mod availability;
pub mod brew;
mod cache;
mod cask;
mod choice;
mod id;
mod local;
mod matching;
mod native;
mod published;
mod routing;
mod search;

use crate::nix::NixError;

pub use availability::{Availability, CaskHost, plain_reason};
pub use cask::{
    CatalogOnce, CatalogView, native_package_attribute, package_attribute, source_display,
};
pub use choice::{Choice, Kept, Names, Scope, short_id};
pub use id::{CatalogId, CatalogIdKind, ParsedId, canonical_source, parse_id};
pub use local::{
    NOT_READY, missing_report, nixpkgs_summary, not_ready_report, prepare_catalog, prepared_system,
    record_search_system, search_local_casks, search_local_native, search_local_saved,
    search_system,
};
pub(crate) use matching::FUZZY_RANK;
pub use matching::SearchQuery;
pub use native::NativeRow;
pub use published::{OTHER_SYSTEMS, channel as published_channel};
pub use routing::Routing;
pub use search::{
    ExactMatch, SearchResult, SourceKind, SourceReport, SourceStatus, SupportBadge, catalog_meta,
    exact_lookup, exposed_attribute, prepare_nixpkgs, report_for,
};

/// A catalog routing error.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// Discovery was cancelled by a signal.
    #[error("discovery cancelled by signal {0}")]
    Interrupted(i32),
    /// A name or attribute matched nothing in the queried source.
    #[error("no supported exact match for `{0}`")]
    NotFound(String),
    /// A name matched more than one supported output.
    #[error(
        "{}",
        choice_message(name, choices, "is ambiguous; choose a package ID")
    )]
    Ambiguous {
        /// The requested name.
        name: String,
        /// The source-qualified or attribute-path choices.
        choices: Vec<String>,
    },
    /// The package exists, but Nix cannot evaluate it, for example a
    /// removed Nixpkgs alias.
    #[error("`{name}` cannot be installed: {cause}")]
    Unusable {
        /// The requested name.
        name: String,
        /// The innermost native evaluation message.
        cause: String,
    },
    /// A source query failed. Native diagnostics are preserved.
    #[error("{0}")]
    Source(String),
    /// A native source query failed.
    #[error("{0}")]
    Native(#[source] NixError),
    /// A host-version probe was cancelled.
    #[error("{0}")]
    Host(#[source] std::sync::Arc<crate::nix::HostVersionError>),
    /// The disposable cache could not be written.
    #[error("cannot write discovery cache: {0}")]
    CacheWrite(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl CatalogError {
    /// Retain the cause of a failed cache operation.
    pub(crate) fn cache_write(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::CacheWrite(source.into())
    }
}

impl From<rusqlite::Error> for CatalogError {
    fn from(error: rusqlite::Error) -> Self {
        Self::cache_write(error)
    }
}

fn choice_message(name: &str, choices: &[String], header: &str) -> String {
    use std::fmt::Write as _;
    let mut message = format!("`{name}` {header}:");
    for choice in choices.iter().take(8) {
        let _ = write!(message, "\n  {choice}");
    }
    if choices.len() > 8 {
        let _ = write!(
            message,
            "\n  {} more choices. Use `pkg search {name} --broad` to inspect them.",
            choices.len() - 8
        );
    }
    message
}

impl From<NixError> for CatalogError {
    fn from(error: NixError) -> Self {
        match error {
            NixError::Interrupted { signal, .. } => Self::Interrupted(signal),
            error => Self::Native(error),
        }
    }
}

impl CatalogError {
    /// The cancellation signal retained by discovery or its native child.
    #[must_use]
    pub fn signal(&self) -> Option<i32> {
        match self {
            Self::Interrupted(signal) => Some(*signal),
            Self::Native(error) => error.signal(),
            Self::Host(error) => error.signal(),
            _ => None,
        }
    }
}
