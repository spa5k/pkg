//! Shared, pure query vocabulary over a verified index document.

use std::fmt;

use serde::Serialize;

use crate::build::{IndexDocument, IndexRecord};
use crate::info::{InfoResponse, lookup};
use crate::list::{CatalogListOptions, CatalogListResponse, catalog_list};
use crate::search::{SearchOptions, SearchResponse, search};

/// The machine-output schema emitted by index queries.
pub const QUERY_SCHEMA_VERSION: u64 = 1;
/// Maximum accepted search or info selector length.
pub const MAX_QUERY_BYTES: usize = 256;
/// Maximum rows returned by one search or catalog-list query.
pub const MAX_QUERY_RESULTS: usize = 1_000;

/// A pure, offline query view over one already-verified index.
#[derive(Debug, Clone, Copy)]
pub struct IndexQuery<'a> {
    document: &'a IndexDocument,
    stale: bool,
}

impl<'a> IndexQuery<'a> {
    /// Creates a query view. `stale` is supplied by the channel/index loader,
    /// because an index cannot infer whether a newer descriptor was accepted.
    #[must_use]
    pub const fn new(document: &'a IndexDocument, stale: bool) -> Self {
        Self { document, stale }
    }

    /// Searches display metadata without network or Nix evaluation.
    pub fn search(&self, options: &SearchOptions) -> Result<SearchResponse, QueryError> {
        search(self.document, self.stale, options)
    }

    /// Returns a bounded page of the derived catalog.
    ///
    /// This is not the user-facing installed-package `pkg list`, which reads
    /// realized lock/generation state.
    pub fn catalog_list(
        &self,
        options: &CatalogListOptions,
    ) -> Result<CatalogListResponse, QueryError> {
        catalog_list(self.document, self.stale, options)
    }

    /// Looks up one canonical package id, alias, or display name offline.
    pub fn info(&self, selector: &str) -> Result<InfoResponse, QueryError> {
        lookup(self.document, self.stale, selector)
    }
}

/// Product-owned package summary shared by search, ambiguity, and catalog pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageSummary {
    package: String,
    name: String,
    version: String,
    description: String,
    licenses: Vec<String>,
    available: bool,
    broken: bool,
}

impl PackageSummary {
    pub(crate) fn from_record(record: &IndexRecord) -> Self {
        Self {
            package: record.attr_path().to_owned(),
            name: display_name(record).to_owned(),
            version: record.version().to_owned(),
            description: record.description().to_owned(),
            licenses: record.licenses().to_vec(),
            available: record.available_here(),
            broken: record.broken(),
        }
    }

    /// Returns the canonical copy/paste package identifier.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Returns the upstream display name, falling back to the package id.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the display version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the display description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns display license identifiers or names.
    #[must_use]
    pub fn licenses(&self) -> &[String] {
        &self.licenses
    }

    /// Returns whether the index considers it available on this target system.
    #[must_use]
    pub const fn available(&self) -> bool {
        self.available
    }

    /// Returns the best-effort broken signal.
    #[must_use]
    pub const fn broken(&self) -> bool {
        self.broken
    }
}

pub(crate) fn display_name(record: &IndexRecord) -> &str {
    if record.pname().is_empty() {
        record.attr_path()
    } else {
        record.pname()
    }
}

pub(crate) fn validate_text(value: &str) -> Result<&str, QueryError> {
    if value.chars().any(char::is_control) {
        return Err(QueryError::InvalidQuery(
            "query contains control characters",
        ));
    }
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(QueryError::InvalidQuery("query must not be empty"));
    }
    if trimmed.len() > MAX_QUERY_BYTES {
        return Err(QueryError::InvalidQuery("query exceeds 256 bytes"));
    }
    Ok(trimmed)
}

pub(crate) const fn platform_label(system: &str) -> &'static str {
    match system.as_bytes() {
        b"x86_64-linux" => "linux-x86-64",
        b"aarch64-linux" => "linux-arm64",
        b"x86_64-darwin" => "macos-x86-64",
        b"aarch64-darwin" => "macos-apple-silicon",
        _ => "unsupported",
    }
}

pub(crate) const fn platform_rank(label: &str) -> u8 {
    match label.as_bytes() {
        b"linux-x86-64" => 0,
        b"linux-arm64" => 1,
        b"macos-x86-64" => 2,
        b"macos-apple-silicon" => 3,
        _ => 4,
    }
}

/// Closed validation failures for pure index queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// Search or selector text violated the bounded public grammar.
    InvalidQuery(&'static str),
    /// A requested page size was zero or exceeded the fixed maximum.
    InvalidLimit,
    /// Offset plus limit overflowed the address space.
    InvalidOffset,
    /// A license filter violated the bounded public grammar.
    InvalidLicense,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQuery(reason) => write!(f, "invalid index query: {reason}"),
            Self::InvalidLimit => write!(f, "query limit must be between 1 and 1000"),
            Self::InvalidOffset => f.write_str("query page offset overflowed"),
            Self::InvalidLicense => f.write_str("invalid license filter"),
        }
    }
}

impl std::error::Error for QueryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_validation_is_bounded_and_rejects_controls() {
        assert_eq!(validate_text("  ripgrep  "), Ok("ripgrep"));
        assert!(validate_text(" ").is_err());
        assert!(validate_text("bad\nquery").is_err());
        assert!(validate_text("ripgrep\n").is_err());
        assert!(validate_text(&"x".repeat(MAX_QUERY_BYTES + 1)).is_err());
    }

    #[test]
    fn platform_names_are_product_owned() {
        assert_eq!(platform_label("x86_64-linux"), "linux-x86-64");
        assert_eq!(platform_label("aarch64-darwin"), "macos-apple-silicon");
    }
}
