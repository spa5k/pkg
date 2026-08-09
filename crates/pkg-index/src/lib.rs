//! Deterministic, disposable package-catalog artifacts.
//!
//! The index helps users discover Nixpkgs attribute paths. It is deliberately
//! not an authority for realization: records cannot contain store paths or NAR
//! hashes, and install-time evaluation remains authoritative.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod build;
pub mod info;
pub mod list;
pub mod query;
pub mod search;

pub use build::{
    BuildMetadata, BuiltIndex, IndexBuildError, IndexCandidate, IndexDocument, IndexRecord,
    IndexSource, build_index, build_index_from_json,
};
pub use info::{AdvisoryStatus, InfoLookup, InfoResponse, PackageInfo};
pub use list::{CatalogListOptions, CatalogListResponse};
pub use query::{IndexQuery, PackageSummary, QueryError};
pub use search::{SearchOptions, SearchResponse};
