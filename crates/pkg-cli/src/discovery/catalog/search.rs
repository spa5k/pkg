//! Discovery queries with provenance, and exact lookups for `info`.
//!
//! Local search is implemented in `local`. This module holds shared rows,
//! provenance, exact lookups, and strict name resolution for mutations.

use std::collections::BTreeMap;
use std::path::Path;

use crate::nix::{CatalogEntry, Nix, SearchMeta};

use super::CatalogError;

/// Which supported source a discovery query ran against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum_macros::IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum SourceKind {
    /// The configured Nixpkgs source.
    Nixpkgs,
    /// The configured Cask product source.
    Cask,
}

impl SourceKind {
    /// The qualified display prefix for this source.
    #[must_use]
    pub fn label(self) -> &'static str {
        self.into()
    }
}

/// Provenance and health of one source query.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceReport {
    /// The moving source reference pkg queried.
    pub source: String,
    /// The display identity from flake metadata, when metadata was readable.
    pub display: Option<String>,
    /// The locked reference the results were produced from.
    pub locked_reference: Option<String>,
    /// The locked revision of that reference.
    pub revision: Option<String>,
    /// The outcome of this source query.
    pub status: SourceStatus,
    /// Failure or limitation detail, when present.
    pub detail: Option<String>,
}

impl SourceReport {
    /// Whether this official source has no complete local data yet.
    #[must_use]
    pub fn is_not_ready(&self) -> bool {
        self.status == SourceStatus::Failed && self.detail.as_deref() == Some(super::NOT_READY)
    }

    /// The report for a source this system cannot query at all.
    ///
    /// Skip a system only when the catalog index's target list excludes it.
    #[must_use]
    pub fn skipped_platform(source: &str, targets: &[String]) -> Self {
        Self {
            source: source.to_string(),
            display: None,
            locked_reference: None,
            revision: None,
            status: SourceStatus::SkippedPlatform,
            detail: Some(format!("catalog targets {} only", targets.join(", "))),
        }
    }
}

/// The outcome of one source query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
#[derive(strum_macros::IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
pub enum SourceStatus {
    /// Live results produced or a revision-exact cache was reused.
    Fresh,
    /// Local complete data; no live source check was made by this search.
    Cached,
    /// Live query failed; results are reused stale cache data.
    Stale,
    /// The query failed and no usable cache existed.
    Failed,
    /// The source was not queried on this system.
    SkippedPlatform,
}

impl SourceStatus {
    /// The JSON status string for this outcome.
    #[must_use]
    pub fn label(self) -> &'static str {
        self.into()
    }
}

/// Support badge for a generated catalog row in discovery results.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum SupportBadge {
    /// The token is eligible on this system.
    ///
    /// Eligible is the generated metadata status; it is not a verification
    /// promise.
    Eligible,
    /// Host requirements could not be checked.
    Unknown {
        /// Why availability is unknown.
        reason: String,
    },
    /// The token is excluded on this system.
    Excluded {
        /// A short machine-readable reason.
        reason: String,
        /// Human-readable detail for the exclusion, when present.
        detail: Option<String>,
    },
}

/// One search result row after routing, with provenance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchResult {
    /// The source-qualified ID, for example `nixpkgs:fd` or `cask:raycast`.
    ///
    /// Standard `packages.<system>.` and `legacyPackages.<system>.` prefixes
    /// are removed so the ID round-trips to install.
    pub id: String,
    /// The full native attribute behind this row, for exact matching.
    pub attribute: String,
    /// The package name.
    pub name: String,
    /// The package version.
    pub version: String,
    /// The package description.
    pub description: String,
    /// The source label (`nixpkgs`, `brew`, or `cask`).
    pub source: String,
    /// The locked reference this row was evaluated from.
    pub reference: Option<String>,
    /// The locked revision of that reference.
    pub revision: Option<String>,
    /// The system this row was evaluated for.
    pub system: String,
    /// Whether the row was reused from stale cache data.
    pub stale: bool,
    /// Generated catalog support classification.
    pub support: Option<SupportBadge>,
    /// The pkg command that installs this row. `None` when the row cannot
    /// install on this system.
    #[serde(default)]
    pub install_command: Option<Vec<String>>,
    /// The main command of a Nixpkgs package, from `meta.mainProgram`.
    #[serde(default)]
    pub command: Option<String>,
}

impl SearchResult {
    pub(super) fn catalog(
        entry: &CatalogEntry,
        source: &str,
        system: &str,
        report: &SourceReport,
    ) -> Self {
        let full_id = format!("{}/{}", entry.source, entry.token);
        let meta = catalog_meta(&entry.token, entry);
        Self {
            id: format!("{source}:{full_id}"),
            attribute: super::package_attribute(system, &full_id),
            name: meta.pname,
            version: meta.version,
            description: meta.description,
            source: source.to_string(),
            reference: report.locked_reference.clone(),
            revision: report.revision.clone(),
            system: system.to_string(),
            stale: report.status == SourceStatus::Stale,
            support: None,
            install_command: None,
            command: None,
        }
    }
}

/// Normalize a native attribute path to its exposed catalog ID suffix.
///
/// Only the standard `packages.<system>.` and `legacyPackages.<system>.`
/// prefixes for the *current* system are removed, so copied search IDs
/// round-trip to install while nested Nixpkgs paths such as
/// `python3Packages.foo` stay explicit. The full native attribute is kept
/// separately for matching.
#[must_use]
pub fn exposed_attribute<'a>(attr: &'a str, system: &str) -> &'a str {
    strip_output_set(attr, Some(system))
}

/// Strip one standard output-set prefix from a native attribute path.
///
/// Removes a leading `packages.<system>.` or `legacyPackages.<system>.`
/// segment pair for the given system, or whatever system it names when
/// `system` is `None`: search keys come from the current system, so the
/// prefix identifies the output set, not a user choice. No other rewriting
/// happens, and a nested path such as `python312Packages.pip` stays intact.
fn strip_output_set<'a>(attr: &'a str, system: Option<&str>) -> &'a str {
    let mut parts = attr.splitn(3, '.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("packages" | "legacyPackages"), Some(named), Some(tail))
            if !named.is_empty()
                && !tail.is_empty()
                && system.is_none_or(|system| system == named) =>
        {
            tail
        }
        _ => attr,
    }
}

/// Return an attribute's short suffix unless another attribute shares it.
///
/// `packages.<system>.probe` and `legacyPackages.<system>.probe` share
/// `probe`, so both keep their full paths. This keeps IDs unambiguous in
/// search results, `info`, and `install`.
/// Pass the current system for search and bare-name resolution. Pass `None`
/// for exact lookups, whose matches already identify their system.
fn exposed_form<'a>(
    attr: &'a str,
    claimed: &BTreeMap<&'a str, usize>,
    system: Option<&str>,
) -> &'a str {
    let exposed = strip_output_set(attr, system);
    if claimed[exposed] > 1 { attr } else { exposed }
}

/// Claim counts for the short forms of every attribute in the universe.
fn claimed_short_forms<'a>(universe: &[&'a str], system: Option<&str>) -> BTreeMap<&'a str, usize> {
    let mut claimed: BTreeMap<&str, usize> = BTreeMap::new();
    for attr in universe {
        *claimed.entry(strip_output_set(attr, system)).or_insert(0) += 1;
    }
    claimed
}

pub(super) fn unique_exposed_forms<'a>(attrs: &[&'a str], system: Option<&str>) -> Vec<&'a str> {
    let claimed = claimed_short_forms(attrs, system);
    attrs
        .iter()
        .map(|attr| exposed_form(attr, &claimed, system))
        .collect()
}

/// Exact attribute matching inside native search results.
///
/// Preference order: the full native attribute path, then the exposed form
/// with the standard output-set prefix removed, so IDs copied from search
/// rows round-trip for top-level and nested attributes such as
/// `python312Packages.pip`. Only when no exact attribute form matches does a
/// final-segment or pname fallback apply, and that fallback can return
/// several rows. Every match is returned; the caller resolves the set or
/// refuses the ambiguity.
fn exact_attribute_matches(
    results: &BTreeMap<String, SearchMeta>,
    attribute: &str,
) -> Vec<(String, SearchMeta)> {
    if let Some(meta) = results.get(attribute) {
        return vec![(attribute.to_string(), meta.clone())];
    }
    let exposed: Vec<(String, SearchMeta)> = results
        .iter()
        .filter(|(attr, _)| strip_output_set(attr, None) == attribute)
        .map(|(attr, meta)| (attr.clone(), meta.clone()))
        .collect();
    if !exposed.is_empty() {
        return exposed;
    }
    results
        .iter()
        .filter(|(attr, meta)| {
            attr.rsplit('.').next() == Some(attribute) || meta.pname == attribute
        })
        .map(|(attr, meta)| (attr.clone(), meta.clone()))
        .collect()
}

/// Prepare complete current Nixpkgs search data before reporting success.
///
/// A NixOS channel source reads the list that the channel publishes.
/// Other sources, and a channel without a list, are evaluated on this
/// computer.
/// # Errors
///
/// Returns an error when the download or evaluation fails or is stopped,
/// the source lacks immutable provenance, or the cache cannot be written.
pub fn prepare_nixpkgs(
    nix: &Nix,
    cache_dir: &Path,
    source: &str,
    system: &str,
) -> Result<(), CatalogError> {
    if let Some(channel) = super::published::channel(source)
        && super::published::prepare(cache_dir, source, system, channel)?
    {
        return Ok(());
    }
    let identity = nix.refresh_source_identity(source)?;
    prepare_source(nix, cache_dir, source, system, &identity)
}

/// Prepare a complete current snapshot with a local evaluation.
fn prepare_source(
    nix: &Nix,
    cache_dir: &Path,
    source: &str,
    system: &str,
    identity: &crate::nix::SourceIdentity,
) -> Result<(), CatalogError> {
    super::native::Snapshot::current(nix, cache_dir, source, system, identity)?;
    super::local::mark_checked(cache_dir, source, system, identity)
}

/// The row metadata shape for any index entry.
///
/// Nullable index strings become the token (name) and empty strings
/// (version, description), matching the native lane's non-nullable row
/// fields; `info` keeps the nullable values.
#[must_use]
pub fn catalog_meta(token: &str, entry: &CatalogEntry) -> SearchMeta {
    SearchMeta {
        pname: entry.name.clone().unwrap_or_else(|| token.to_string()),
        version: entry.version.clone().unwrap_or_default(),
        description: entry.description.clone().unwrap_or_default(),
    }
}

/// One exact native match for `info` identity.
///
/// `attribute` is the actual full native attribute that matched, such as
/// `packages.aarch64-darwin.default`; installed identity is compared against
/// it, never against the short request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactMatch {
    /// The actual full matched native attribute.
    pub attribute: String,
    /// The evaluated metadata at that attribute.
    pub meta: SearchMeta,
}

/// A provenance report from one resolved source identity.
#[must_use]
pub fn report_for(moving_source: &str, identity: &crate::nix::SourceIdentity) -> SourceReport {
    SourceReport {
        source: moving_source.to_string(),
        display: Some(identity.display.clone()),
        locked_reference: identity.locked_url.clone(),
        revision: identity.revision.clone(),
        status: SourceStatus::Fresh,
        detail: None,
    }
}

/// Exact attribute lookup for `info`.
///
/// Searches the locked reference with the escaped attribute and returns the
/// actual full matched attribute. The query selects only this output. It
/// does not evaluate the whole catalog. Native evaluation failures retain
/// their diagnostics. A successful query with no exact row is a distinct
/// missing-attribute outcome; pkg never invents a package identity for a miss.
/// # Errors
///
/// Returns an error when source metadata cannot be read, the exact attribute is missing or ambiguous, or Nix evaluation fails.
pub fn exact_lookup(
    nix: &Nix,
    moving_source: &str,
    attribute: &str,
) -> Result<(ExactMatch, SourceReport), CatalogError> {
    let identity = nix
        .source_identity(moving_source)
        .map_err(CatalogError::from)?;
    let search_ref = identity
        .locked_url
        .clone()
        .unwrap_or_else(|| moving_source.to_string());
    let selected_output = format!("{search_ref}#{attribute}");
    let results = nix.search(&selected_output, ".").map_err(|error| {
        match super::choice::unusable(attribute, error) {
            // The source has no such attribute: that is a missing package.
            CatalogError::Unusable { cause, .. }
                if cause.contains("does not provide attribute") =>
            {
                CatalogError::NotFound(selected_output.clone())
            }
            error => error,
        }
    })?;
    let report = report_for(moving_source, &identity);
    let exact_matches = exact_attribute_matches(&results, attribute);
    if exact_matches.len() > 1 {
        // Choices must separate the namespaces that share a short name:
        // `packages.<system>.probe` and `legacyPackages.<system>.probe`
        // stay full paths, so repeating a choice resolves it instead of
        // reproducing the ambiguity.
        let attrs: Vec<&str> = exact_matches
            .iter()
            .map(|(attr, _)| attr.as_str())
            .collect();
        return Err(CatalogError::Ambiguous {
            name: attribute.to_string(),
            choices: unique_exposed_forms(&attrs, None)
                .into_iter()
                .map(str::to_string)
                .collect(),
        });
    }
    match exact_matches.into_iter().next() {
        Some((matched, meta)) => Ok((
            ExactMatch {
                attribute: matched,
                meta,
            },
            report,
        )),
        None => Err(CatalogError::NotFound(format!(
            "{moving_source}#{attribute}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_names_and_json_statuses_remain_stable() {
        assert_eq!(SourceKind::Nixpkgs.label(), "nixpkgs");
        assert_eq!(SourceKind::Cask.label(), "cask");
        for (status, expected) in [
            (SourceStatus::Fresh, "fresh"),
            (SourceStatus::Cached, "cached"),
            (SourceStatus::Stale, "stale"),
            (SourceStatus::Failed, "failed"),
            (SourceStatus::SkippedPlatform, "skipped-platform"),
        ] {
            assert_eq!(status.label(), expected);
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{expected}\"")
            );
        }
    }

    #[test]
    fn exposed_attribute_strips_only_standard_prefixes() {
        let system = "aarch64-darwin";
        assert_eq!(
            exposed_attribute("packages.aarch64-darwin.cursor", system),
            "cursor"
        );
        assert_eq!(
            exposed_attribute("legacyPackages.aarch64-darwin.fd", system),
            "fd"
        );
        // Nested paths survive so they stay installable and unambiguous.
        assert_eq!(
            exposed_attribute(
                "legacyPackages.aarch64-darwin.python312Packages.requests",
                system
            ),
            "python312Packages.requests"
        );
        // A different system's prefix is not stripped.
        assert_eq!(
            exposed_attribute("legacyPackages.x86_64-linux.fd", system),
            "legacyPackages.x86_64-linux.fd"
        );
        // Bare attributes pass through.
        assert_eq!(exposed_attribute("cursor", system), "cursor");
    }

    #[test]
    fn exact_attribute_matching_round_trips_search_ids() {
        // Keys follow the captured native search shape: full attribute
        // paths under a standard output-set prefix.
        let meta = |pname: &str, version: &str| SearchMeta {
            pname: String::from(pname),
            version: String::from(version),
            description: String::new(),
        };
        let mut results = BTreeMap::new();
        results.insert(
            String::from("legacyPackages.aarch64-darwin.ripgrep"),
            meta("ripgrep", "14.1.0"),
        );
        results.insert(
            String::from("legacyPackages.aarch64-darwin.gnugrep"),
            meta("grep", "3.11"),
        );
        results.insert(
            String::from("legacyPackages.aarch64-darwin.python312Packages.pip"),
            meta("pip", "25.0"),
        );
        results.insert(
            String::from("legacyPackages.aarch64-darwin.python39Packages.pip"),
            meta("pip", "24.0"),
        );

        // A full native attribute path matches exactly.
        let full = exact_attribute_matches(&results, "legacyPackages.aarch64-darwin.ripgrep");
        assert_eq!(full.len(), 1);

        // The exposed top-level ID shown by search rows round-trips.
        let top = exact_attribute_matches(&results, "ripgrep");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].0, "legacyPackages.aarch64-darwin.ripgrep");

        // A nested exposed ID shown by search rows round-trips too.
        let nested = exact_attribute_matches(&results, "python312Packages.pip");
        assert_eq!(nested.len(), 1);
        assert_eq!(
            nested[0].0,
            "legacyPackages.aarch64-darwin.python312Packages.pip"
        );

        // The tail fallback can return several rows; callers refuse them.
        let tail = exact_attribute_matches(&results, "pip");
        assert_eq!(tail.len(), 2);

        // A pname fallback still matches when the tail does not.
        let pname = exact_attribute_matches(&results, "grep");
        assert_eq!(pname.len(), 1);
        assert_eq!(pname[0].0, "legacyPackages.aarch64-darwin.gnugrep");

        // No match stays empty.
        assert_eq!(
            exact_attribute_matches(&results, "fd"),
            [] as [(String, SearchMeta); 0]
        );
    }

    /// Keep full paths for attributes that share a short name across output sets.
    #[test]
    fn shared_short_names_keep_full_paths_in_ids_and_choices() {
        let system = "x86_64-linux";
        let attrs = [
            "packages.x86_64-linux.probe",
            "legacyPackages.x86_64-linux.probe",
            "packages.x86_64-linux.cursor",
        ];

        // The colliding pair keeps full paths; the unique name stays short.
        let forms = unique_exposed_forms(&attrs, Some(system));
        assert_eq!(
            forms,
            vec![
                "packages.x86_64-linux.probe",
                "legacyPackages.x86_64-linux.probe",
                "cursor",
            ]
        );

        // The exact lookup strips whatever system the match names, so the
        // same collision rule applies through `None`.
        let forms = unique_exposed_forms(&attrs, None);
        assert_eq!(
            forms,
            vec![
                "packages.x86_64-linux.probe",
                "legacyPackages.x86_64-linux.probe",
                "cursor",
            ]
        );
    }
}
