//! Catalog name routing and disposable discovery data (design D5).
//!
//! The catalog maps names to installables. It owns no installed state. Discovery
//! uses native evaluation results only; repositories are never parsed.
//!
//! Every discovery result is bound to provenance: the moving source reference,
//! the locked reference actually queried, the locked revision, and the system.
//! Installation always passes the original moving reference to Nix; only
//! discovery reads through the locked reference so a result set can never mix
//! revisions.
//!
//! Layout: `id` parses and normalizes user IDs, `search` queries sources
//! with provenance and looks up exact matches, `cask` reads the generated
//! catalog index, and `cache` holds the disposable derived data.

mod cache;
mod cask;
mod id;
mod routing;
mod search;

use std::fmt;

use crate::nix::NixError;

pub use cache::invalidate_cache;
pub use cask::{
    CaskStatus, CatalogOnce, CatalogView, INDEX_ATTRIBUTE, native_package_attribute,
    package_attribute, record, sources_with_token, status,
};
pub use id::{CatalogId, ParsedId, canonical_source, escape_regex, parse_id};
pub use routing::Routing;
pub use search::{
    ExactMatch, SearchResult, SourceKind, SourceReport, SourceStatus, SupportBadge, catalog_meta,
    exact_lookup, exact_lookup_in, exposed_attribute, report_for, resolve_bare, resolve_bare_cask,
    search_catalog, search_saved_catalog, search_source,
};

/// A catalog routing error.
#[derive(Debug)]
pub enum CatalogError {
    /// A name or attribute matched nothing in the queried source.
    NotFound(String),
    /// A name matched more than one supported output.
    Ambiguous {
        /// The requested name.
        name: String,
        /// The source-qualified or attribute-path choices.
        choices: Vec<String>,
    },
    /// A source query failed. Native diagnostics are preserved.
    Source(String),
    /// The disposable cache could not be written.
    CacheWrite(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(name) => write!(f, "no supported exact match for `{name}`"),
            Self::Ambiguous { name, choices } => write!(
                f,
                "`{name}` is ambiguous; choose one of: {}",
                choices.join(", ")
            ),
            Self::Source(detail) => write!(f, "{detail}"),
            Self::CacheWrite(detail) => write!(f, "cannot write discovery cache: {detail}"),
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<NixError> for CatalogError {
    fn from(error: NixError) -> Self {
        Self::Source(error.to_string())
    }
}
