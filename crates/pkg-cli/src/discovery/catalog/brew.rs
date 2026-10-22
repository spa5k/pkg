//! Generated Formula support and native Nix identities, without Homebrew.

use std::collections::BTreeMap;
use std::path::Path;

use crate::nix::{Nix, SourceIdentity};

use super::{
    Availability, CaskHost, CatalogError, CatalogView, Kept, SearchQuery, SearchResult,
    SourceReport, SupportBadge, cache, local,
};

const KIND: &str = "formula-index";
const SOURCE: &str = "homebrew/core";

pub(super) fn valid_formula_name(name: &str) -> bool {
    crate::nix::valid_catalog_token(name)
}

pub(crate) struct FormulaView {
    pub catalog: CatalogView,
    aliases: BTreeMap<String, String>,
    executables: BTreeMap<String, Option<Vec<String>>>,
}

impl FormulaView {
    fn decode(identity: SourceIdentity, raw: &serde_json::Value) -> Result<Self, CatalogError> {
        let index = crate::nix::decode_formula_index(raw)?;
        if index.inputs().len() != 1 || !index.inputs().contains_key(SOURCE) {
            return Err(CatalogError::Source(String::from(
                "Formula catalog must contain only homebrew/core",
            )));
        }
        let aliases: BTreeMap<String, String> =
            serde::Deserialize::deserialize(raw.get("aliases").unwrap_or(&serde_json::Value::Null))
                .map_err(|error| {
                    CatalogError::Source(format!("invalid Formula aliases: {error}"))
                })?;
        let executables: BTreeMap<String, Option<Vec<String>>> = serde::Deserialize::deserialize(
            raw.get("executables").unwrap_or(&serde_json::Value::Null),
        )
        .map_err(|error| CatalogError::Source(format!("invalid Formula executables: {error}")))?;
        let exists = |name: &str| {
            index
                .systems()
                .values()
                .any(|system| system.entries.contains_key(&format!("{SOURCE}/{name}")))
        };
        if aliases.iter().any(|(alias, name)| {
            !valid_formula_name(alias) || !valid_formula_name(name) || !exists(name)
        }) || executables
            .keys()
            .any(|name| !valid_formula_name(name) || !exists(name))
        {
            return Err(CatalogError::Source(String::from(
                "Formula metadata has invalid identities",
            )));
        }
        Ok(Self {
            catalog: CatalogView::from_parts(identity, index),
            aliases,
            executables,
        })
    }

    /// Read the Formula index that search prepared, if it is present.
    #[must_use]
    pub fn local(cache_dir: &Path, source: &str, system: &str) -> Option<Self> {
        let snapshot =
            cache::read_snapshot::<serde_json::Value>(cache_dir, KIND, source, system, None)?;
        Self::decode(snapshot.identity("Homebrew Formulae"), &snapshot.payload).ok()
    }

    /// Read the local Formula index, or evaluate it when it is missing.
    /// # Errors
    /// Returns an error when the evaluation or decoding fails.
    pub fn local_or_load(
        nix: &Nix,
        cache_dir: &Path,
        source: &str,
        system: &str,
    ) -> Result<Self, CatalogError> {
        match Self::local(cache_dir, source, system) {
            Some(view) => Ok(view),
            None => Self::load(nix, source),
        }
    }

    /// Formula names on `system` whose executables include `command`.
    #[must_use]
    pub fn providing(&self, system: &str, command: &str) -> Vec<String> {
        self.executables
            .iter()
            .filter(|(name, executables)| {
                executables
                    .as_ref()
                    .is_some_and(|executables| executables.iter().any(|item| item == command))
                    && self.catalog.index.record(system, SOURCE, name).is_some()
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// The executables of one Formula, when the index declares them.
    #[must_use]
    pub fn executables(&self, name: &str) -> &[String] {
        self.executables
            .get(name)
            .and_then(Option::as_deref)
            .unwrap_or_default()
    }

    pub fn load(nix: &Nix, source: &str) -> Result<Self, CatalogError> {
        let identity = nix.source_identity(source)?;
        let reference = identity.locked_url.as_deref().unwrap_or(source);
        let raw = nix.formula_index(reference)?;
        Self::decode(identity, &raw)
    }

    pub fn resolve(&self, system: &str, name: &str) -> Result<String, CatalogError> {
        if !self.catalog.index.targeted(system) {
            return Err(CatalogError::Source(format!(
                "Homebrew Formulae do not support {system}; supported targets: {}",
                self.catalog.index.targets().join(", ")
            )));
        }
        let canonical = self.aliases.get(name).map_or(name, String::as_str);
        if self
            .catalog
            .index
            .record(system, SOURCE, canonical)
            .is_none()
        {
            return Err(CatalogError::NotFound(format!("brew:homebrew/core/{name}")));
        }
        Ok(canonical.to_string())
    }

    /// Judge the Formula record's catalog and host requirements.
    /// # Errors
    /// Returns the retained error if a host-version probe is cancelled.
    pub fn availability(
        &self,
        host: &CaskHost,
        system: &str,
        name: &str,
    ) -> Result<Availability, CatalogError> {
        host.availability(&self.catalog.index, system, SOURCE, name)
    }
}

pub(crate) fn prepare(
    nix: &Nix,
    cache_dir: &Path,
    source: &str,
    system: &str,
) -> Result<usize, CatalogError> {
    let identity = nix.refresh_source_identity(source)?;
    let reference = identity.locked_url.as_deref().ok_or_else(|| {
        CatalogError::Source(String::from("Formula source has no locked reference"))
    })?;
    let revision = identity
        .revision
        .as_deref()
        .ok_or_else(|| CatalogError::Source(String::from("Formula source has no revision")))?;
    let existing =
        cache::read_snapshot::<serde_json::Value>(cache_dir, KIND, source, system, Some(revision))
            .filter(|snapshot| snapshot.locked_reference.as_deref() == Some(reference))
            .filter(|snapshot| FormulaView::decode(identity.clone(), &snapshot.payload).is_ok());
    let raw = match existing {
        Some(snapshot) => snapshot.payload,
        None => nix.formula_index(reference)?,
    };
    let view = FormulaView::decode(identity.clone(), &raw)?;
    let count = view.catalog.index.entries(system).map_or(0, BTreeMap::len);
    cache::write_snapshot(
        cache_dir,
        &cache::Snapshot::new(KIND, source, system, &identity, raw),
    )?;
    Ok(count)
}

pub(crate) fn search(
    cache_dir: &Path,
    source: &str,
    system: &str,
    query: &SearchQuery,
    kept: &mut Kept,
    mut sink: impl FnMut(SearchResult, u32) -> Result<(), CatalogError>,
) -> Result<SourceReport, CatalogError> {
    let Some(snapshot) =
        cache::read_snapshot::<serde_json::Value>(cache_dir, KIND, source, system, None)
    else {
        let mut report = super::not_ready_report(source);
        report.display = Some(String::from("Homebrew Formulae"));
        return Ok(report);
    };
    let identity = snapshot.identity("Homebrew Formulae");
    let reference = identity.locked_url.as_deref().unwrap_or(source);
    let mut report = local::report(
        source,
        reference,
        snapshot.revision.clone(),
        snapshot.saved_unix,
    );
    let view = FormulaView::decode(identity, &snapshot.payload)?;
    report.display = Some(String::from("Homebrew Formulae"));
    if !view.catalog.index.targeted(system) {
        let report = super::SourceReport::skipped_platform(source, view.catalog.index.targets());
        kept.formulae = Some(view);
        return Ok(report);
    }
    let host = CaskHost::default();
    visit_formula_entries(&view, &host, system, query, &report, &mut sink)?;
    kept.formulae = Some(view);
    Ok(report)
}

fn visit_formula_entries(
    view: &FormulaView,
    host: &CaskHost,
    system: &str,
    query: &SearchQuery,
    report: &SourceReport,
    sink: &mut impl FnMut(SearchResult, u32) -> Result<(), CatalogError>,
) -> Result<(), CatalogError> {
    let mut aliases_by_name: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (alias, name) in &view.aliases {
        aliases_by_name.entry(name).or_default().push(alias);
    }
    if let Some(entries) = view.catalog.index.entries(system) {
        for entry in entries.values() {
            let id = format!("brew:{SOURCE}/{}", entry.token);
            let aliases = aliases_by_name
                .get(entry.token.as_str())
                .map_or(&[][..], Vec::as_slice);
            let exact = query.exact(&[&entry.token, &id]) || query.exact(aliases);
            if view
                .executables
                .get(&entry.token)
                .is_some_and(|executables| executables.as_ref().is_some_and(Vec::is_empty))
                && !query.includes_hidden()
                && !query.library_query()
                && !exact
            {
                continue;
            }
            let Some(support) = view
                .availability(host, system, &entry.token)?
                .search_badge(query.unavailable || exact, entry.detail.as_deref())
            else {
                continue;
            };
            let mut row = SearchResult::catalog(entry, "brew", system, report);
            row.name.clone_from(&entry.token);
            row.description = row
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            row.install_command = matches!(support, SupportBadge::Eligible)
                .then(|| vec![String::from("pkg"), String::from("install"), id]);
            row.support = Some(support);
            let mut rank = query.rank(&row);
            for alias in aliases {
                let mut alternative = row.clone();
                alternative.id = format!("brew:{SOURCE}/{alias}");
                alternative.name = (*alias).to_string();
                if let Some(alias_rank) = query.rank(&alternative) {
                    let alias_rank = alias_rank.max(super::matching::ALIAS_RANK);
                    rank = Some(rank.map_or(alias_rank, |rank| rank.min(alias_rank)));
                }
            }
            if let Some(rank) = rank {
                sink(row, rank)?;
            }
        }
    }
    Ok(())
}
