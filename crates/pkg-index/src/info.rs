//! Offline package metadata lookup over the disposable index.

use serde::Serialize;

use crate::build::{IndexDocument, IndexRecord, IndexSource};
use crate::query::{
    PackageSummary, QUERY_SCHEMA_VERSION, QueryError, display_name, platform_label, platform_rank,
    validate_text,
};

/// Honest status of vulnerability/advisory data in V1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdvisoryStatus {
    /// No product advisory feed is part of the index; this does not mean safe.
    Unavailable,
}

/// Stable machine-facing info response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InfoResponse {
    schema_version: u64,
    channel_seq: u64,
    stale: bool,
    lookup: InfoLookup,
}

impl InfoResponse {
    /// Returns the channel sequence that supplied the metadata.
    #[must_use]
    pub const fn channel_seq(&self) -> u64 {
        self.channel_seq
    }

    /// Returns whether the loader marked the artifact stale.
    #[must_use]
    pub const fn stale(&self) -> bool {
        self.stale
    }

    /// Returns the lookup outcome.
    #[must_use]
    pub const fn lookup(&self) -> &InfoLookup {
        &self.lookup
    }
}

/// Lookup outcome that preserves ambiguity instead of guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum InfoLookup {
    /// Exactly one package matched.
    Found {
        /// Complete index-served metadata.
        package: Box<PackageInfo>,
    },
    /// A display name or alias matched multiple canonical package ids.
    Ambiguous {
        /// Stable, canonical-package-sorted candidates.
        candidates: Vec<PackageSummary>,
    },
    /// No canonical package id, alias, or display name matched.
    NotFound {
        /// The bounded selector supplied by the caller.
        selector: String,
    },
}

/// Complete index-served package metadata, excluding realized identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageInfo {
    package: String,
    name: String,
    version: String,
    description: String,
    homepage: String,
    licenses: Vec<String>,
    outputs: Vec<String>,
    platforms: Vec<&'static str>,
    available: bool,
    broken: bool,
    catalog_revision: String,
    catalog_generated_at: String,
    catalog_source: IndexSource,
    advisory_status: AdvisoryStatus,
    installed_size_estimate_bytes: Option<u64>,
}

impl PackageInfo {
    fn from_record(document: &IndexDocument, record: &IndexRecord) -> Self {
        let mut platforms: Vec<_> = record
            .platforms()
            .iter()
            .map(|system| platform_label(system))
            .collect();
        platforms.sort_unstable_by_key(|label| platform_rank(label));
        Self {
            package: record.attr_path().to_owned(),
            name: display_name(record).to_owned(),
            version: record.version().to_owned(),
            description: record.description().to_owned(),
            homepage: record.homepage().to_owned(),
            licenses: record.licenses().to_vec(),
            outputs: record.outputs().to_vec(),
            platforms,
            available: record.available_here(),
            broken: record.broken(),
            catalog_revision: document.nixpkgs_rev().to_owned(),
            catalog_generated_at: document.generated_at().to_owned(),
            catalog_source: document.source(),
            advisory_status: AdvisoryStatus::Unavailable,
            installed_size_estimate_bytes: None,
        }
    }

    /// Returns the canonical package identifier.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Returns the upstream display name.
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

    /// Returns the display homepage without fetching it.
    #[must_use]
    pub fn homepage(&self) -> &str {
        &self.homepage
    }

    /// Returns display licenses.
    #[must_use]
    pub fn licenses(&self) -> &[String] {
        &self.licenses
    }

    /// Returns informational output names from the disposable index.
    #[must_use]
    pub fn outputs(&self) -> &[String] {
        &self.outputs
    }

    /// Returns product-owned platform identifiers.
    #[must_use]
    pub fn platforms(&self) -> &[&'static str] {
        &self.platforms
    }

    /// Returns best-effort current-platform availability.
    #[must_use]
    pub const fn available(&self) -> bool {
        self.available
    }

    /// Returns the best-effort broken signal.
    #[must_use]
    pub const fn broken(&self) -> bool {
        self.broken
    }

    /// Returns the exact catalog revision that supplied this metadata.
    #[must_use]
    pub fn catalog_revision(&self) -> &str {
        &self.catalog_revision
    }

    /// Returns the canonical UTC index generation instant.
    #[must_use]
    pub fn catalog_generated_at(&self) -> &str {
        &self.catalog_generated_at
    }

    /// Returns whether the artifact was derived from the pinned source.
    #[must_use]
    pub const fn catalog_source(&self) -> IndexSource {
        self.catalog_source
    }

    /// Returns the honest advisory-feed status.
    #[must_use]
    pub const fn advisory_status(&self) -> AdvisoryStatus {
        self.advisory_status
    }

    /// Returns an estimate only when the index really supplied one.
    #[must_use]
    pub const fn installed_size_estimate_bytes(&self) -> Option<u64> {
        self.installed_size_estimate_bytes
    }
}

pub(crate) fn lookup(
    document: &IndexDocument,
    stale: bool,
    selector: &str,
) -> Result<InfoResponse, QueryError> {
    let selector = validate_text(selector)?;
    if let Some(record) = document
        .records()
        .iter()
        .find(|record| record.attr_path() == selector)
    {
        return Ok(response(
            document,
            stale,
            InfoLookup::Found {
                package: Box::new(PackageInfo::from_record(document, record)),
            },
        ));
    }

    let mut matches: Vec<_> = document
        .records()
        .iter()
        .filter(|record| {
            display_name(record) == selector
                || record.aliases().iter().any(|alias| alias == selector)
        })
        .collect();
    matches.sort_unstable_by_key(|record| record.attr_path());
    let lookup = match matches.as_slice() {
        [] => InfoLookup::NotFound {
            selector: selector.to_owned(),
        },
        [record] => InfoLookup::Found {
            package: Box::new(PackageInfo::from_record(document, record)),
        },
        _ => InfoLookup::Ambiguous {
            candidates: matches
                .into_iter()
                .map(PackageSummary::from_record)
                .collect(),
        },
    };
    Ok(response(document, stale, lookup))
}

const fn response(document: &IndexDocument, stale: bool, lookup: InfoLookup) -> InfoResponse {
    InfoResponse {
        schema_version: QUERY_SCHEMA_VERSION,
        channel_seq: document.channel_seq(),
        stale,
        lookup,
    }
}
