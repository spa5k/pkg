//! Closed product catalog query vocabulary for the private broker channel.

use pkg_core::ChannelSequence;

const MAX_QUERY_BYTES: usize = 256;
const MAX_LICENSE_BYTES: usize = 128;
const MAX_METADATA_STRING_BYTES: usize = 4 * 1024;
const MAX_LIST_ITEMS: usize = 256;
const MAX_SEARCH_RESULTS: usize = 1_000;
// Leave room below the 1 MiB frame ceiling for JSON structure and the
// worst-case doubling of accepted quotes and backslashes during encoding.
const MAX_REPORT_TEXT_BYTES: usize = 400 * 1024;

/// One bounded search request over the broker-owned verified index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSearchRequest {
    query: String,
    limit: u16,
    exact: bool,
    license: Option<String>,
}

impl CatalogSearchRequest {
    /// Validates product search text and display-only filters.
    #[must_use]
    pub fn new(query: &str, limit: u16, exact: bool, license: Option<&str>) -> Option<Self> {
        let query = bounded_query(query)?;
        if limit == 0 || usize::from(limit) > MAX_SEARCH_RESULTS {
            return None;
        }
        let license = match license {
            Some(value) => Some(bounded_license(value)?),
            None => None,
        };
        Some(Self {
            query,
            limit,
            exact,
            license,
        })
    }

    /// Returns the validated search text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns the bounded result limit.
    #[must_use]
    pub const fn limit(&self) -> u16 {
        self.limit
    }

    /// Returns whether only exact display identity may match.
    #[must_use]
    pub const fn exact(&self) -> bool {
        self.exact
    }

    /// Returns the optional display license filter.
    #[must_use]
    pub fn license(&self) -> Option<&str> {
        self.license.as_deref()
    }
}

/// One bounded package-info request over the broker-owned verified index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogInfoRequest {
    selector: String,
}

impl CatalogInfoRequest {
    /// Validates one canonical package id, alias, or display name.
    #[must_use]
    pub fn new(selector: &str) -> Option<Self> {
        Some(Self {
            selector: bounded_query(selector)?,
        })
    }

    /// Returns the validated selector.
    #[must_use]
    pub fn selector(&self) -> &str {
        &self.selector
    }
}

/// Product-owned package summary returned by catalog search or ambiguity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPackageSummary {
    package: String,
    name: String,
    version: String,
    description: String,
    licenses: Vec<String>,
    available: bool,
    broken: bool,
}

impl CatalogPackageSummary {
    /// Constructs one bounded summary from a verified index record.
    #[must_use]
    pub fn new(
        package: &str,
        name: &str,
        version: &str,
        description: &str,
        licenses: Vec<String>,
        available: bool,
        broken: bool,
    ) -> Option<Self> {
        if licenses.len() > MAX_LIST_ITEMS || licenses.iter().any(|value| !bounded_metadata(value))
        {
            return None;
        }
        Some(Self {
            package: checked_metadata(package)?,
            name: checked_metadata(name)?,
            version: checked_metadata(version)?,
            description: checked_metadata(description)?,
            licenses,
            available,
            broken,
        })
    }

    /// Returns the canonical copy/paste package id.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Returns the display name.
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

    /// Returns current-platform availability from the verified index.
    #[must_use]
    pub const fn available(&self) -> bool {
        self.available
    }

    /// Returns the best-effort broken signal.
    #[must_use]
    pub const fn broken(&self) -> bool {
        self.broken
    }

    fn text_bytes(&self) -> Option<usize> {
        [
            self.package.len(),
            self.name.len(),
            self.version.len(),
            self.description.len(),
        ]
        .into_iter()
        .chain(self.licenses.iter().map(String::len))
        .try_fold(0_usize, usize::checked_add)
    }
}

/// Sanitized result of one catalog search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSearchReport {
    sequence: ChannelSequence,
    results: Vec<CatalogPackageSummary>,
}

impl CatalogSearchReport {
    /// Constructs a bounded report tied to one authenticated channel sequence.
    #[must_use]
    pub fn new(sequence: ChannelSequence, results: Vec<CatalogPackageSummary>) -> Option<Self> {
        if results.len() > MAX_SEARCH_RESULTS
            || report_text_bytes(&results)? > MAX_REPORT_TEXT_BYTES
        {
            return None;
        }
        Some(Self { sequence, results })
    }

    /// Returns the authenticated channel sequence.
    #[must_use]
    pub const fn sequence(&self) -> ChannelSequence {
        self.sequence
    }

    /// Returns ranked product summaries.
    #[must_use]
    pub fn results(&self) -> &[CatalogPackageSummary] {
        &self.results
    }
}

/// Complete index-served package metadata, without realized Nix identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPackageInfo {
    summary: CatalogPackageSummary,
    homepage: String,
    outputs: Vec<String>,
    platforms: Vec<String>,
    catalog_revision: String,
    catalog_generated_at: String,
}

impl CatalogPackageInfo {
    /// Constructs bounded informational metadata from one verified index record.
    #[must_use]
    pub fn new(
        summary: CatalogPackageSummary,
        homepage: &str,
        outputs: Vec<String>,
        platforms: Vec<String>,
        catalog_revision: &str,
        catalog_generated_at: &str,
    ) -> Option<Self> {
        if outputs.len() > MAX_LIST_ITEMS
            || platforms.len() > MAX_LIST_ITEMS
            || outputs.iter().any(|value| !bounded_metadata(value))
            || platforms.iter().any(|value| !bounded_metadata(value))
        {
            return None;
        }
        let value = Self {
            summary,
            homepage: checked_metadata(homepage)?,
            outputs,
            platforms,
            catalog_revision: checked_metadata(catalog_revision)?,
            catalog_generated_at: checked_metadata(catalog_generated_at)?,
        };
        (value.text_bytes()? <= MAX_REPORT_TEXT_BYTES).then_some(value)
    }

    /// Returns the shared package summary.
    #[must_use]
    pub const fn summary(&self) -> &CatalogPackageSummary {
        &self.summary
    }

    /// Returns the display homepage without fetching it.
    #[must_use]
    pub fn homepage(&self) -> &str {
        &self.homepage
    }

    /// Returns informational output names.
    #[must_use]
    pub fn outputs(&self) -> &[String] {
        &self.outputs
    }

    /// Returns product-owned platform labels.
    #[must_use]
    pub fn platforms(&self) -> &[String] {
        &self.platforms
    }

    /// Returns the exact Nixpkgs revision that generated the catalog.
    #[must_use]
    pub fn catalog_revision(&self) -> &str {
        &self.catalog_revision
    }

    /// Returns the canonical index generation time.
    #[must_use]
    pub fn catalog_generated_at(&self) -> &str {
        &self.catalog_generated_at
    }

    fn text_bytes(&self) -> Option<usize> {
        self.summary
            .text_bytes()?
            .checked_add(self.homepage.len())?
            .checked_add(self.catalog_revision.len())?
            .checked_add(self.catalog_generated_at.len())?
            .checked_add(self.outputs.iter().map(String::len).sum::<usize>())?
            .checked_add(self.platforms.iter().map(String::len).sum::<usize>())
    }
}

/// Stable package-info lookup result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogInfoLookup {
    /// Exactly one package matched.
    Found(Box<CatalogPackageInfo>),
    /// A display name or alias matched multiple canonical ids.
    Ambiguous(Vec<CatalogPackageSummary>),
    /// No package matched.
    NotFound,
}

/// Sanitized result of one package-info lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogInfoReport {
    sequence: ChannelSequence,
    lookup: CatalogInfoLookup,
}

impl CatalogInfoReport {
    /// Constructs one bounded lookup report.
    #[must_use]
    pub fn new(sequence: ChannelSequence, lookup: CatalogInfoLookup) -> Option<Self> {
        let valid = match &lookup {
            CatalogInfoLookup::Found(package) => package.text_bytes()? <= MAX_REPORT_TEXT_BYTES,
            CatalogInfoLookup::Ambiguous(candidates) => {
                candidates.len() <= MAX_SEARCH_RESULTS
                    && report_text_bytes(candidates)? <= MAX_REPORT_TEXT_BYTES
            }
            CatalogInfoLookup::NotFound => true,
        };
        valid.then_some(Self { sequence, lookup })
    }

    /// Returns the authenticated channel sequence.
    #[must_use]
    pub const fn sequence(&self) -> ChannelSequence {
        self.sequence
    }

    /// Returns the stable lookup outcome.
    #[must_use]
    pub const fn lookup(&self) -> &CatalogInfoLookup {
        &self.lookup
    }
}

fn bounded_query(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()
        && trimmed.len() <= MAX_QUERY_BYTES
        && !value.chars().any(char::is_control))
    .then(|| trimmed.to_owned())
}

fn bounded_license(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()
        && trimmed.len() <= MAX_LICENSE_BYTES
        && !value.chars().any(char::is_control))
    .then(|| trimmed.to_owned())
}

fn checked_metadata(value: &str) -> Option<String> {
    bounded_metadata(value).then(|| value.to_owned())
}

fn bounded_metadata(value: &str) -> bool {
    value.len() <= MAX_METADATA_STRING_BYTES && !value.chars().any(char::is_control)
}

fn report_text_bytes(values: &[CatalogPackageSummary]) -> Option<usize> {
    values.iter().try_fold(0_usize, |total, value| {
        total.checked_add(value.text_bytes()?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_and_report_bounds_are_closed() {
        assert!(CatalogSearchRequest::new("ripgrep", 25, false, Some("MIT")).is_some());
        assert!(CatalogSearchRequest::new("bad\nquery", 25, false, None).is_none());
        assert!(CatalogSearchRequest::new("ripgrep", 0, false, None).is_none());
        assert!(CatalogInfoRequest::new(&"x".repeat(MAX_QUERY_BYTES + 1)).is_none());

        let summary = CatalogPackageSummary::new(
            "ripgrep",
            "ripgrep",
            "14.1.1",
            "fast search",
            vec![String::from("MIT")],
            true,
            false,
        )
        .unwrap();
        assert!(
            CatalogSearchReport::new(ChannelSequence::from_u64(42).unwrap(), vec![summary])
                .is_some()
        );
    }
}
