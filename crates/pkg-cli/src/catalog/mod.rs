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
mod id;
mod local;
mod matching;
mod native;
mod routing;
mod search;

use std::fmt;

use crate::nix::NixError;

pub use availability::{Availability, CaskHost};
pub use cask::{
    CaskStatus, CatalogOnce, CatalogView, INDEX_ATTRIBUTE, native_package_attribute,
    package_attribute, record, sources_with_token, status,
};
pub use id::{CatalogId, CatalogIdKind, ParsedId, canonical_source, parse_id};
pub use local::{
    missing_report, prepare_catalog, record_search_system, search_local_casks, search_local_native,
    search_local_saved, search_system,
};
pub(crate) use matching::FUZZY_RANK;
pub use matching::SearchQuery;
pub use routing::Routing;
pub use search::{
    ExactMatch, SearchResult, SourceKind, SourceReport, SourceStatus, SupportBadge, catalog_meta,
    exact_lookup, exposed_attribute, prepare_source, report_for, resolve_bare, resolve_bare_cask,
};

/// A catalog routing error.
#[derive(Debug)]
pub enum CatalogError {
    /// Discovery was cancelled by a signal.
    Interrupted(i32),
    /// A name or attribute matched nothing in the queried source.
    NotFound(String),
    /// A name matched more than one supported output.
    Ambiguous {
        /// The requested name.
        name: String,
        /// The source-qualified or attribute-path choices.
        choices: Vec<String>,
    },
    /// A short alias only resolves to a hidden development package or variant.
    RequiresId {
        /// The requested short name.
        name: String,
        /// Exact source-qualified choices that remain directly installable.
        choices: Vec<String>,
    },
    /// A source query failed. Native diagnostics are preserved.
    Source(String),
    /// A native source query failed.
    Native(NixError),
    /// The disposable cache could not be written.
    CacheWrite(Box<dyn std::error::Error + Send + Sync>),
}

impl CatalogError {
    /// Retain the cause of a failed cache operation.
    pub(crate) fn cache_write(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::CacheWrite(source.into())
    }
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Interrupted(signal) => write!(f, "discovery cancelled by signal {signal}"),
            Self::NotFound(name) => write!(f, "no supported exact match for `{name}`"),
            Self::Ambiguous { name, choices } | Self::RequiresId { name, choices } => {
                if matches!(self, Self::RequiresId { .. }) {
                    write!(f, "`{name}` needs an exact package ID:")?;
                } else {
                    write!(f, "`{name}` is ambiguous; choose a package ID:")?;
                }
                for choice in choices.iter().take(8) {
                    write!(f, "\n  {choice}")?;
                }
                if choices.len() > 8 {
                    write!(
                        f,
                        "\n  {} more choices. Use `pkg search {name} --broad` to inspect them.",
                        choices.len() - 8
                    )?;
                }
                Ok(())
            }
            Self::Source(detail) => write!(f, "{detail}"),
            Self::Native(source) => source.fmt(f),
            Self::CacheWrite(detail) => write!(f, "cannot write discovery cache: {detail}"),
        }
    }
}

impl std::error::Error for CatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Native(source) => Some(source),
            Self::CacheWrite(source) => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<NixError> for CatalogError {
    fn from(error: NixError) -> Self {
        match error {
            NixError::Interrupted { signal, .. } => Self::Interrupted(signal),
            error => Self::Native(error),
        }
    }
}
