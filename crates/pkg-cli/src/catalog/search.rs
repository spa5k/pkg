//! Discovery queries with provenance, and exact lookups for `info`.
//!
//! Every search row carries the locked reference and revision it was
//! evaluated from, and every source query reports its own health. Rows
//! are produced by [`search_source`]; single exact matches for `info`
//! come from [`exact_lookup`] and [`exact_lookup_in`].

use std::collections::BTreeMap;
use std::path::Path;

use crate::nix::{CatalogEntry, Nix, SearchMeta};
use crate::tap::SavedCatalog;

use super::CatalogError;
use super::cache;
use super::cask::{self, CatalogOnce};
use super::id::{CatalogId, escape_regex};
use super::routing::Routing;

/// One resolved row set for a lane: the metadata map it supplies, the
/// reference the rows describe, its revision, and whether the rows are
/// stale cache reuse. For the nixpkgs lane the map is the complete
/// revision snapshot; the caller filters it with the query pattern.
struct LaneOutcome {
    results: BTreeMap<String, SearchMeta>,
    reference: String,
    revision: Option<String>,
    stale: bool,
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

/// The exposed suffix of every attribute in one result set, with full
/// paths kept where short forms would collide.
///
/// `packages.<system>.probe` and `legacyPackages.<system>.probe` expose
/// the same short name. Two search rows, two bare-name choices, and two
/// exact-lookup choices must never share one ID, and a choice must
/// round-trip through `info` and `install`, so an attribute whose short
/// form is claimed by another attribute keeps its full path. Uniquely
/// short names stay short. `system` follows [`strip_output_set`]: the
/// current system for search rows and bare resolution, `None` for the
/// exact lookup, whose matches already name their own system.
/// The short (exposed) suffix of one attribute, with the claim map used to
/// decide whether that suffix is safe to expose.
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

fn unique_exposed_forms<'a>(attrs: &[&'a str], system: Option<&str>) -> Vec<&'a str> {
    let claimed = claimed_short_forms(attrs, system);
    attrs
        .iter()
        .map(|attr| exposed_form(attr, &claimed, system))
        .collect()
}

/// Exposed forms for a filtered result set, counted against the FULL
/// source namespace (`universe`), not the filtered rows.
///
/// A query that matches only one namespace's description must not earn
/// the short form: the same short name is still claimed by another
/// attribute in the full snapshot, and a short ID would map to a
/// different or ambiguous package through `info` and `install`. Claims
/// are therefore counted over the universe, and each matched result is
/// exposed through `exposed_form` with those full-snapshot claims.
/// Resolve a bare name to exactly one supported output.
///
/// The Nixpkgs lane runs an anchored native search against the moving
/// source: bare install and info resolution stays fail-closed and never
/// answers from the discovery snapshot cache. The cask
/// lane resolves through the generated index of the official source and
/// the saved catalogs of every registered tap: the name must equal one
/// token exactly (fully anchored, token only — a display name is never a
/// resolution), whatever the entry's status is, because the bare form
/// cannot say which source the user meant. Bare resolution fails closed:
/// an unreadable official index or a failed saved source is an error, never
/// a silent answer from the remaining sources. `catalog` is the shared
/// once-per-command index handle for the official source.
///
/// On a system the catalogs do not target, a source contributes no
/// choices at all, so it can never be a fatal requirement there.
pub fn resolve_bare(
    nix: &Nix,
    routing: &Routing,
    saved: &crate::tap::SavedCatalogs,
    name: &str,
    system: &str,
    catalog: &mut CatalogOnce<'_>,
) -> Result<CatalogId, CatalogError> {
    resolve_inner(nix, Some(&routing.nixpkgs), saved, name, system, catalog)
}

/// Resolve one bare cask token (`cask:token`) across every cask source.
///
/// The official source and every registered tap count, including excluded
/// entries. Exactly one source may carry the token; more than one is an
/// ambiguity that names the full `cask:owner/tap/token` form, and none is
/// a miss. Failed sources fail the resolution instead of being skipped.
pub fn resolve_bare_cask(
    nix: &Nix,
    saved: &crate::tap::SavedCatalogs,
    token: &str,
    system: &str,
    catalog: &mut CatalogOnce<'_>,
) -> Result<CatalogId, CatalogError> {
    resolve_inner(nix, None, saved, token, system, catalog)
}

/// The shared resolver behind both bare forms.
///
/// `nixpkgs` names the configured Nixpkgs source to also search, or
/// `None` for the cask-only form.
fn resolve_inner(
    nix: &Nix,
    nixpkgs: Option<&str>,
    saved: &crate::tap::SavedCatalogs,
    name: &str,
    system: &str,
    catalog: &mut CatalogOnce<'_>,
) -> Result<CatalogId, CatalogError> {
    let mut choices = Vec::new();
    if let Some(nixpkgs) = nixpkgs {
        let pattern = format!("^{}$", escape_regex(name));
        let nixpkgs = nix.search(nixpkgs, &pattern).map_err(CatalogError::from)?;
        let matches = exact_attribute_matches(&nixpkgs, name);
        let attrs: Vec<&str> = matches.iter().map(|(attr, _)| attr.as_str()).collect();
        // Choices are exposed as the normalized suffix when that suffix
        // is unique, and as the full native attribute when `packages` and
        // `legacyPackages` expose the same short name; either form
        // round-trips through info and install.
        for attr in unique_exposed_forms(&attrs, Some(system)) {
            choices.push(CatalogId::Nixpkgs(attr.to_string()));
        }
    }
    // The cask lane: the official index is evaluated (fail closed) and
    // every saved tap catalog counts, including excluded entries.
    let view = catalog.load()?;
    let mut sources = view.sources_with_token(system, name);
    sources.extend(saved.sources_with_token(system, name));
    sources.sort();
    sources.dedup();
    match sources.len() {
        0 => {}
        1 => choices.push(CatalogId::Cask {
            source: sources.swap_remove(0),
            token: name.to_string(),
        }),
        _ => {
            return Err(CatalogError::Ambiguous {
                name: name.to_string(),
                choices: sources
                    .iter()
                    .map(|source| format!("cask:{source}/{name}"))
                    .collect(),
            });
        }
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

/// Compile the one shared query pattern.
///
/// Both lanes use the Rust regex engine, compiled case-insensitively; an
/// invalid pattern is an honest error reported before any evaluation runs.
fn compile_pattern(query: &str) -> Result<regex::Regex, crate::nix::NixError> {
    regex::RegexBuilder::new(query)
        .case_insensitive(true)
        .build()
        .map_err(|error| crate::nix::NixError::Decode {
            what: "search pattern",
            detail: format!("invalid regex `{query}`: {error}"),
        })
}

/// Native search matching semantics, applied locally.
///
/// Native `nix search` (Nix 2.35.2, src/nix/search.cc) matches each
/// pattern against three strings per derivation: the **full attribute
/// path** (for example `legacyPackages.x86_64-linux.python312Packages.requests`),
/// the derivation **name with the version stripped** (which is exactly the
/// `pname` the JSON already reports — it is never reparsed here), and the
/// **description** (newlines replaced by spaces upstream, an absent or
/// empty description is the empty string, which `^$` matches natively).
/// Patterns are unanchored and case-insensitive; the exposed catalog ID
/// is never the matched string — the full attribute is.
fn matches_native(pattern: &regex::Regex, attr: &str, meta: &SearchMeta) -> bool {
    pattern.is_match(attr) || pattern.is_match(&meta.pname) || pattern.is_match(&meta.description)
}

/// Query one source for `query`, using and refreshing the revision snapshot.
///
/// Discovery resolves the moving source to a locked reference first. The
/// complete metadata map for (source, system) is read from the one latest
/// snapshot — fetched once with `nix search <locked> ^ --json` on a miss,
/// and atomically replaced whenever the locked revision moves — and
/// filtered locally: the pattern is matched case-insensitively against
/// each entry's full attribute path, pname, and description, the three
/// strings native `nix search` matches. A
/// snapshot is reused only when the freshly resolved revision and locked
/// reference equal the snapshot's recorded identity; a failed metadata
/// fetch is never treated as proof of freshness. On a live failure, the
/// latest readable snapshot is returned flagged stale with the failure
/// recorded.
#[must_use]
pub fn search_source(
    nix: &Nix,
    cache_dir: &Path,
    moving_source: &str,
    query: &str,
    system: &str,
    kind: SourceKind,
) -> (Vec<SearchResult>, SourceReport) {
    // The pattern is compiled first: an invalid regex is a cheap, honest
    // failure before any child runs.
    let pattern = match compile_pattern(query) {
        Ok(pattern) => pattern,
        Err(error) => {
            return (
                Vec::new(),
                SourceReport {
                    source: moving_source.to_string(),
                    display: None,
                    locked_reference: None,
                    revision: None,
                    status: SourceStatus::Failed,
                    detail: Some(error.to_string()),
                    support_detail: None,
                },
            );
        }
    };
    let (outcome, report) = native_lane(nix, cache_dir, moving_source, system);
    match outcome {
        Some(lane) => {
            // Rows are built straight from the borrowed map: only matched
            // rows are cloned, never the whole catalog.
            let matched = lane
                .results
                .iter()
                .filter(|(attr, meta)| matches_native(&pattern, attr, meta));
            // The universe for ID claims is the complete snapshot: every
            // attribute of the locked revision, not only the matches.
            let universe: Vec<&str> = lane.results.keys().map(String::as_str).collect();
            (
                rows_from(
                    matched,
                    &universe,
                    kind,
                    &lane.reference,
                    &lane.revision,
                    system,
                    lane.stale,
                ),
                report,
            )
        }
        None => (Vec::new(), report),
    }
}

/// Supply the complete nixpkgs metadata map for one source and system.
///
/// The moving source resolves to a locked reference first; report and rows
/// describe that identity, and a metadata failure keeps its detail. A
/// snapshot is reused only when the freshly resolved revision and locked
/// reference equal the snapshot's recorded identity — proven freshness;
/// `None == None` is not proof. On a miss the complete map is fetched once
/// with `nix search <locked> ^ --json` and atomically replaces the one
/// latest snapshot (only when the revision is immutable; local path
/// sources query live every time). Any live or metadata failure reuses
/// the latest readable snapshot of the source flagged stale, or fails
/// honestly.
fn native_lane(
    nix: &Nix,
    cache_dir: &Path,
    moving_source: &str,
    system: &str,
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
    let id = match identity {
        Ok(id) => id,
        Err(error) => {
            report.detail = Some(format!("metadata fetch failed: {error}"));
            return native_stale(cache_dir, moving_source, system, report);
        }
    };
    report.locked_reference = id.locked_url.clone();
    report.revision = id.revision.clone();
    report.display = Some(id.display.clone());
    if let Some(revision) = &id.revision
        && let Some(snapshot) =
            cache::read_native_snapshot(cache_dir, moving_source, system, Some(revision))
        && snapshot.locked_reference == id.locked_url
    {
        return (
            Some(LaneOutcome {
                results: snapshot.payload,
                reference: snapshot
                    .locked_reference
                    .unwrap_or_else(|| moving_source.to_string()),
                revision: Some(revision.clone()),
                stale: false,
            }),
            report,
        );
    }
    let eval_ref = id
        .locked_url
        .clone()
        .unwrap_or_else(|| moving_source.to_string());
    match nix.search(&eval_ref, "^") {
        Ok(results) => {
            // The owned map is serialized by reference into the snapshot,
            // then consumed for filtering: no full-catalog clone.
            let results = if id.revision.is_some() {
                let snap = cache::NativeSnapshot {
                    schema: cache::SNAPSHOT_SCHEMA,
                    kind: String::from(cache::KIND_NATIVE),
                    source: moving_source.to_string(),
                    locked_reference: id.locked_url.clone(),
                    revision: id.revision.clone(),
                    system: system.to_string(),
                    saved_unix: cache::now_unix(),
                    payload: results,
                };
                let _ = cache::write_snapshot(cache_dir, &snap);
                snap.payload
            } else {
                results
            };
            (
                Some(LaneOutcome {
                    results,
                    reference: eval_ref,
                    revision: id.revision.clone(),
                    stale: false,
                }),
                report,
            )
        }
        Err(error) => {
            report.detail = Some(format!("live query failed: {error}"));
            native_stale(cache_dir, moving_source, system, report)
        }
    }
}

/// The stale/failed fallback of the native lane.
///
/// The current revision is unknown here, so the one latest snapshot of
/// the same source and system is read (revision unchecked); its own locked
/// reference and revision label the reused rows. A corrupt or missing
/// snapshot is a failure.
fn native_stale(
    cache_dir: &Path,
    moving_source: &str,
    system: &str,
    mut report: SourceReport,
) -> (Option<LaneOutcome>, SourceReport) {
    let detail = report
        .detail
        .clone()
        .unwrap_or_else(|| String::from("live query failed"));
    match cache::read_native_snapshot(cache_dir, moving_source, system, None) {
        Some(snapshot) => {
            report.status = SourceStatus::Stale;
            report.detail = Some(detail);
            report.locked_reference = snapshot.locked_reference.clone();
            report.revision = snapshot.revision.clone();
            (
                Some(LaneOutcome {
                    reference: snapshot
                        .locked_reference
                        .unwrap_or_else(|| moving_source.to_string()),
                    revision: snapshot.revision,
                    results: snapshot.payload,
                    stale: true,
                }),
                report,
            )
        }
        None => {
            report.status = SourceStatus::Failed;
            report.detail = Some(detail);
            (None, report)
        }
    }
}

/// Query the cask catalog index for `query` with local filtering.
///
/// The lane reads the complete generated `catalogIndex` envelope of the
/// locked revision of the moving source — from the revision snapshot on a
/// hit, evaluated once on a miss — and filters locally. No package
/// derivation is forced. The pattern is the same shared Rust regex as the
/// nixpkgs lane (documented in docs/commands.md), compiled case-
/// insensitively and matched against the token, the name, and the
/// description of each entry; only eligible entries are returned, so
/// search rows stay installable and unambiguous. Excluded tokens stay
/// discoverable through `info` with their reason.
///
/// The platform rule is read from the envelope: a system outside `targets`
/// is skipped with the target list shown, and an index failure is a
/// failure — never a platform skip.
#[must_use]
pub fn search_catalog(
    nix: &Nix,
    cache_dir: &Path,
    moving_source: &str,
    query: &str,
    system: &str,
) -> (Vec<SearchResult>, SourceReport) {
    // Compiled first, so an invalid pattern fails before any evaluation.
    if let Err(error) = compile_pattern(query) {
        return (
            Vec::new(),
            SourceReport {
                source: moving_source.to_string(),
                display: None,
                locked_reference: None,
                revision: None,
                status: SourceStatus::Failed,
                detail: Some(error.to_string()),
                support_detail: None,
            },
        );
    }
    let (outcome, report) = index_lane(nix, cache_dir, moving_source, query, system);
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

/// Run the cask index lane end to end.
///
/// Same identity, snapshot, and stale rules as the native lane
/// ([`native_lane`]), applied to the generated index envelope; the only
/// extra rule is the platform skip, which is read from the envelope itself
/// whichever side it came from (snapshot or live evaluation).
fn index_lane(
    nix: &Nix,
    cache_dir: &Path,
    moving_source: &str,
    query: &str,
    system: &str,
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
    let id = match nix.source_identity(moving_source) {
        Ok(id) => id,
        Err(error) => {
            report.detail = Some(format!("metadata fetch failed: {error}"));
            return index_stale(cache_dir, moving_source, query, system, report);
        }
    };
    report.locked_reference = id.locked_url.clone();
    report.revision = id.revision.clone();
    report.display = Some(id.display.clone());
    if let Some(revision) = &id.revision
        && let Some(snapshot) =
            cache::read_index_snapshot(cache_dir, moving_source, system, Some(revision))
        && snapshot.locked_reference == id.locked_url
    {
        return index_apply(
            &snapshot.payload,
            query,
            system,
            snapshot
                .locked_reference
                .unwrap_or_else(|| moving_source.to_string()),
            snapshot.revision,
            false,
            report,
        );
    }
    let eval_ref = id
        .locked_url
        .clone()
        .unwrap_or_else(|| moving_source.to_string());
    match nix.catalog_index(&eval_ref) {
        Ok(index) => {
            // Serialized by reference, then consumed: no envelope clone.
            let index = if id.revision.is_some() {
                let snap = cache::IndexSnapshot {
                    schema: cache::SNAPSHOT_SCHEMA,
                    kind: String::from(cache::KIND_INDEX),
                    source: moving_source.to_string(),
                    locked_reference: id.locked_url.clone(),
                    revision: id.revision.clone(),
                    system: system.to_string(),
                    saved_unix: cache::now_unix(),
                    payload: index,
                };
                let _ = cache::write_snapshot(cache_dir, &snap);
                snap.payload
            } else {
                index
            };
            index_apply(&index, query, system, eval_ref, id.revision, false, report)
        }
        Err(error) => {
            report.detail = Some(format!("live index query failed: {error}"));
            index_stale(cache_dir, moving_source, query, system, report)
        }
    }
}

/// Turn one index into a lane outcome: platform skip or filtered rows.
fn index_apply(
    index: &crate::nix::CatalogIndex,
    query: &str,
    system: &str,
    reference: String,
    revision: Option<String>,
    stale: bool,
    mut report: SourceReport,
) -> (Option<LaneOutcome>, SourceReport) {
    // The platform rule comes from the envelope alone: an untargeted
    // system is skipped with the target list shown, whatever supplied it.
    // But only a successfully resolved (fresh) index may say so: a stale
    // cached envelope that excludes this system cannot prove the current
    // source is unsupported, so the earlier failure stays visible as a
    // stale report with its original detail instead of a clean skip.
    if !index.targets.iter().any(|target| target == system) {
        if stale {
            report.status = SourceStatus::Stale;
            return (None, report);
        }
        return (
            None,
            SourceReport::skipped_platform(&report.source, &index.targets),
        );
    }
    if stale {
        report.status = SourceStatus::Stale;
    }
    match filter_catalog(index, system, query) {
        Ok(results) => (
            Some(LaneOutcome {
                results,
                reference,
                revision,
                stale,
            }),
            report,
        ),
        Err(error) => {
            report.status = SourceStatus::Failed;
            report.detail = Some(error.to_string());
            (None, report)
        }
    }
}

/// The stale/failed fallback of the cask index lane.
///
/// Same rule as the native stale fallback: the one latest snapshot of the
/// source is read with its revision unchecked; the reused snapshot's own
/// provenance labels the rows.
fn index_stale(
    cache_dir: &Path,
    moving_source: &str,
    query: &str,
    system: &str,
    mut report: SourceReport,
) -> (Option<LaneOutcome>, SourceReport) {
    let detail = report
        .detail
        .clone()
        .unwrap_or_else(|| String::from("live index query failed"));
    match cache::read_index_snapshot(cache_dir, moving_source, system, None) {
        Some(snapshot) => {
            report.detail = Some(detail);
            // The reused snapshot's own provenance labels the report too,
            // matching native_stale: whatever the identity step knew (or
            // failed to learn) before the failure, the rows come from this
            // snapshot, so the report describes exactly those rows.
            report.locked_reference = snapshot.locked_reference.clone();
            report.revision = snapshot.revision.clone();
            index_apply(
                &snapshot.payload,
                query,
                system,
                snapshot
                    .locked_reference
                    .unwrap_or_else(|| moving_source.to_string()),
                snapshot.revision,
                true,
                report,
            )
        }
        None => {
            report.status = SourceStatus::Failed;
            report.detail = Some(detail);
            (None, report)
        }
    }
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
    let pattern = compile_pattern(query)?;
    let mut results = BTreeMap::new();
    for (full_id, entry) in entries {
        if entry.status != crate::nix::EntryStatus::Eligible {
            continue;
        }
        let token = entry.token.as_str();
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
            results.insert(full_id.clone(), catalog_meta(token, entry));
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

pub(super) fn rows_from<'a>(
    results: impl Iterator<Item = (&'a String, &'a SearchMeta)>,
    universe: &[&str],
    kind: SourceKind,
    reference: &str,
    revision: &Option<String>,
    system: &str,
    stale: bool,
) -> Vec<SearchResult> {
    let collected: Vec<(&'a String, &'a SearchMeta)> = results.collect();
    // Search IDs never collide: claims are counted against the complete
    // snapshot (`universe`), so an attribute whose short form is shared
    // anywhere in the source keeps its full path as the ID suffix even
    // when the query matched only that one attribute.
    let claimed = claimed_short_forms(universe, Some(system));
    collected
        .into_iter()
        .map(|(attr, meta)| {
            let id_suffix = exposed_form(attr, &claimed, Some(system));
            SearchResult {
                id: format!("{}:{id_suffix}", kind.label()),
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
            }
        })
        .collect()
}

/// Build cask search rows from locally filtered index results.
///
/// Keys are full entry identities; every row points at the one quoted
/// `packages.<system>."<encoded owner/tap/token>"` attribute the identity
/// installs through, and every row is eligible by construction of the
/// filter.
pub(super) fn catalog_rows_from(
    results: &BTreeMap<String, SearchMeta>,
    reference: &str,
    revision: &Option<String>,
    system: &str,
    stale: bool,
) -> Vec<SearchResult> {
    results
        .iter()
        .map(|(full_id, meta)| SearchResult {
            id: format!("cask:{full_id}"),
            attribute: cask::package_attribute(system, full_id),
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

/// Search one saved tap catalog locally.
///
/// The lane never evaluates Nix and never executes Ruby: it filters the
/// strictly validated saved capture. The platform rule is the envelope's
/// own target list, so an untargeted system is an honest skip with the
/// targets shown; a malformed capture was already refused at load.
#[must_use]
pub fn search_saved_catalog(
    saved: &SavedCatalog,
    query: &str,
    system: &str,
) -> (Vec<SearchResult>, SourceReport) {
    if !saved.index.targets.iter().any(|target| target == system) {
        return (
            Vec::new(),
            SourceReport::skipped_platform(&saved.source, &saved.index.targets),
        );
    }
    let revision = Some(saved.provenance.revision.clone());
    match filter_catalog(&saved.index, system, query) {
        Ok(results) => (
            catalog_rows_from(&results, &saved.reference, &revision, system, false),
            SourceReport {
                source: saved.source.clone(),
                display: Some(saved.source.clone()),
                locked_reference: Some(saved.reference.clone()),
                revision,
                status: SourceStatus::Fresh,
                detail: None,
                support_detail: None,
            },
        ),
        Err(error) => (
            Vec::new(),
            SourceReport {
                source: saved.source.clone(),
                display: Some(saved.source.clone()),
                locked_reference: None,
                revision: None,
                status: SourceStatus::Failed,
                detail: Some(error.to_string()),
                support_detail: None,
            },
        ),
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
        // Choices must separate the namespaces that share a short name:
        // `packages.<system>.probe` and `legacyPackages.<system>.probe`
        // stay full paths, so repeating a choice resolves it instead of
        // reproducing the ambiguity.
        let attrs: Vec<&str> = matches.iter().map(|(attr, _)| attr.as_str()).collect();
        return Err(CatalogError::Ambiguous {
            name: attribute.to_string(),
            choices: unique_exposed_forms(&attrs, None)
                .into_iter()
                .map(str::to_string)
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
    fn native_matching_covers_all_three_native_fields() {
        let meta = SearchMeta {
            pname: String::from("python3.12-requests"),
            version: String::from("2.32.0"),
            description: String::from("HTTP library"),
        };
        let attr = "legacyPackages.x86_64-linux.python312Packages.requests";
        // Full attribute path is matched (native field 1): a pattern that
        // can only match through the path still hits.
        let path_only =
            compile_pattern("^legacyPackages\\.x86_64-linux\\.python312").expect("valid");
        assert!(matches_native(&path_only, attr, &meta));
        // Pname is matched as-is: already the version-stripped derivation
        // name; the version is never matched.
        let name = compile_pattern("^python3\\.12-requests$").expect("valid");
        assert!(matches_native(&name, attr, &meta));
        let version = compile_pattern("2\\.32\\.0").expect("valid");
        assert!(!matches_native(&version, attr, &meta));
        // Description is matched, case-insensitively.
        let desc = compile_pattern("^http lib").expect("valid");
        assert!(matches_native(&desc, attr, &meta));
    }

    #[test]
    fn empty_description_is_matched_not_skipped() {
        let meta = SearchMeta {
            pname: String::from("zzz"),
            version: String::from("1.0"),
            description: String::new(),
        };
        let empty = compile_pattern("^$").expect("valid");
        assert!(matches_native(
            &empty,
            "legacyPackages.x86_64-linux.zzz",
            &meta
        ));
        // A described entry never matches `^$`: all three of its fields
        // are non-empty.
        let described = SearchMeta {
            pname: String::from("fd"),
            version: String::from("10.2.0"),
            description: String::from("find entries"),
        };
        assert!(!matches_native(
            &empty,
            "legacyPackages.x86_64-linux.fd",
            &described,
        ));
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

    /// A stale cached index that does not target this system cannot prove
    /// the current source is unsupported: the earlier failure stays
    /// visible as a stale report with its original detail, never a clean
    /// platform skip. Only a successfully resolved index reports
    /// `skipped-platform`.
    /// The stale fallback of the index lane labels both the report and
    /// the rows with the reused snapshot's own provenance, whatever the
    /// identity step knew (or failed to learn) before the failure: a
    /// metadata failure reports the cached identity instead of `None`,
    /// and a live failure after a fresh identity reports the cached
    /// revision instead of the new one the rows do not describe.
    #[test]
    fn index_stale_report_and_rows_carry_cached_provenance() {
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/catalog-index.json"))
                .expect("valid json");
        let index = crate::nix::decode_catalog_index(&raw).expect("decodes fixture");
        let system = "x86_64-linux";
        let source = "github:spa5k/pkg/main?dir=nix/casks";
        let dir = tempfile::tempdir().expect("temp dir");
        let snap = cache::IndexSnapshot {
            schema: cache::SNAPSHOT_SCHEMA,
            kind: String::from(cache::KIND_INDEX),
            source: source.to_string(),
            locked_reference: Some(String::from("locked:old")),
            revision: Some(String::from("oldrev")),
            system: system.to_string(),
            saved_unix: cache::now_unix(),
            payload: index,
        };
        cache::write_snapshot(dir.path(), &snap).expect("writes snapshot");

        // Metadata failure: the identity step learned nothing, so the
        // report fields are None — the cached snapshot must fill them.
        let metadata_report = SourceReport {
            source: source.to_string(),
            display: None,
            locked_reference: None,
            revision: None,
            status: SourceStatus::Fresh,
            detail: Some(String::from("metadata fetch failed: offline")),
            support_detail: None,
        };
        let (outcome, report) =
            index_stale(dir.path(), source, "koreader", system, metadata_report);
        let lane = outcome.expect("stale rows");
        assert!(lane.stale);
        assert_eq!(lane.reference, "locked:old");
        assert_eq!(lane.revision.as_deref(), Some("oldrev"));
        assert_eq!(report.status, SourceStatus::Stale);
        assert_eq!(report.locked_reference.as_deref(), Some("locked:old"));
        assert_eq!(report.revision.as_deref(), Some("oldrev"));

        // Live failure at a newer revision: the identity step already
        // stamped the new revision, but the reused rows describe the old
        // one — the snapshot's provenance must win.
        let live_report = SourceReport {
            source: source.to_string(),
            display: Some(String::from("pkg")),
            locked_reference: Some(String::from("locked:new")),
            revision: Some(String::from("newrev")),
            status: SourceStatus::Fresh,
            detail: Some(String::from("live index query failed: offline")),
            support_detail: None,
        };
        let (outcome, report) = index_stale(dir.path(), source, "koreader", system, live_report);
        let lane = outcome.expect("stale rows");
        assert!(lane.stale);
        assert_eq!(lane.reference, "locked:old");
        assert_eq!(lane.revision.as_deref(), Some("oldrev"));
        assert_eq!(report.status, SourceStatus::Stale);
        assert_eq!(report.locked_reference.as_deref(), Some("locked:old"));
        assert_eq!(report.revision.as_deref(), Some("oldrev"));
    }

    /// A stale untargeted envelope keeps its failure report — and that
    /// report keeps the cached snapshot provenance it was labeled with.
    #[test]
    fn stale_untargeted_index_keeps_the_failure_report() {
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/catalog-index.json"))
                .expect("valid json");
        let mut index = crate::nix::decode_catalog_index(&raw).expect("decodes fixture");
        // Build a deliberately untargeted envelope: the fixture targets
        // both systems, so this test owns its targets directly.
        let untargeted = if index.targets.iter().any(|t| t == "x86_64-linux") {
            index.targets = vec![String::from("x86_64-linux")];
            "aarch64-darwin"
        } else {
            index.targets = vec![String::from("aarch64-darwin")];
            "x86_64-linux"
        };
        let report = SourceReport {
            source: String::from("github:spa5k/pkg/main?dir=nix/casks"),
            display: None,
            locked_reference: Some(String::from("locked:old")),
            revision: Some(String::from("oldrev")),
            status: SourceStatus::Fresh,
            detail: Some(String::from("metadata fetch failed: offline")),
            support_detail: None,
        };
        let (outcome, stale_report) = index_apply(
            &index,
            "q",
            untargeted,
            String::from("locked"),
            Some(String::from("rev")),
            true,
            report,
        );
        assert!(outcome.is_none());
        assert_eq!(stale_report.status, SourceStatus::Stale);
        assert_eq!(
            stale_report.detail.as_deref(),
            Some("metadata fetch failed: offline")
        );
        // The cached provenance survives the untargeted stale return.
        assert_eq!(stale_report.locked_reference.as_deref(), Some("locked:old"));
        assert_eq!(stale_report.revision.as_deref(), Some("oldrev"));

        // The same untargeted index read fresh is an honest platform skip.
        let report = SourceReport {
            source: String::from("src"),
            display: None,
            locked_reference: None,
            revision: None,
            status: SourceStatus::Fresh,
            detail: None,
            support_detail: None,
        };
        let (outcome, skip_report) = index_apply(
            &index,
            "q",
            untargeted,
            String::from("locked"),
            None,
            false,
            report,
        );
        assert!(outcome.is_none());
        assert_eq!(skip_report.status, SourceStatus::SkippedPlatform);
        assert!(skip_report.detail.is_some_and(|d| d.contains("targets")));
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
        assert!(token_hit.contains_key("homebrew/cask/koreader"));
        let name_hit = filter_catalog(&index, "x86_64-linux", "1password").expect("filters");
        assert!(name_hit.contains_key("homebrew/cask/1password-cli"));
        let desc_hit = filter_catalog(&index, "aarch64-darwin", "command-line").expect("filters");
        assert!(desc_hit.contains_key("homebrew/cask/1password-cli"));

        // Search shows eligible entries only: the excluded `iterm2` on
        // Linux and the installer-script `zoom` never appear.
        let any = filter_catalog(&index, "x86_64-linux", ".").expect("filters");
        assert!(any.contains_key("homebrew/cask/koreader"));
        assert!(!any.contains_key("homebrew/cask/iterm2"));
        assert!(!any.contains_key("homebrew/cask/zoom"));
        // An excluded token stays discoverable through info instead; the
        // `@` and `+` tokens stay searchable when eligible.
        let macos_any = filter_catalog(&index, "aarch64-darwin", ".").expect("filters");
        assert!(!macos_any.contains_key("homebrew/cask/zoom"));
        assert!(macos_any.contains_key("homebrew/cask/1password@7"));
        assert!(macos_any.contains_key("homebrew/cask/4k-video-downloader+"));

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
        assert_eq!(row.id, "cask:homebrew/cask/koreader");
        assert_eq!(
            row.attribute,
            "packages.x86_64-linux.\"homebrew/cask/koreader\""
        );
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

    /// Attributes that share one short name keep their full paths as IDs
    /// and choices; unique short names stay short. This is the rule that
    /// keeps `packages` and `legacyPackages` entries with the same name
    /// from colliding in search rows, bare-name choices, and exact-lookup
    /// choices (review 2026-09-30, P3).
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

        // Search rows never share one ID for the colliding pair, and the
        // full-path IDs still round-trip through install and info.
        let meta = SearchMeta {
            pname: String::from("probe"),
            version: String::from("1"),
            description: String::new(),
        };
        let results: BTreeMap<String, SearchMeta> = attrs
            .iter()
            .map(|attr| (attr.to_string(), meta.clone()))
            .collect();
        let rows = rows_from(
            results.iter(),
            &attrs,
            SourceKind::Nixpkgs,
            "nixpkgs-ref",
            &None,
            system,
            false,
        );
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "nixpkgs:legacyPackages.x86_64-linux.probe",
                "nixpkgs:cursor",
                "nixpkgs:packages.x86_64-linux.probe",
            ]
        );
        for id in &ids {
            let suffix = id.strip_prefix("nixpkgs:").expect("prefixed");
            assert!(crate::catalog::parse_id(id).is_ok(), "{id} must parse");
            let matches = exact_attribute_matches(&results, suffix);
            assert_eq!(matches.len(), 1, "{id} must resolve to one attribute");
        }
    }

    #[test]
    fn description_only_match_in_a_shared_namespace_keeps_its_full_path() {
        // One query matches only the `packages` copy through its
        // description, but the full snapshot still contains the
        // `legacyPackages` twin: the short form stays claimed by the
        // universe, so the row ID keeps the full path.
        let system = "x86_64-linux";
        let full: BTreeMap<String, SearchMeta> = [
            ("packages.x86_64-linux.probe", "probing tool"),
            ("legacyPackages.x86_64-linux.probe", "unrelated"),
        ]
        .into_iter()
        .map(|(attr, description)| {
            (
                attr.to_string(),
                SearchMeta {
                    pname: String::from("probe"),
                    version: String::from("1"),
                    description: String::from(description),
                },
            )
        })
        .collect();
        let universe: Vec<&str> = full.keys().map(String::as_str).collect();
        let matched = full
            .iter()
            .filter(|(_, meta)| meta.description.contains("probing"));
        let rows = rows_from(
            matched,
            &universe,
            SourceKind::Nixpkgs,
            "nixpkgs-ref",
            &None,
            system,
            false,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "nixpkgs:packages.x86_64-linux.probe");
    }
}
