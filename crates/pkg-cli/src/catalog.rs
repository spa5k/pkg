//! Catalog name routing and disposable discovery data (design D5).
//!
//! The catalog maps names to installables. It owns no installed state. Discovery
//! uses native evaluation results only; repositories are never parsed.
//!
//! Every discovery result is bound to provenance: the moving source reference,
//! the locked reference actually queried, the locked revision, and the system.
//! Installation always passes the original moving reference to Nix; only
//! discovery reads through the locked reference so a result set can never mix
//! revisions.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Sources;
use crate::nix::{CaskSupport, Nix, SearchMeta};

/// The discovery cache schema version.
///
/// Version 2 keys cache files by source, system, and query, and records the
/// locked reference and revision the results came from.
pub const CACHE_SCHEMA: u32 = 2;

/// The only system the cask source supports (design D6).
pub const CASK_SYSTEM: &str = "aarch64-darwin";

/// Whether the cask source can be queried on `system`.
#[must_use]
pub fn casks_supported_on(system: &str) -> bool {
    system == CASK_SYSTEM
}

/// A catalog routing error.
#[derive(Debug)]
pub enum CatalogError {
    /// A name or attribute matched nothing in the queried source.
    NotFound(String),
    /// A name matched more than one supported output.
    Ambiguous {
        /// The requested name.
        name: String,
        /// The source-qualified or attribute-path choices.
        choices: Vec<String>,
    },
    /// A source query failed. Native diagnostics are preserved.
    Source(String),
    /// The disposable cache could not be written.
    CacheWrite(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(name) => write!(f, "no supported exact match for `{name}`"),
            Self::Ambiguous { name, choices } => write!(
                f,
                "`{name}` is ambiguous; choose one of: {}",
                choices.join(", ")
            ),
            Self::Source(detail) => write!(f, "{detail}"),
            Self::CacheWrite(detail) => write!(f, "cannot write discovery cache: {detail}"),
        }
    }
}

impl std::error::Error for CatalogError {}

/// A routed catalog identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogId {
    /// An attribute in the configured Nixpkgs source.
    Nixpkgs(String),
    /// A token in the configured Cask product flake.
    Cask(String),
    /// An explicit public GitHub flake reference with an attribute.
    Explicit {
        /// The flake reference, for example `github:owner/repo`.
        reference: String,
        /// The attribute after `#`.
        attribute: String,
    },
}

impl CatalogId {
    /// The source-qualified display name.
    #[must_use]
    pub fn qualified(&self) -> String {
        match self {
            Self::Nixpkgs(attr) => format!("nixpkgs:{attr}"),
            Self::Cask(token) => format!("cask:{token}"),
            Self::Explicit { attribute, .. } => format!("flake:{attribute}"),
        }
    }

    /// The Nix installable for this identity.
    ///
    /// Installation always uses the original moving reference; locking is
    /// native behavior at install time.
    #[must_use]
    pub fn installable(&self, sources: &Sources) -> String {
        match self {
            Self::Nixpkgs(attr) => format!("{}#{attr}", sources.nixpkgs),
            Self::Cask(token) => format!("{}#{token}", sources.casks),
            Self::Explicit {
                reference,
                attribute,
            } => format!("{reference}#{attribute}"),
        }
    }
}

/// How a user-supplied ID parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedId {
    /// A source-qualified or explicit identity.
    Qualified(CatalogId),
    /// A bare name that needs disambiguation.
    Bare(String),
}

/// Parse and validate a user-supplied ID without contacting any source.
///
/// Supported forms: `nixpkgs:attr`, `cask:token`, bare names, and public
/// GitHub flake installables `github:owner/repo[#ref]#attribute`. Qualified
/// attributes must be nonempty dot-separated identifiers; explicit GitHub
/// forms need a nonempty owner and repository and no arbitrary path forms.
/// Anything else that looks like an explicit source is refused with a reason.
pub fn parse_id(input: &str) -> Result<ParsedId, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err(String::from("ID must not be empty"));
    }
    if let Some(attr) = input.strip_prefix("nixpkgs:") {
        validate_attr_path(attr, "nixpkgs:")?;
        return Ok(ParsedId::Qualified(CatalogId::Nixpkgs(attr.to_string())));
    }
    if let Some(token) = input.strip_prefix("cask:") {
        validate_cask_token(token, "cask:")?;
        return Ok(ParsedId::Qualified(CatalogId::Cask(token.to_string())));
    }
    if let Some(rest) = input.strip_prefix("github:") {
        let Some((reference, attribute)) = rest.split_once('#') else {
            return Err(format!(
                "`{input}` needs an attribute after `#`, \
                 for example github:owner/repo#packages.aarch64-darwin.default"
            ));
        };
        let segments: Vec<&str> = reference.split('/').collect();
        if segments.len() < 2
            || segments.len() > 3
            || segments.iter().any(|segment| segment.is_empty())
        {
            return Err(format!(
                "`{input}` is not a public GitHub reference; \
                 use github:owner/repo or github:owner/repo/ref"
            ));
        }
        validate_attr_path(attribute, "`github:` installables ")?;
        return Ok(ParsedId::Qualified(CatalogId::Explicit {
            reference: format!("github:{reference}"),
            attribute: attribute.to_string(),
        }));
    }
    if input.contains('#') {
        return Err(format!(
            "`{input}` is not a supported explicit source; \
             only public github: references are supported"
        ));
    }
    if input.contains("://") {
        return Err(format!(
            "`{input}` is not a supported explicit source; \
             only public github: references are supported"
        ));
    }
    Ok(ParsedId::Bare(input.to_string()))
}

/// Validate a cask token.
///
/// Homebrew tokens may also use `@` (versioned tokens such as
/// `1password@nightly`) and `.`; refusing them would hide the recorded
/// exclusion reason for exactly those tokens.
fn validate_cask_token(token: &str, prefix: &str) -> Result<(), String> {
    if token.is_empty() {
        return Err(format!("{prefix} needs a nonempty token"));
    }
    if !token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '@' | '.'))
    {
        return Err(format!(
            "`{token}` is not a valid cask token; \
             tokens may use letters, digits, `_`, `-`, `@`, and `.`"
        ));
    }
    Ok(())
}

fn validate_attr_path(attr: &str, prefix: &str) -> Result<(), String> {
    if attr.is_empty() {
        return Err(format!("{prefix} needs a nonempty attribute"));
    }
    for segment in attr.split('.') {
        if segment.is_empty() {
            return Err(format!(
                "`{attr}` is not a valid attribute path; \
                 empty segment between dots"
            ));
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!(
                "`{attr}` is not a valid attribute path; \
                 segments may use letters, digits, `_`, and `-`"
            ));
        }
    }
    Ok(())
}

/// Escape literal text for use as a regex.
///
/// Every regex metacharacter is escaped, so the query matches the literal
/// text only. Only ASCII punctuation is escaped, which the regex engine
/// always accepts.
#[must_use]
pub fn escape_regex(text: &str) -> String {
    const META: &[char] = &[
        '\\', '.', '+', '*', '?', '^', '$', '(', ')', '|', '[', ']', '{', '}', '#', '&', '-', '~',
    ];
    let mut escaped = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if META.contains(&c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Canonical source identity of a reference, for installed matching.
///
/// Two references map to the same canonical source when they name the same
/// repository *and* the same flake subdirectory: reference, revision, and
/// other parameters are ignored, but `dir=` is retained so two flakes in one
/// repository are never conflated.
#[must_use]
pub fn canonical_source(reference: &str) -> String {
    let without_fragment = reference.split('#').next().unwrap_or(reference);
    let (base, query) = without_fragment
        .split_once('?')
        .unwrap_or((without_fragment, ""));
    let dir = query
        .split('&')
        .find_map(|param| param.strip_prefix("dir="));
    let canonical = if let Some(rest) = base.strip_prefix("github:") {
        let mut segments = rest.split('/');
        let owner = segments.next().unwrap_or_default();
        let repo = segments.next().unwrap_or_default();
        format!("github:{owner}/{repo}")
    } else {
        base.to_string()
    };
    match dir {
        Some(dir) => format!("{canonical}?dir={dir}"),
        None => canonical,
    }
}

/// Normalize a native attribute path to its exposed catalog ID suffix.
///
/// Only the standard `packages.<system>.` and `legacyPackages.<system>.`
/// prefixes are removed, so copied search IDs round-trip to install while
/// nested Nixpkgs paths such as `python3Packages.foo` stay explicit. The full
/// native attribute is kept separately for matching.
#[must_use]
pub fn exposed_attribute<'a>(attr: &'a str, system: &str) -> &'a str {
    for prefix in ["packages", "legacyPackages"] {
        let standard = format!("{prefix}.{system}.");
        if let Some(rest) = attr.strip_prefix(&standard)
            && !rest.is_empty()
        {
            return rest;
        }
    }
    attr
}

/// Strip one standard output-set prefix from a native attribute path.
///
/// Removes a leading `packages.<system>.` or `legacyPackages.<system>.`
/// segment pair, whatever system it names: search keys come from the
/// current system, so the prefix identifies the output set, not a user
/// choice. No other rewriting happens, and a nested path such as
/// `python312Packages.pip` stays intact.
fn strip_standard_prefix(attr: &str) -> &str {
    for prefix in ["packages.", "legacyPackages."] {
        if let Some(rest) = attr.strip_prefix(prefix)
            && let Some((system, tail)) = rest.split_once('.')
            && !system.is_empty()
            && !tail.is_empty()
        {
            return tail;
        }
    }
    attr
}

/// Resolve a bare name to exactly one supported output.
///
/// On systems without cask support the macOS-only cask source is not queried
/// at all, so it can never be a fatal requirement there. Ambiguous names are
/// refused with the qualified choices.
pub fn resolve_bare(
    nix: &Nix,
    sources: &Sources,
    name: &str,
    system: &str,
) -> Result<CatalogId, CatalogError> {
    let pattern = format!("^{}$", escape_regex(name));
    let mut choices = Vec::new();
    let nixpkgs = nix
        .search(&sources.nixpkgs, &pattern)
        .map_err(|error| CatalogError::Source(error.to_string()))?;
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
    if casks_supported_on(system) {
        let casks = nix
            .search(&sources.casks, &pattern)
            .map_err(|error| CatalogError::Source(error.to_string()))?;
        for attr in exact_attribute_matches(&casks, name)
            .into_iter()
            .map(|(attr, _)| attr)
        {
            // Cask search keys are `packages.<system>.<token>`; routing uses
            // the exposed token so the choice round-trips to install.
            choices.push(CatalogId::Cask(
                exposed_attribute(&attr, system).to_string(),
            ));
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
        .filter(|(attr, _)| strip_standard_prefix(attr) == attribute)
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
    /// The token is supported on this system.
    Supported,
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

/// Cached discovery results with full provenance.
///
/// The cache is keyed by source, system, and query; a file written for one
/// query is never reused to answer a different query.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CachedSearch {
    /// The cache schema version.
    pub schema: u32,
    /// The moving source reference the cache belongs to.
    pub source: String,
    /// The locked reference the cached results came from.
    pub locked_reference: Option<String>,
    /// The locked revision of that reference.
    pub revision: Option<String>,
    /// The system the cached results were produced for.
    pub system: String,
    /// The query the cached results answer.
    pub query: String,
    /// When the cache was written (Unix seconds).
    pub saved_unix: u64,
    /// Attribute path to metadata.
    pub results: BTreeMap<String, SearchMeta>,
}

/// Read the cached results for one source, system, and query, if present.
#[must_use]
pub fn read_cache(
    cache_dir: &Path,
    source: &str,
    system: &str,
    query: &str,
) -> Option<CachedSearch> {
    let path = cache_dir.join(format!("search-{}.json", cache_key(source, system, query)));
    let text = std::fs::read_to_string(path).ok()?;
    let cached: CachedSearch = serde_json::from_str(&text).ok()?;
    (cached.schema == CACHE_SCHEMA
        && cached.source == source
        && cached.system == system
        && cached.query == query)
        .then_some(cached)
}

/// Write discovery results to the disposable cache.
pub fn write_cache(cache_dir: &Path, cached: &CachedSearch) -> Result<(), CatalogError> {
    std::fs::create_dir_all(cache_dir)
        .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
    let path = cache_dir.join(format!(
        "search-{}.json",
        cache_key(&cached.source, &cached.system, &cached.query)
    ));
    let text = serde_json::to_string_pretty(cached)
        .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
    std::fs::write(path, text).map_err(|error| CatalogError::CacheWrite(error.to_string()))
}

/// Remove all cached discovery data (the `update` refresh path).
pub fn invalidate_cache(cache_dir: &Path) -> Result<usize, CatalogError> {
    if !cache_dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    let entries = std::fs::read_dir(cache_dir)
        .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("search-") && name.ends_with(".json") {
            std::fs::remove_file(entry.path())
                .map_err(|error| CatalogError::CacheWrite(error.to_string()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn cache_key(source: &str, system: &str, query: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source.bytes().chain(system.bytes()).chain(query.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
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
    let identity = nix.source_identity(moving_source);
    let mut report = SourceReport {
        source: moving_source.to_string(),
        display: None,
        locked_reference: None,
        revision: None,
        status: SourceStatus::Fresh,
        detail: None,
        support_detail: None,
    };
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

    // Fresh reuse requires a proven revision match; None == None is not proof.
    if let (Some(rev), Some(locked_ref)) = (&revision, &locked)
        && let Some(cached) = read_cache(cache_dir, moving_source, system, query)
        && cached.revision.as_ref() == Some(rev)
    {
        let support = cask_manifest_for(nix, kind, system, locked_ref, &mut report);
        return (
            rows_from(
                &cached.results,
                kind,
                locked_ref,
                &revision,
                system,
                false,
                &support,
            ),
            report,
        );
    }
    let search_ref = locked.clone().unwrap_or_else(|| moving_source.to_string());
    match nix.search(&search_ref, query) {
        Ok(results) => {
            let cached = CachedSearch {
                schema: CACHE_SCHEMA,
                source: moving_source.to_string(),
                locked_reference: locked.clone(),
                revision: revision.clone(),
                system: system.to_string(),
                query: query.to_string(),
                saved_unix: now_unix(),
                results: results.clone(),
            };
            let _ = write_cache(cache_dir, &cached);
            let support = cask_manifest_for(nix, kind, system, &search_ref, &mut report);
            (
                rows_from(
                    &results,
                    kind,
                    &search_ref,
                    &revision,
                    system,
                    false,
                    &support,
                ),
                report,
            )
        }
        Err(error) => match read_cache(cache_dir, moving_source, system, query) {
            Some(cached) => {
                report.status = SourceStatus::Stale;
                report.detail = Some(format!("live query failed: {error}"));
                let support = cask_manifest_for(
                    nix,
                    kind,
                    system,
                    cached.locked_reference.as_deref().unwrap_or(moving_source),
                    &mut report,
                );
                (
                    rows_from(
                        &cached.results,
                        kind,
                        cached.locked_reference.as_deref().unwrap_or(moving_source),
                        &cached.revision.clone(),
                        system,
                        true,
                        &support,
                    ),
                    report,
                )
            }
            None => {
                report.status = SourceStatus::Failed;
                report.detail = Some(format!("live query failed: {error}"));
                (Vec::new(), report)
            }
        },
    }
}

fn cask_manifest_for(
    nix: &Nix,
    kind: SourceKind,
    system: &str,
    locked_ref: &str,
    report: &mut SourceReport,
) -> Option<CaskSupport> {
    if kind != SourceKind::Cask {
        return None;
    }
    match nix.cask_support(locked_ref, system) {
        Ok(support) => Some(support),
        Err(error) => {
            report.support_detail = Some(format!("cask support manifest unavailable: {error}"));
            None
        }
    }
}

fn rows_from(
    results: &BTreeMap<String, SearchMeta>,
    kind: SourceKind,
    reference: &str,
    revision: &Option<String>,
    system: &str,
    stale: bool,
    support: &Option<CaskSupport>,
) -> Vec<SearchResult> {
    results
        .iter()
        .map(|(attr, meta)| {
            // A manifest present but token unlisted yields no badge; pkg does
            // not claim support the manifest does not state.
            let support_badge = support.as_ref().and_then(|manifest| {
                let token = exposed_attribute(attr, system);
                match classify_cask(manifest, token) {
                    CaskStatus::Supported { .. } => Some(SupportBadge::Supported),
                    CaskStatus::Excluded { reason, detail, .. } => {
                        Some(SupportBadge::Excluded { reason, detail })
                    }
                    CaskStatus::Unlisted { .. } => None,
                }
            });
            SearchResult {
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
                support: support_badge,
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
        .map_err(|error| CatalogError::Source(error.to_string()))?;
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
        .map_err(|error| CatalogError::Source(error.to_string()))?;
    let report = report_for(moving_source, identity);
    let matches = exact_attribute_matches(&results, attribute);
    if matches.len() > 1 {
        return Err(CatalogError::Ambiguous {
            name: attribute.to_string(),
            choices: matches
                .iter()
                .map(|(attr, _)| strip_standard_prefix(attr).to_string())
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

/// Support status for one cask token on one system (design D6).
///
/// Derived from the `pkg-cask-support/1` manifest the casks flake exports.
/// The manifest is evaluated from the locked reference of the same moving
/// source that native search uses, so support decisions and search results
/// always describe one source revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaskStatus {
    /// The token is supported on this system.
    Supported {
        /// The package kind, for example `app` or `app+cli`.
        kind: String,
        /// The upstream version, when the flake interface provides one.
        version: Option<String>,
        /// The upstream homepage, when the flake interface provides one.
        homepage: Option<String>,
        /// The artifact kinds the package provides.
        artifact_kinds: Vec<String>,
        /// Whether an override is applied on top of upstream.
        override_applied: bool,
        /// The locked revision of the casks source this decision came from.
        source_revision: Option<String>,
    },
    /// The token is excluded on this system, with the recorded reason.
    Excluded {
        /// The upstream version that was excluded, when known.
        version: Option<String>,
        /// A short machine-readable reason.
        reason: String,
        /// Human-readable detail for the exclusion, when present.
        detail: Option<String>,
        /// The locked revision of the casks source this decision came from.
        source_revision: Option<String>,
    },
    /// The token is absent from the manifest for this system.
    Unlisted {
        /// The cask token.
        token: String,
    },
}

/// Classify one token against a decoded cask support manifest.
///
/// Exclusions win over support so a token listed in both is refused.
#[must_use]
pub fn classify_cask(support: &CaskSupport, token: &str) -> CaskStatus {
    if let Some(excluded) = support.excluded.iter().find(|e| e.token == token) {
        return CaskStatus::Excluded {
            version: excluded.version.clone(),
            reason: excluded.reason.clone(),
            detail: excluded.detail.clone(),
            source_revision: None,
        };
    }
    if let Some(supported) = support.supported.iter().find(|s| s.token == token) {
        return CaskStatus::Supported {
            kind: supported.kind.clone(),
            version: supported.version.clone(),
            homepage: supported.homepage.clone(),
            artifact_kinds: supported.artifact_kinds.clone(),
            override_applied: supported.override_applied,
            source_revision: None,
        };
    }
    CaskStatus::Unlisted {
        token: token.to_string(),
    }
}

/// One cask support decision with the source identity it came from.
///
/// The identity is the same locked source any metadata lookup for the same
/// info result must use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaskLookup {
    /// The resolved identity of the casks source.
    pub identity: crate::nix::SourceIdentity,
    /// The classification for the token.
    pub status: CaskStatus,
}

/// Resolve cask support status for `token` on a cask-capable `system`.
///
/// `source` must be the configured casks source reference; the manifest is
/// evaluated from its locked reference and the locked revision recorded by
/// `nix flake metadata` is attached to the decision.
pub fn cask_status(
    nix: &Nix,
    source: &str,
    system: &str,
    token: &str,
) -> Result<CaskLookup, CatalogError> {
    let identity = nix
        .source_identity(source)
        .map_err(|error| CatalogError::Source(error.to_string()))?;
    let eval_ref = identity
        .locked_url
        .clone()
        .unwrap_or_else(|| source.to_string());
    let support = nix
        .cask_support(&eval_ref, system)
        .map_err(|error| CatalogError::Source(error.to_string()))?;
    let revision = identity.revision.clone();
    let status = match classify_cask(&support, token) {
        CaskStatus::Supported {
            kind,
            version,
            homepage,
            artifact_kinds,
            override_applied,
            source_revision: _,
        } => CaskStatus::Supported {
            kind,
            version,
            homepage,
            artifact_kinds,
            override_applied,
            source_revision: revision,
        },
        CaskStatus::Excluded {
            version,
            reason,
            detail,
            source_revision: _,
        } => CaskStatus::Excluded {
            version,
            reason,
            detail,
            source_revision: revision,
        },
        unlisted @ CaskStatus::Unlisted { .. } => unlisted,
    };
    Ok(CaskLookup { identity, status })
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_ids_and_refuses_malformed_ones() {
        assert_eq!(
            parse_id("nixpkgs:ripgrep"),
            Ok(ParsedId::Qualified(CatalogId::Nixpkgs(String::from(
                "ripgrep"
            ))))
        );
        assert_eq!(
            parse_id("nixpkgs:legacyPackages.aarch64-darwin.fd"),
            Ok(ParsedId::Qualified(CatalogId::Nixpkgs(String::from(
                "legacyPackages.aarch64-darwin.fd"
            ))))
        );
        assert_eq!(
            parse_id("cask:iterm2"),
            Ok(ParsedId::Qualified(CatalogId::Cask(String::from("iterm2"))))
        );
        assert_eq!(
            parse_id("github:hraban/mac-app-util#default"),
            Ok(ParsedId::Qualified(CatalogId::Explicit {
                reference: String::from("github:hraban/mac-app-util"),
                attribute: String::from("default"),
            }))
        );
        assert_eq!(
            parse_id("ripgrep"),
            Ok(ParsedId::Bare(String::from("ripgrep")))
        );
        for refused in [
            "",
            " ",
            "nixpkgs:",
            "nixpkgs:a..b",
            "nixpkgs:a/b",
            "cask:",
            "cask:to ken",
            "github:owner",
            "github:owner/",
            "github:/repo#x",
            "github:owner/repo/sub/dir#x",
            "github:owner/repo",
            "path:/tmp/foo#default",
            "git+https://example.com/repo#default",
            "https://example.com/repo#default",
            "flake#attr",
        ] {
            assert!(parse_id(refused).is_err(), "`{refused}` must be refused");
        }
    }

    #[test]
    fn escapes_every_regex_metacharacter() {
        assert_eq!(escape_regex("ripgrep"), "ripgrep");
        assert_eq!(
            escape_regex("a.b+c*d?e(f)g[h]i{j}k^l$m|n#o&p-q~r\\s"),
            "a\\.b\\+c\\*d\\?e\\(f\\)g\\[h\\]i\\{j\\}k\\^l\\$m\\|n\\#o\\&p\\-q\\~r\\\\s"
        );
    }

    #[test]
    fn canonical_source_keeps_subdirectory_identity() {
        // Revision and reference differences still collapse.
        assert_eq!(
            canonical_source("github:NixOS/nixpkgs/nixpkgs-unstable"),
            canonical_source("github:NixOS/nixpkgs?rev=0a56ceb53d693f3e0eaea0f9f4d5b8cf5b9b9d1a")
        );
        // `dir=` distinguishes two flakes inside one repository.
        assert_eq!(
            canonical_source("github:spa5k/pkg/main?dir=nix/casks"),
            canonical_source("github:spa5k/pkg?dir=nix/casks")
        );
        assert_ne!(
            canonical_source("github:spa5k/pkg"),
            canonical_source("github:spa5k/pkg?dir=nix/casks")
        );
        assert_ne!(
            canonical_source("github:NixOS/nixpkgs"),
            canonical_source("github:spa5k/pkg")
        );
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
    fn cask_tokens_may_carry_known_extra_characters() {
        // `@` tokens must parse so their exclusion reason can be shown.
        assert_eq!(
            parse_id("cask:1password@nightly"),
            Ok(ParsedId::Qualified(CatalogId::Cask(String::from(
                "1password@nightly"
            ))))
        );
        assert!(parse_id("cask:to ken").is_err());
        assert!(parse_id("cask:").is_err());
    }

    #[test]
    fn cache_is_keyed_by_source_system_and_query() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cached = CachedSearch {
            schema: CACHE_SCHEMA,
            source: String::from("github:NixOS/nixpkgs/nixpkgs-unstable"),
            locked_reference: Some(String::from("github:NixOS/nixpkgs/aaa")),
            revision: Some(String::from("aaa")),
            system: String::from("aarch64-darwin"),
            query: String::from("query-one"),
            saved_unix: 1,
            results: BTreeMap::new(),
        };
        write_cache(dir.path(), &cached).expect("writes");
        // Same source/system/query reads back; a different query never does.
        assert_eq!(
            read_cache(dir.path(), &cached.source, "aarch64-darwin", "query-one"),
            Some(cached.clone())
        );
        assert_eq!(
            read_cache(dir.path(), &cached.source, "aarch64-darwin", "query-two"),
            None
        );
        assert_eq!(
            read_cache(dir.path(), &cached.source, "x86_64-linux", "query-one"),
            None
        );
        // Schema and source fields are re-checked on read.
        assert_eq!(invalidate_cache(dir.path()).expect("invalidates"), 1);
        assert_eq!(
            read_cache(dir.path(), &cached.source, "aarch64-darwin", "query-one"),
            None
        );
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

    /// Classification runs against the actual generated manifest fixture,
    /// not an invented shape.
    #[test]
    fn classifies_cask_support_from_the_real_fixture() {
        let manifest = include_str!("../tests/fixtures/cask-support.json");
        let support: crate::nix::CaskSupport =
            serde_json::from_str(manifest).expect("decodes pkg-cask-support/1");
        match classify_cask(&support, "iterm2") {
            CaskStatus::Supported {
                kind,
                override_applied,
                ..
            } => {
                assert_eq!(kind, "app");
                assert!(!override_applied);
            }
            other => panic!("iterm2 must be supported: {other:?}"),
        }
        match classify_cask(&support, "cursor") {
            CaskStatus::Supported {
                override_applied, ..
            } => assert!(override_applied),
            other => panic!("cursor must be supported: {other:?}"),
        }
        match classify_cask(&support, "zoom") {
            CaskStatus::Excluded {
                reason,
                detail,
                version,
                ..
            } => {
                assert_eq!(reason, "native-installer");
                assert_eq!(detail, None);
                assert_eq!(version.as_deref(), Some("7.1.5.84650"));
            }
            other => panic!("zoom must be excluded: {other:?}"),
        }
        // The real current manifest excludes raycast on-device with a
        // recorded reason and detail (manager verification).
        match classify_cask(&support, "raycast") {
            CaskStatus::Excluded {
                reason,
                detail,
                version,
                ..
            } => {
                assert_eq!(reason, "minimum-os");
                assert!(detail.as_deref().is_some_and(|d| !d.is_empty()));
                assert_eq!(version.as_deref(), Some("2.0.5.0"));
            }
            other => panic!("raycast must be excluded: {other:?}"),
        }
        assert_eq!(
            classify_cask(&support, "missing-token"),
            CaskStatus::Unlisted {
                token: String::from("missing-token")
            }
        );
        // Unlisted tokens never get a badge in search rows; excluded tokens
        // carry their reason and (nullable) detail. Keys arrive in native
        // full-attribute form; the exposed ID is normalized.
        let mut results = BTreeMap::new();
        results.insert(
            String::from("packages.aarch64-darwin.zoom"),
            SearchMeta {
                pname: String::from("zoom"),
                version: String::from("7.1.5.84650"),
                description: String::new(),
            },
        );
        results.insert(
            String::from("packages.aarch64-darwin.missing-token"),
            SearchMeta {
                pname: String::from("missing-token"),
                version: String::from("1"),
                description: String::new(),
            },
        );
        let rows = rows_from(
            &results,
            SourceKind::Cask,
            "github:spa5k/pkg/rev",
            &Some(String::from("rev")),
            CASK_SYSTEM,
            false,
            &Some(support),
        );
        let zoom_row = rows
            .iter()
            .find(|row| row.id == "cask:zoom")
            .expect("zoom row with normalized ID");
        assert_eq!(zoom_row.attribute, "packages.aarch64-darwin.zoom");
        assert_eq!(
            zoom_row.support,
            Some(SupportBadge::Excluded {
                reason: String::from("native-installer"),
                detail: None,
            })
        );
        let missing_row = rows
            .iter()
            .find(|row| row.id == "cask:missing-token")
            .expect("missing row");
        assert_eq!(missing_row.support, None);
    }

    #[test]
    fn cask_platform_capability_is_explicit() {
        assert!(casks_supported_on("aarch64-darwin"));
        assert!(!casks_supported_on("x86_64-darwin"));
        assert!(!casks_supported_on("x86_64-linux"));
        assert_eq!(SourceKind::Nixpkgs.label(), "nixpkgs");
        assert_eq!(SourceKind::Cask.label(), "cask");
        assert_eq!(SourceStatus::Stale.label(), "stale");
    }
}
