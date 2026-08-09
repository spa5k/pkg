//! Bounded enumeration of the derived catalog.

use serde::Serialize;

use crate::build::IndexDocument;
use crate::query::{
    MAX_QUERY_RESULTS, PackageSummary, QUERY_SCHEMA_VERSION, QueryError, platform_label,
};

/// Validated catalog-page options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogListOptions {
    offset: usize,
    limit: usize,
    include_unavailable: bool,
}

impl CatalogListOptions {
    /// Creates a bounded page request.
    pub fn new(offset: usize, limit: usize, include_unavailable: bool) -> Result<Self, QueryError> {
        if !(1..=MAX_QUERY_RESULTS).contains(&limit) {
            return Err(QueryError::InvalidLimit);
        }
        offset.checked_add(limit).ok_or(QueryError::InvalidOffset)?;
        Ok(Self {
            offset,
            limit,
            include_unavailable,
        })
    }
}

/// Stable machine-facing derived-catalog page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogListResponse {
    schema_version: u64,
    channel_seq: u64,
    system: &'static str,
    stale: bool,
    offset: usize,
    total: usize,
    packages: Vec<PackageSummary>,
}

impl CatalogListResponse {
    /// Returns the channel sequence that supplied this page.
    #[must_use]
    pub const fn channel_seq(&self) -> u64 {
        self.channel_seq
    }

    /// Returns the product-owned target-platform identifier.
    #[must_use]
    pub const fn system(&self) -> &'static str {
        self.system
    }

    /// Returns whether the loader marked the artifact stale.
    #[must_use]
    pub const fn stale(&self) -> bool {
        self.stale
    }

    /// Returns the requested page offset.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the total matching records before pagination.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.total
    }

    /// Returns the page's package summaries.
    #[must_use]
    pub fn packages(&self) -> &[PackageSummary] {
        &self.packages
    }
}

pub(crate) fn catalog_list(
    document: &IndexDocument,
    stale: bool,
    options: &CatalogListOptions,
) -> Result<CatalogListResponse, QueryError> {
    let matching: Vec<_> = document
        .records()
        .iter()
        .filter(|record| {
            options.include_unavailable || (record.available_here() && !record.broken())
        })
        .collect();
    let total = matching.len();
    let packages = matching
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .map(PackageSummary::from_record)
        .collect();
    Ok(CatalogListResponse {
        schema_version: QUERY_SCHEMA_VERSION,
        channel_seq: document.channel_seq(),
        system: platform_label(document.system()),
        stale,
        offset: options.offset,
        total,
        packages,
    })
}
