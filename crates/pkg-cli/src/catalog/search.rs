//! Discovery queries with provenance, and exact lookups for `info`.
//!
//! Every search row carries the locked reference and revision it was
//! evaluated from, and every source query reports its own health. Rows
//! are produced by [`search_source`]; single exact matches for `info`
//! come from [`exact_lookup`] and [`exact_lookup_in`].

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::Sources;
use crate::nix::{CatalogEntry, Nix, SearchMeta};

use super::CatalogError;
use super::cache::{self, CachedSearch};
use super::cask::{self, CatalogOnce};
use super::id::{CatalogId, escape_regex};

/// One resolved row set: results, the reference they were read from, its
/// revision, and whether the rows are stale cache reuse.
struct LaneOutcome {
    results: BTreeMap<String, SearchMeta>,
    reference: String,
    revision: Option<String>,
    stale: bool,
}

/// The live half of one lane, run only on a cache miss.
///
/// `Fresh` rows refresh the cache, `Skipped` marks the honest platform
/// skip (an outcome of its own, never a disguised failure), and `Failed`
/// enters the stale/failed fallback.
enum LiveRows {
    Fresh(BTreeMap<String, SearchMeta>),
    Skipped(Vec<String>),
    Failed(crate::nix::NixError),
}

/// Which supported source a discovery query ran against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        match self {
            Self::Nixpkgs => "nixpkgs",
            Self::Cask => "cask",
        }
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
    /// Why cask support badges are absent, when they are.
    pub support_detail: Option<String>,
}

impl SourceReport {
    /// The report for a source this system cannot query at all.
    ///
    /// The target list comes from the catalog index envelope itself, so a
    /// system outside the targets is an honest skip, never a disguised
    /// network failure.
    #[must_use]
    pub fn skipped_platform(source: &str, targets: &[String]) -> Self {
        Self {
            source: source.to_string(),
            display: None,
            locked_reference: None,
            revision: None,
            status: SourceStatus::SkippedPlatform,
            detail: Some(format!("cask catalog targets {} only", targets.join(", "))),
            support_detail: None,
        }
    }
}

/// The outcome of one source query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceStatus {
    /// Live results produced or a revision-exact cache was reused.
    Fresh,
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
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Failed => "failed",
            Self::SkippedPlatform => "skipped-platform",
        }
    }
}

/// Support badge for a cask row in discovery results.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum SupportBadge {
    /// The token is eligible on this system.
    ///
    /// Eligible is the generated metadata status; it is not a verification
    /// promise.
    Eligible,
    /// The token is excluded on this system.
    Excluded {
        /// A short machine-readable reason.
        reason: String,
        /// Human-readable detail for the exclusion, when present.
        detail: Option<String>,
    },
}

/// One search result row after routing, with provenance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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
    /// The source label (`nixpkgs` or `cask`).
    pub source: String,
    /// The locked reference this row was evaluated from.
    pub reference: Option<String>,
    /// The locked revision of that reference.
    pub revision: Option<String>,
    /// The system this row was evaluated for.
    pub system: String,
    /// Whether the row was reused from stale cache data.
    pub stale: bool,
    /// Cask support classification, for cask rows.
    pub support: Option<SupportBadge>,
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
    for prefix in ["packages", "legacyPackages"] {
        let Some(rest) = attr.strip_prefix(prefix) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix('.') else {
            continue;
        };
        let Some((named, tail)) = rest.split_once('.') else {
            continue;
        };
        if named.is_empty() || tail.is_empty() {
            continue;
        }
        if system.is_some_and(|system| system != named) {
            continue;
        }
        return tail;
    }
    attr
}

/// Resolve a bare name to exactly one supported output.
///
/// The Nixpkgs lane runs the anchored native search as before. The cask
/// lane resolves through the generated index: the name must equal one
/// token exactly (fully anchored, token only — a display name is never a
/// resolution). `catalog` is the shared once-per-command index handle, so
/// a bare name and a later gate or info lookup evaluate the index once.
///
/// On a system the catalog does not target, the cask lane contributes no
/// choices at all, so it can never be a fatal requirement there.
pub fn resolve_bare(
    nix: &Nix,
    sources: &Sources,
    name: &str,
    system: &str,
    catalog: &mut CatalogOnce<'_>,
) -> Result<CatalogId, CatalogError> {
    let pattern = format!("^{}$", escape_regex(name));
    let mut choices = Vec::new();
    let nixpkgs = nix
        .search(&sources.nixpkgs, &pattern)
        .map_err(CatalogError::from)?;
    for attr in exact_attribute_matches(&nixpkgs, name)
        .into_iter()
        .map(|(attr, _)| attr)
    {
        // Choices are exposed as the normalized suffix (matching search
        // rows); the full native attribute is resolved again at info time.
        choices.push(CatalogId::Nixpkgs(
            exposed_attribute(&attr, system).to_string(),
        ));
    }
    let view = catalog.load();
    if let Ok(view) = view
        && view.targeted(system)
        && view.exact_token(system, name).is_some()
    {
        // Bare cask resolution is the exact token; nothing else matches.
        choices.push(CatalogId::Cask(name.to_string()));
    }
    match choices.len() {
        1 => Ok(choices.swap_remove(0)),
        0 => Err(CatalogError::NotFound(name.to_string())),
        _ => Err(CatalogError::Ambiguous {
            name: name.to_string(),
            choices: choices.iter().map(CatalogId::qualified).collect(),
        }),
    }
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

/// Query one source for `query`, using and refreshing the disposable cache.
///
/// Discovery resolves the moving source to a locked reference first and runs
/// the native search against the locked reference, so rows and their revision
/// label always describe the same revision. A cache is reused only when the
/// freshly resolved revision equals the cached revision; a failed metadata
/// fetch is never treated as proof of freshness. On a live failure, cached
/// rows for the same source, system, and query are returned flagged stale
/// with the failure recorded.
#[must_use]
pub fn search_source(
    nix: &Nix,
    cache_dir: &Path,
    moving_source: &str,
    query: &str,
    system: &str,
    kind: SourceKind,
) -> (Vec<SearchResult>, SourceReport) {
    // The native query is a regex; pkg does not substring-filter results.
    let (outcome, report) = lane_outcome(
        nix,
        cache_dir,
        moving_source,
        query,
        system,
        "live query failed",
        |search_ref| {
            nix.search(search_ref, query)
                .map(LiveRows::Fresh)
                .unwrap_or_else(LiveRows::Failed)
        },
    );
    match outcome {
        Some(lane) => (
            rows_from(
                &lane.results,
                kind,
                &lane.reference,
                &lane.revision,
                system,
                lane.stale,
            ),
            report,
        ),
        None => (Vec::new(), report),
    }
}

/// Query the cask catalog index for `query` with local filtering.
///
/// The lane evaluates the generated `catalogIndex` attribute once from the
/// locked reference of the moving source — the same source identity and
/// revision rules as the native lane — and filters locally. No package
/// derivation is forced. The pattern is a Rust regex (documented in
/// docs/commands.md), compiled case-insensitively and matched against the
/// token, the name, and the description of each entry; only eligible
/// entries are returned, so search rows stay installable and unambiguous.
/// Excluded tokens stay discoverable through `info` with their reason.
///
/// The platform rule is read from the envelope: a system outside `targets`
/// is skipped with the target list shown, and an index evaluation failure
/// is a failure — never a platform skip.
#[must_use]
pub fn search_catalog(
    nix: &Nix,
    cache_dir: &Path,
    moving_source: &str,
    query: &str,
    system: &str,
) -> (Vec<SearchResult>, SourceReport) {
    let (outcome, report) = lane_outcome(
        nix,
        cache_dir,
        moving_source,
        query,
        system,
        "live index query failed",
        |eval_ref| match nix.catalog_index(eval_ref) {
            // The platform rule comes from the envelope alone: an
            // untargeted system is skipped with the target list shown.
            Ok(index) if !index.targets.iter().any(|target| target == system) => {
                LiveRows::Skipped(index.targets)
            }
            Ok(index) => filter_catalog(&index, system, query)
                .map(LiveRows::Fresh)
                .unwrap_or_else(LiveRows::Failed),
            Err(error) => LiveRows::Failed(error),
        },
    );
    match outcome {
        Some(lane) => (
            catalog_rows_from(
                &lane.results,
                &lane.reference,
                &lane.revision,
                system,
                lane.stale,
            ),
            report,
        ),
        None => (Vec::new(), report),
    }
}

/// Run one discovery lane end to end: identity resolution, cache reuse,
/// the live query, and the stale/failed fallback.
///
/// The moving source resolves to a locked reference first; report and rows
/// describe that identity, and a metadata failure keeps its detail. A
/// cache is reused only when the freshly resolved revision equals the
/// cached revision — proven freshness; `None == None` is not proof. On a
/// cache miss `live` evaluates exactly once: fresh rows refresh the cache,
/// a platform skip replaces the report and writes nothing, and a failure
/// reuses cached rows flagged stale or fails honestly. `live_failure`
/// names the lane in the stale and failure details.
fn lane_outcome(
    nix: &Nix,
    cache_dir: &Path,
    moving_source: &str,
    query: &str,
    system: &str,
    live_failure: &str,
    live: impl FnOnce(&str) -> LiveRows,
) -> (Option<LaneOutcome>, SourceReport) {
    let mut report = SourceReport {
        source: moving_source.to_string(),
        display: None,
        locked_reference: None,
        revision: None,
        status: SourceStatus::Fresh,
        detail: None,
        support_detail: None,
    };
    let identity = nix.source_identity(moving_source);
    let (locked, revision, display) = match &identity {
        Ok(id) => (
            id.locked_url.clone(),
            id.revision.clone(),
            Some(id.display.clone()),
        ),
        Err(error) => {
            report.detail = Some(format!("metadata fetch failed: {error}"));
            (None, None, None)
        }
    };
    report.locked_reference = locked.clone();
    report.revision = revision.clone();
    report.display = display;

    // Exactly one outcome supplies the rows; `stale` marks reused rows.
    // Fresh reuse requires a proven revision match; None == None is not proof.
    let outcome = if let (Some(rev), Some(locked_ref)) = (&revision, &locked)
        && let Some(cached) = cache::read_cache(cache_dir, moving_source, system, query)
        && cached.revision.as_ref() == Some(rev)
    {
        Some(LaneOutcome {
            results: cached.results,
            reference: locked_ref.clone(),
            revision: cached.revision,
            stale: false,
        })
    } else {
        let eval_ref = locked.clone().unwrap_or_else(|| moving_source.to_string());
        match live(&eval_ref) {
            LiveRows::Fresh(results) => {
                let _ = cache::write_cache(
                    cache_dir,
                    &CachedSearch {
                        schema: cache::CACHE_SCHEMA,
                        source: moving_source.to_string(),
                        locked_reference: locked.clone(),
                        revision: revision.clone(),
                        system: system.to_string(),
                        query: query.to_string(),
                        saved_unix: cache::now_unix(),
                        results: results.clone(),
                    },
                );
                Some(LaneOutcome {
                    results,
                    reference: eval_ref,
                    revision: revision.clone(),
                    stale: false,
                })
            }
            LiveRows::Skipped(targets) => {
                report = SourceReport::skipped_platform(moving_source, &targets);
                None
            }
            LiveRows::Failed(error) => {
                match cache::read_cache(cache_dir, moving_source, system, query) {
                    Some(cached) => {
                        report.status = SourceStatus::Stale;
                        report.detail = Some(format!("{live_failure}: {error}"));
                        Some(LaneOutcome {
                            results: cached.results,
                            reference: cached
                                .locked_reference
                                .clone()
                                .unwrap_or_else(|| moving_source.to_string()),
                            revision: cached.revision,
                            stale: true,
                        })
                    }
                    None => {
                        report.status = SourceStatus::Failed;
                        report.detail = Some(format!("{live_failure}: {error}"));
                        None
                    }
                }
            }
        }
    };
    (outcome, report)
}

/// Filter the eligible entries of one loaded index with the query pattern.
///
/// The catalog targets `system` (checked by the caller). An invalid regex is
/// an honest error; the pattern grammar is documented, so a broken pattern
/// is reported instead of silently matching nothing.
fn filter_catalog(
    index: &crate::nix::CatalogIndex,
    system: &str,
    query: &str,
) -> Result<BTreeMap<String, SearchMeta>, crate::nix::NixError> {
    let empty = BTreeMap::new();
    let entries = index
        .systems
        .get(system)
        .map(|section| &section.entries)
        .unwrap_or(&empty);
    let pattern = regex::RegexBuilder::new(query)
        .case_insensitive(true)
        .build()
        .map_err(|error| crate::nix::NixError::Decode {
            what: "cask catalog search pattern",
            detail: format!("invalid regex `{query}`: {error}"),
        })?;
    let mut results = BTreeMap::new();
    for (token, entry) in entries {
        if entry.status != crate::nix::EntryStatus::Eligible {
            continue;
        }
        let matches = pattern.is_match(token)
            || entry
                .name
                .as_deref()
                .is_some_and(|name| pattern.is_match(name))
            || entry
                .description
                .as_deref()
                .is_some_and(|description| pattern.is_match(description));
        if matches {
            results.insert(token.clone(), catalog_meta(token, entry));
        }
    }
    Ok(results)
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

pub(super) fn rows_from(
    results: &BTreeMap<String, SearchMeta>,
    kind: SourceKind,
    reference: &str,
    revision: &Option<String>,
    system: &str,
    stale: bool,
) -> Vec<SearchResult> {
    results
        .iter()
        .map(|(attr, meta)| SearchResult {
            id: format!("{}:{}", kind.label(), exposed_attribute(attr, system)),
            attribute: attr.clone(),
            name: meta.pname.clone(),
            version: meta.version.clone(),
            description: meta.description.clone(),
            source: kind.label().to_string(),
            reference: Some(reference.to_string()),
            revision: revision.clone(),
            system: system.to_string(),
            stale,
            support: None,
        })
        .collect()
}

/// Build cask search rows from locally filtered index results.
///
/// Keys are catalog tokens; every row points at the ordinary
/// `packages.<system>.<token>` attribute the token installs through, and
/// every row is eligible by construction of the filter.
pub(super) fn catalog_rows_from(
    results: &BTreeMap<String, SearchMeta>,
    reference: &str,
    revision: &Option<String>,
    system: &str,
    stale: bool,
) -> Vec<SearchResult> {
    results
        .iter()
        .map(|(token, meta)| SearchResult {
            id: format!("cask:{token}"),
            attribute: cask::package_attribute(system, token),
            name: meta.pname.clone(),
            version: meta.version.clone(),
            description: meta.description.clone(),
            source: SourceKind::Cask.label().to_string(),
            reference: Some(reference.to_string()),
            revision: revision.clone(),
            system: system.to_string(),
            stale,
            support: Some(SupportBadge::Eligible),
        })
        .collect()
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
        support_detail: None,
    }
}

/// Exact attribute lookup for `info`.
///
/// Searches the locked reference with the escaped attribute and returns the
/// actual full matched attribute. A source failure and a missing attribute
/// are distinct outcomes; pkg never invents a package identity for a miss.
pub fn exact_lookup(
    nix: &Nix,
    moving_source: &str,
    attribute: &str,
) -> Result<(ExactMatch, SourceReport), CatalogError> {
    let identity = nix
        .source_identity(moving_source)
        .map_err(CatalogError::from)?;
    exact_lookup_in(nix, moving_source, &identity, attribute)
}

/// Exact attribute lookup against an already-resolved source identity.
///
/// Sharing one identity keeps the cask support decision and the package
/// metadata of a single info result on the same locked source. The lookup
/// is targeted: `nix search <locked>#<attribute> .` makes Nix resolve the
/// selected output itself — including `packages`/`legacyPackages` and
/// nested paths — so the whole catalog is never evaluated for one exact
/// ID. The verified runtime returns exactly one record whose key is the
/// full native attribute path; the shared matcher still decides which row
/// is exact and refuses several rows as an ambiguity instead of silently
/// picking the first one.
///
/// A missing or non-package attribute is a native search failure here:
/// its upstream diagnostic is preserved through [`CatalogError::Source`]
/// rather than being synthesized as a quiet `NotFound`. `NotFound` remains
/// the outcome when the native search succeeds but no row is an exact
/// match for the requested attribute.
pub fn exact_lookup_in(
    nix: &Nix,
    moving_source: &str,
    identity: &crate::nix::SourceIdentity,
    attribute: &str,
) -> Result<(ExactMatch, SourceReport), CatalogError> {
    let search_ref = identity
        .locked_url
        .clone()
        .unwrap_or_else(|| moving_source.to_string());
    let selected_output = format!("{search_ref}#{attribute}");
    let results = nix
        .search(&selected_output, ".")
        .map_err(CatalogError::from)?;
    let report = report_for(moving_source, identity);
    let matches = exact_attribute_matches(&results, attribute);
    if matches.len() > 1 {
        return Err(CatalogError::Ambiguous {
            name: attribute.to_string(),
            choices: matches
                .iter()
                .map(|(attr, _)| strip_output_set(attr, None).to_string())
                .collect(),
        });
    }
    match matches.into_iter().next() {
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

    /// The catalog lane filters the generated index locally: eligible
    /// entries only, Rust-regex semantics, matched case-insensitively
    /// against token, name, and description. Data is the actual catalog
    /// slice fixture.
    #[test]
    fn catalog_lane_filters_eligible_entries_locally() {
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/catalog-index.json"))
                .expect("valid json");
        let index = crate::nix::decode_catalog_index(&raw).expect("decodes fixture");

        // Token, name, and description each match, case-insensitively.
        let token_hit = filter_catalog(&index, "x86_64-linux", "koreader").expect("filters");
        assert!(token_hit.contains_key("koreader"));
        let name_hit = filter_catalog(&index, "x86_64-linux", "1password").expect("filters");
        assert!(name_hit.contains_key("1password-cli"));
        let desc_hit = filter_catalog(&index, "aarch64-darwin", "command-line").expect("filters");
        assert!(desc_hit.contains_key("1password-cli"));

        // Search shows eligible entries only: the excluded `iterm2` on
        // Linux and the installer-script `zoom` never appear.
        let any = filter_catalog(&index, "x86_64-linux", ".").expect("filters");
        assert!(any.contains_key("koreader"));
        assert!(!any.contains_key("iterm2"));
        assert!(!any.contains_key("zoom"));
        // An excluded token stays discoverable through info instead; the
        // `@` and `+` tokens stay searchable when eligible.
        let macos_any = filter_catalog(&index, "aarch64-darwin", ".").expect("filters");
        assert!(!macos_any.contains_key("zoom"));
        assert!(macos_any.contains_key("1password@7"));
        assert!(macos_any.contains_key("4k-video-downloader+"));

        // A broken pattern is an honest error, not a silent empty result.
        let error = filter_catalog(&index, "x86_64-linux", "(").expect_err("invalid regex");
        assert!(error.to_string().contains("invalid regex"));

        // Rows point at the ordinary package attribute of each token.
        let rows = catalog_rows_from(
            &token_hit,
            "locked-ref",
            &Some(String::from("rev")),
            "x86_64-linux",
            false,
        );
        let row = &rows[0];
        assert_eq!(row.id, "cask:koreader");
        assert_eq!(row.attribute, "packages.x86_64-linux.koreader");
        assert_eq!(row.support, Some(SupportBadge::Eligible));
        assert_eq!(row.source, "cask");
        // Row shape carries non-nullable strings; `info` keeps the nullable
        // values.
        assert_eq!(row.name, "KOReader");
        assert_eq!(row.version, "2026.07.1");
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
        assert!(exact_attribute_matches(&results, "fd").is_empty());
    }
}
