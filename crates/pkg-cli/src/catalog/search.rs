//! Discovery queries with provenance, and exact lookups for `info`.
//!
//! Local search is implemented in `local`. This module holds shared rows,
//! provenance, exact lookups, and strict name resolution for mutations.

use std::collections::BTreeMap;
use std::path::Path;

use crate::nix::{CatalogEntry, Nix, SearchMeta};

use super::CatalogError;
use super::cask::CatalogOnce;
use super::id::CatalogId;
use super::routing::Routing;

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
        match self {
            Self::Fresh => "fresh",
            Self::Cached => "cached",
            Self::Stale => "stale",
            Self::Failed => "failed",
            Self::SkippedPlatform => "skipped-platform",
        }
    }
}

/// Support badge for a cask row in discovery results.
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
/// The Nixpkgs lane probes exact attributes at the current immutable source
/// revision. Alias lookup may read a complete snapshot of that same revision.
/// A missing snapshot never starts a full discovery scan. The cask
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
    cache_dir: &Path,
    routing: &Routing,
    saved: &crate::tap::SavedCatalogs,
    name: &str,
    system: &str,
    catalog: &mut CatalogOnce<'_>,
) -> Result<CatalogId, CatalogError> {
    resolve_inner(
        nix,
        Some((&routing.nixpkgs, cache_dir)),
        saved,
        name,
        system,
        catalog,
    )
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
    nixpkgs: Option<(&str, &Path)>,
    saved: &crate::tap::SavedCatalogs,
    name: &str,
    system: &str,
    catalog: &mut CatalogOnce<'_>,
) -> Result<CatalogId, CatalogError> {
    let mut choices = Vec::new();
    let mut requires_id = false;
    if let Some((source, cache_dir)) = nixpkgs {
        let identity = nix.source_identity(source).map_err(CatalogError::from)?;
        let reference = identity.locked_url.as_deref().unwrap_or(source);
        let mut exact = BTreeMap::new();
        for root in ["packages", "legacyPackages"] {
            let mut path = vec![root.to_string(), system.to_string()];
            path.extend(name.split('.').map(str::to_string));
            if name.starts_with("packages.") || name.starts_with("legacyPackages.") {
                path = name.split('.').map(str::to_string).collect();
                if path.first().is_none_or(|prefix| prefix != root)
                    || path.get(1).is_none_or(|target| target != system)
                {
                    continue;
                }
            }
            match nix.metadata_batch(reference, system, root, &[path]) {
                Ok(nodes) => {
                    for node in nodes {
                        if let Some(meta) = node.meta {
                            exact.insert(node.path.join("."), meta);
                        }
                    }
                }
                Err(crate::nix::NixError::Failed { stderr, .. })
                    if stderr.contains("does not provide attribute")
                        || stderr.contains("missing") && stderr.contains("attribute") => {}
                Err(error) => return Err(error.into()),
            }
        }
        if exact.is_empty() {
            if let Some(snapshot) =
                super::native::Snapshot::prepared(cache_dir, source, system, &identity)
            {
                let query = super::SearchQuery::new(name, false, false)
                    .map_err(|error| CatalogError::Source(error.to_string()))?
                    .fuzzy(false);
                snapshot.visit(|attr, exposed, meta| {
                    if attr.rsplit('.').next() == Some(name) || meta.pname == name {
                        let row = SearchResult {
                            id: format!("nixpkgs:{exposed}"),
                            attribute: attr,
                            name: meta.pname,
                            version: meta.version,
                            description: meta.description,
                            source: String::from("nixpkgs"),
                            reference: Some(reference.to_string()),
                            revision: identity.revision.clone(),
                            system: system.to_string(),
                            stale: false,
                            support: None,
                        };
                        requires_id |= query.rank(&row).is_none();
                        choices.push(CatalogId::Nixpkgs(exposed));
                    }
                    Ok(())
                })?;
            }
        } else {
            let attrs: Vec<&str> = exact.keys().map(String::as_str).collect();
            for attr in unique_exposed_forms(&attrs, Some(system)) {
                choices.push(CatalogId::Nixpkgs(attr.to_string()));
            }
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
        1 if requires_id => Err(CatalogError::RequiresId {
            name: name.to_string(),
            choices: choices.iter().map(CatalogId::qualified).collect(),
        }),
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
/// Prepare a complete current snapshot before reporting update success.
pub fn prepare_source(
    nix: &Nix,
    cache_dir: &Path,
    source: &str,
    system: &str,
    identity: &crate::nix::SourceIdentity,
) -> Result<(), CatalogError> {
    if identity.revision.as_deref().is_none_or(str::is_empty)
        || identity.locked_url.as_deref().is_none_or(str::is_empty)
    {
        return Err(CatalogError::Source(String::from(
            "catalog preparation requires an immutable source revision",
        )));
    }
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

#[cfg(test)]
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
