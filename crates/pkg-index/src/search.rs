//! Deterministic host-filtered search over index display metadata.

use serde::Serialize;

use crate::build::{IndexDocument, IndexRecord};
use crate::query::{
    MAX_QUERY_RESULTS, PackageSummary, QUERY_SCHEMA_VERSION, QueryError, display_name,
    platform_label, validate_text,
};

/// Validated options for one search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOptions {
    query: String,
    limit: usize,
    exact: bool,
    license: Option<String>,
}

impl SearchOptions {
    /// Validates a search request. Results are host-filtered by default.
    pub fn new(
        query: &str,
        limit: usize,
        exact: bool,
        license: Option<&str>,
    ) -> Result<Self, QueryError> {
        let query = validate_text(query)?.to_owned();
        if !(1..=MAX_QUERY_RESULTS).contains(&limit) {
            return Err(QueryError::InvalidLimit);
        }
        let license = match license {
            Some(value) => {
                if value.chars().any(char::is_control) {
                    return Err(QueryError::InvalidLicense);
                }
                let value = value.trim();
                if value.is_empty() || value.len() > 128 {
                    return Err(QueryError::InvalidLicense);
                }
                Some(value.to_owned())
            }
            None => None,
        };
        Ok(Self {
            query,
            limit,
            exact,
            license,
        })
    }
}

/// Stable machine-facing search response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    schema_version: u64,
    channel_seq: u64,
    system: &'static str,
    stale: bool,
    results: Vec<PackageSummary>,
}

impl SearchResponse {
    /// Returns the channel sequence that supplied these results.
    #[must_use]
    pub const fn channel_seq(&self) -> u64 {
        self.channel_seq
    }

    /// Returns the product-owned target-platform identifier.
    #[must_use]
    pub const fn system(&self) -> &'static str {
        self.system
    }

    /// Returns ranked package summaries.
    #[must_use]
    pub fn results(&self) -> &[PackageSummary] {
        &self.results
    }

    /// Returns whether the loader marked the underlying artifact stale.
    #[must_use]
    pub const fn stale(&self) -> bool {
        self.stale
    }
}

pub(crate) fn search(
    document: &IndexDocument,
    stale: bool,
    options: &SearchOptions,
) -> Result<SearchResponse, QueryError> {
    let query = options.query.to_lowercase();
    let terms: Vec<_> = query.split_whitespace().collect();
    let mut hits = Vec::new();
    for record in document.records() {
        if let Some(license) = &options.license
            && !record
                .licenses()
                .iter()
                .any(|item| item.eq_ignore_ascii_case(license))
        {
            continue;
        }
        if let Some(score) = score(record, &query, &terms, options.exact) {
            hits.push((
                score,
                record.attr_path(),
                PackageSummary::from_record(record),
            ));
        }
    }
    hits.sort_unstable_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));
    let results = hits
        .into_iter()
        .take(options.limit)
        .map(|(_, _, summary)| summary)
        .collect();
    Ok(SearchResponse {
        schema_version: QUERY_SCHEMA_VERSION,
        channel_seq: document.channel_seq(),
        system: platform_label(document.system()),
        stale,
        results,
    })
}

fn score(record: &IndexRecord, query: &str, terms: &[&str], exact: bool) -> Option<u16> {
    let package = record.attr_path().to_lowercase();
    let name = display_name(record).to_lowercase();
    let aliases: Vec<_> = record
        .aliases()
        .iter()
        .map(|alias| alias.to_lowercase())
        .collect();

    if package == query {
        return Some(0);
    }
    if name == query {
        return Some(1);
    }
    if aliases.iter().any(|alias| alias == query) {
        return Some(2);
    }
    if exact {
        return None;
    }
    if package.starts_with(query) {
        return Some(10);
    }
    if name.starts_with(query) {
        return Some(11);
    }
    if aliases.iter().any(|alias| alias.starts_with(query)) {
        return Some(12);
    }
    if package.contains(query) {
        return Some(20);
    }
    if name.contains(query) {
        return Some(21);
    }
    if aliases.iter().any(|alias| alias.contains(query)) {
        return Some(22);
    }

    let description = record.description().to_lowercase();
    if !terms.is_empty()
        && terms.iter().all(|term| {
            package.contains(term)
                || name.contains(term)
                || description.contains(term)
                || aliases.iter().any(|alias| alias.contains(term))
        })
    {
        return Some(30);
    }
    if !query.contains(char::is_whitespace)
        && (is_subsequence(query, &package)
            || is_subsequence(query, &name)
            || aliases.iter().any(|alias| is_subsequence(query, alias)))
    {
        return Some(40);
    }
    None
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = needle.chars();
    let mut next = chars.next();
    for candidate in haystack.chars() {
        if next == Some(candidate) {
            next = chars.next();
            if next.is_none() {
                return true;
            }
        }
    }
    next.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::IndexCandidate;

    fn record(path: &str, pname: &str, description: &str, aliases: &[&str]) -> IndexRecord {
        let candidate = IndexCandidate {
            attr_path: path.into(),
            pname: Some(pname.into()),
            version: None,
            description: Some(description.into()),
            homepage: None,
            licenses: vec!["MIT".into()],
            platforms: vec!["aarch64-darwin".into()],
            available_here: true,
            broken: false,
            position: None,
            outputs: vec!["out".into()],
            aliases: aliases.iter().map(|value| (*value).into()).collect(),
            skipped: false,
        };
        crate::build::test_record(candidate)
    }

    #[test]
    fn ranking_prefers_package_then_name_then_alias() {
        let package = record("rg", "other", "", &[]);
        let name = record("name-hit", "rg", "", &[]);
        let alias = record("alias-hit", "other", "", &["rg"]);
        assert_eq!(score(&package, "rg", &["rg"], false), Some(0));
        assert_eq!(score(&name, "rg", &["rg"], false), Some(1));
        assert_eq!(score(&alias, "rg", &["rg"], false), Some(2));
    }

    #[test]
    fn keyword_and_subsequence_matches_are_bounded() {
        let keyword = record("requests", "requests", "Python HTTP client", &[]);
        assert_eq!(
            score(&keyword, "python client", &["python", "client"], false),
            Some(30)
        );
        assert_eq!(score(&keyword, "rqsts", &["rqsts"], false), Some(40));
        assert_eq!(score(&keyword, "rqsts", &["rqsts"], true), None);
    }

    #[test]
    fn options_reject_zero_huge_and_control_bearing_filters() {
        assert_eq!(
            SearchOptions::new("ripgrep", 0, false, None),
            Err(QueryError::InvalidLimit)
        );
        assert_eq!(
            SearchOptions::new("ripgrep", MAX_QUERY_RESULTS + 1, false, None),
            Err(QueryError::InvalidLimit)
        );
        assert_eq!(
            SearchOptions::new("ripgrep", 25, false, Some("MIT\n")),
            Err(QueryError::InvalidLicense)
        );
    }
}
