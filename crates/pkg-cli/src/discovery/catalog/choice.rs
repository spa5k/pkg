//! One package for a bare name.
//!
//! Install, info, and search use one rule:
//!
//! 1. Check the sources in this order: Nixpkgs, Homebrew Casks, Homebrew
//!    Formulae, and saved taps.
//! 2. Matches that cannot install on this computer and hidden package sets
//!    do not count.
//! 3. In each source, an exact name matches first, then the main command of
//!    a package. The first source with a match wins: `rg` selects Nixpkgs
//!    `ripgrep` before the Formula alias `rg`. Two matches in one source
//!    stay ambiguous.
//! 4. When nothing installable matches, an unavailable exact match is
//!    chosen, so the install check can name the reason.
//!
//! The local search data answers the rule. Nix answers only for data that
//! is missing, and for a name that the local data does not know.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::config::Sources;
use crate::nix::{self, CatalogIndex, Nix, NixError};
use crate::tap::SavedCatalogs;

use super::brew::FormulaView;
use super::matching::SearchFields;
use super::native::{NativeRow, Snapshot};
use super::{
    Availability, CaskHost, CatalogError, CatalogId, CatalogIdKind, CatalogOnce, CatalogView,
    SearchQuery, SourceReport, SourceStatus,
};

/// Where a candidate comes from, in choice order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Nixpkgs,
    Cask,
    Formula,
    Tap,
}

/// Every source, in choice order.
const TIERS: [Tier; 4] = [Tier::Nixpkgs, Tier::Cask, Tier::Formula, Tier::Tap];

#[derive(Debug, Clone)]
struct Candidate {
    tier: Tier,
    id: CatalogId,
    available: bool,
}

/// Which sources a bare name can select.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every source: a plain bare name.
    All,
    /// Cask sources only: `cask:token`.
    Casks,
}

/// The package that a bare name selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The selected package.
    pub id: CatalogId,
    /// Other installable packages with the same name, as short IDs.
    pub alternatives: Vec<String>,
    /// Whether the name matched the main command of the package.
    pub by_command: bool,
}

/// Local data that a search already read. `Names::local` reuses it, so
/// one command reads each file once.
#[derive(Default)]
pub struct Kept {
    pub(super) native: Option<Snapshot>,
    pub(super) casks: Option<Rc<CatalogIndex>>,
    pub(super) formulae: Option<FormulaView>,
}

/// Local name data for one command run.
///
/// Cask and Formula data load on first use. A name that Nixpkgs answers
/// reads only the Nixpkgs database.
pub struct Names {
    system: String,
    saved: SavedCatalogs,
    host: CaskHost,
    native: Option<Snapshot>,
    cache_dir: PathBuf,
    sources: Sources,
    casks: OnceCell<Option<Rc<CatalogIndex>>>,
    formulae: OnceCell<Option<FormulaView>>,
}

impl Names {
    /// Open the local search data, and reuse the data in `kept`. No Nix
    /// command runs.
    #[must_use]
    pub fn local(
        cache_dir: &Path,
        sources: &Sources,
        system: &str,
        saved: SavedCatalogs,
        kept: Kept,
    ) -> Self {
        let native = kept
            .native
            .or_else(|| Snapshot::read(cache_dir, &sources.nixpkgs, system));
        Self {
            system: system.to_string(),
            saved,
            host: CaskHost::default(),
            native,
            cache_dir: cache_dir.to_path_buf(),
            sources: sources.clone(),
            casks: kept
                .casks
                .map_or_else(OnceCell::new, |index| OnceCell::from(Some(index))),
            formulae: kept
                .formulae
                .map_or_else(OnceCell::new, |view| OnceCell::from(Some(view))),
        }
    }

    fn casks(&self) -> Option<&Rc<CatalogIndex>> {
        self.casks
            .get_or_init(|| {
                CatalogView::local(&self.cache_dir, &self.sources.casks, &self.system)
                    .map(|view| view.index)
            })
            .as_ref()
    }

    fn formulae(&self) -> Option<&FormulaView> {
        self.formulae
            .get_or_init(|| {
                FormulaView::local(&self.cache_dir, &self.sources.formulae, &self.system)
            })
            .as_ref()
    }

    /// The package that `name` selects from local data only.
    ///
    /// Search uses it to show the shortest name that installs a row. An
    /// ambiguous name, a removed Nixpkgs alias, or missing data selects
    /// nothing: `choose` asks Nix in these cases, and the result can differ.
    #[must_use]
    pub fn local_choice(&self, name: &str) -> Option<CatalogId> {
        if self.native.is_none() || self.native_failed(name).unwrap_or(true) {
            return None;
        }
        let query = query(name).ok()?;
        let mut exact = Vec::new();
        for tier in TIERS {
            let found = self.tier(tier, name, &query).ok()??;
            let decided = found.iter().any(|candidate| candidate.available)
                || self
                    .tier_commands(tier, name, &query)
                    .ok()?
                    .iter()
                    .any(|candidate| candidate.available);
            exact.extend(found);
            if decided {
                // Later sources cannot change the choice.
                break;
            }
        }
        select(&exact, |tier| self.tier_commands(tier, name, &query), name)
            .ok()
            .flatten()
            .map(|choice| choice.id)
    }

    /// Select one package for `name`.
    ///
    /// Missing local data falls back to Nix: the Nixpkgs attribute check,
    /// the official cask index, and the Formula index. A name that the
    /// local data does not know gets one Nix check, which finds packages
    /// newer than the data and reports removed Nixpkgs aliases.
    /// # Errors
    ///
    /// Returns an error when no package matches, one source has several
    /// matches, a needed source cannot load, or Nix cannot evaluate the
    /// package.
    pub fn choose(
        &mut self,
        nix: &Nix,
        sources: &Sources,
        casks: &mut CatalogOnce<'_>,
        name: &str,
        scope: Scope,
    ) -> Result<Choice, CatalogError> {
        let query = query(name)?;
        if self.casks().is_none() {
            self.casks = OnceCell::from(Some(casks.load()?.index.clone()));
        }
        let local_native = self.native.is_some();
        if scope == Scope::All && local_native && self.native_failed(name)? {
            // Nixpkgs comes first, and it keeps this name as an attribute
            // that fails, such as a removed alias. Nix reports the cause.
            let late = nix_exact(nix, &sources.nixpkgs, name, &self.system)?;
            if let Some(choice) = pick(&late, true, name)? {
                return Ok(choice);
            }
        }
        let mut exact = Vec::new();
        if scope == Scope::All && !local_native {
            exact.extend(nix_exact(nix, &sources.nixpkgs, name, &self.system)?);
        }
        exact.extend(self.exact(name, scope, &query)?);
        if scope == Scope::All
            && self.formulae().is_none()
            && !exact.iter().any(|candidate| candidate.available)
            && !self
                .tier_commands(Tier::Nixpkgs, name, &query)?
                .iter()
                .any(|candidate| candidate.available)
        {
            // The Formula tier comes after Nixpkgs and Casks. Without local
            // data, a failed evaluation leaves the tier empty.
            self.formulae = OnceCell::from(FormulaView::load(nix, &sources.formulae).ok());
            exact.extend(self.formula_exact(name)?);
        }
        // A `cask:token` request matches tokens only, never commands.
        let commands = |tier| match scope {
            Scope::All => self.tier_commands(tier, name, &query),
            Scope::Casks => Ok(Vec::new()),
        };
        if let Some(choice) = select(&exact, commands, name)? {
            return Ok(choice);
        }
        if scope == Scope::All && local_native {
            let late = nix_exact(nix, &sources.nixpkgs, name, &self.system)?;
            if let Some(choice) = pick(&late, true, name)? {
                return Ok(choice);
            }
        }
        Err(CatalogError::NotFound(name.to_string()))
    }

    /// The local Nixpkgs row and provenance for one attribute, if exactly
    /// one row matches.
    /// # Errors
    ///
    /// Returns an error when the local database cannot be read.
    pub fn native_row(
        &self,
        moving_source: &str,
        attribute: &str,
    ) -> Result<Option<(NativeRow, SourceReport)>, CatalogError> {
        let Some(snapshot) = &self.native else {
            return Ok(None);
        };
        let mut rows = Vec::new();
        for candidate in attribute_paths(attribute, &self.system) {
            if let Some(row) = snapshot.attribute(&candidate)? {
                rows.push(row);
            }
        }
        if rows.len() != 1 {
            return Ok(None);
        }
        let report = SourceReport {
            source: moving_source.to_string(),
            display: Some(super::cask::source_display(&snapshot.header.reference)),
            locked_reference: Some(snapshot.header.reference.clone()),
            revision: snapshot.header.revision.clone(),
            status: SourceStatus::Cached,
            detail: None,
        };
        Ok(rows.pop().map(|row| (row, report)))
    }

    /// Whether a Nixpkgs attribute for `name` failed to evaluate when the
    /// local data was prepared.
    fn native_failed(&self, name: &str) -> Result<bool, CatalogError> {
        let Some(snapshot) = &self.native else {
            return Ok(false);
        };
        for attribute in attribute_paths(name, &self.system) {
            if snapshot.failed(&attribute)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The executables that one Formula declares, from local data.
    #[must_use]
    pub fn formula_commands(&self, name: &str) -> Vec<String> {
        self.formulae()
            .map(|view| view.executables(name).to_vec())
            .unwrap_or_default()
    }

    fn exact(
        &self,
        name: &str,
        scope: Scope,
        query: &SearchQuery,
    ) -> Result<Vec<Candidate>, CatalogError> {
        let tiers: &[Tier] = match scope {
            Scope::All => &TIERS,
            Scope::Casks => &[Tier::Cask, Tier::Tap],
        };
        let mut found = Vec::new();
        for tier in tiers {
            found.extend(self.tier(*tier, name, query)?.unwrap_or_default());
        }
        Ok(found)
    }

    /// Exact matches in one source. `None` means that the local data for
    /// the source is missing.
    fn tier(
        &self,
        tier: Tier,
        name: &str,
        query: &SearchQuery,
    ) -> Result<Option<Vec<Candidate>>, CatalogError> {
        match tier {
            Tier::Nixpkgs => self
                .native
                .as_ref()
                .map(|snapshot| native_exact(snapshot, name, &self.system, query))
                .transpose(),
            Tier::Cask => self
                .casks()
                .map(|index| self.cask_exact(index, name))
                .transpose(),
            Tier::Formula => {
                if self.formulae().is_none() {
                    return Ok(None);
                }
                self.formula_exact(name).map(Some)
            }
            Tier::Tap => self.tap_exact(name).map(Some),
        }
    }

    fn cask_exact(&self, index: &CatalogIndex, name: &str) -> Result<Vec<Candidate>, CatalogError> {
        let mut found = Vec::new();
        for source in index.sources_with_token(&self.system, name) {
            found.push(Candidate {
                tier: Tier::Cask,
                id: CatalogId::cask(&source, name).map_err(CatalogError::Source)?,
                available: available(&self.host.availability(
                    index,
                    &self.system,
                    &source,
                    name,
                )?),
            });
        }
        Ok(found)
    }

    fn tap_exact(&self, name: &str) -> Result<Vec<Candidate>, CatalogError> {
        let mut found = Vec::new();
        for catalog in self.saved.iter() {
            let index = catalog.index();
            if index.record(&self.system, catalog.source(), name).is_some() {
                found.push(Candidate {
                    tier: Tier::Tap,
                    id: CatalogId::cask(catalog.source(), name).map_err(CatalogError::Source)?,
                    available: available(&self.host.availability(
                        index,
                        &self.system,
                        catalog.source(),
                        name,
                    )?),
                });
            }
        }
        Ok(found)
    }

    fn formula_exact(&self, name: &str) -> Result<Vec<Candidate>, CatalogError> {
        let Some(view) = self.formulae() else {
            return Ok(Vec::new());
        };
        let Ok(canonical) = view.resolve(&self.system, name) else {
            return Ok(Vec::new());
        };
        Ok(vec![Candidate {
            tier: Tier::Formula,
            id: CatalogId::formula(&canonical).map_err(CatalogError::Source)?,
            available: available(&view.availability(&self.host, &self.system, &canonical)?),
        }])
    }

    /// Packages in one source whose main command is `name`. Only Nixpkgs
    /// and Formulae declare commands.
    fn tier_commands(
        &self,
        tier: Tier,
        name: &str,
        query: &SearchQuery,
    ) -> Result<Vec<Candidate>, CatalogError> {
        let mut found = Vec::new();
        match tier {
            Tier::Nixpkgs => {
                if let Some(snapshot) = &self.native {
                    let rows = snapshot
                        .commands(name)?
                        .into_iter()
                        .filter(|row| visible(query, row, &self.system))
                        .collect();
                    for row in prefer(
                        rows,
                        |row| row.exposed.as_str(),
                        |row| Some(row.version.as_str()),
                    ) {
                        found.push(Candidate {
                            tier,
                            id: CatalogId::nixpkgs(&row.exposed).map_err(CatalogError::Source)?,
                            available: row.unavailable.is_none(),
                        });
                    }
                }
            }
            Tier::Formula => {
                if let Some(view) = self.formulae() {
                    let mut names = Vec::new();
                    for formula in view.providing(&self.system, name) {
                        if available(&view.availability(&self.host, &self.system, &formula)?) {
                            names.push(formula);
                        }
                    }
                    for formula in prefer(names, String::as_str, |_| None) {
                        found.push(Candidate {
                            tier,
                            id: CatalogId::formula(&formula).map_err(CatalogError::Source)?,
                            available: true,
                        });
                    }
                }
            }
            Tier::Cask | Tier::Tap => {}
        }
        Ok(found)
    }
}

/// The short ID that installs one package: `nixpkgs:NAME`, `cask:TOKEN`
/// for the official casks, `brew:NAME`, or the full ID.
#[must_use]
pub fn short_id(id: &CatalogId) -> String {
    match id.kind() {
        CatalogIdKind::Nixpkgs(attribute) => format!("nixpkgs:{attribute}"),
        CatalogIdKind::Formula(name) => format!("brew:{name}"),
        CatalogIdKind::Cask { source, token } if source == nix::OFFICIAL_CASK_SOURCE => {
            format!("cask:{token}")
        }
        _ => id.qualified(),
    }
}

fn query(name: &str) -> Result<SearchQuery, CatalogError> {
    SearchQuery::new(name, false, false)
        .map(|query| query.fuzzy(false))
        .map_err(|error| CatalogError::Source(error.to_string()))
}

const fn available(availability: &Availability) -> bool {
    !matches!(availability, Availability::Unavailable { .. })
}

/// The full attributes that a bare Nixpkgs name can mean on `system`.
fn attribute_paths(name: &str, system: &str) -> Vec<String> {
    if name.starts_with("packages.") || name.starts_with("legacyPackages.") {
        vec![name.to_string()]
    } else {
        vec![
            format!("legacyPackages.{system}.{name}"),
            format!("packages.{system}.{name}"),
        ]
    }
}

fn visible(query: &SearchQuery, row: &NativeRow, system: &str) -> bool {
    let id = format!("nixpkgs:{}", row.exposed);
    query.visible(&SearchFields {
        id: &id,
        attribute: &row.attribute,
        name: &row.name,
        version: &row.version,
        description: &row.description,
        source: "nixpkgs",
        system,
        command: row.command.as_deref(),
    })
}

/// Exact Nixpkgs matches: the attribute itself, else visible packages with
/// this package name or last attribute segment.
fn native_exact(
    snapshot: &Snapshot,
    name: &str,
    system: &str,
    query: &SearchQuery,
) -> Result<Vec<Candidate>, CatalogError> {
    let mut rows = Vec::new();
    for attribute in attribute_paths(name, system) {
        if let Some(row) = snapshot.attribute(&attribute)? {
            rows.push(row);
        }
    }
    if rows.is_empty() {
        let named = snapshot
            .named(name)?
            .into_iter()
            .filter(|row| visible(query, row, system))
            .collect();
        rows = prefer(
            named,
            |row| row.exposed.as_str(),
            |row| Some(row.version.as_str()),
        );
    }
    rows.iter()
        .map(|row| {
            Ok(Candidate {
                tier: Tier::Nixpkgs,
                id: CatalogId::nixpkgs(&row.exposed).map_err(CatalogError::Source)?,
                available: row.unavailable.is_none(),
            })
        })
        .collect()
}

/// Keep top-level names before nested ones. When one name is the start of
/// every other name, keep only that name: `nodejs` before `nodejs_22`.
/// A name that adds only a number to it and has a newer major version
/// wins instead: `awscli2` (2.x) before `awscli` (1.x).
fn prefer<T>(
    items: Vec<T>,
    name: impl Fn(&T) -> &str,
    version: impl Fn(&T) -> Option<&str>,
) -> Vec<T> {
    let (top, nested): (Vec<T>, Vec<T>) = items
        .into_iter()
        .partition(|item| !name(item).contains('.'));
    let mut pool = if top.is_empty() { nested } else { top };
    let base = pool.iter().position(|candidate| {
        let prefix = name(candidate);
        pool.iter().all(|other| name(other).starts_with(prefix))
    });
    let Some(base) = base.filter(|_| pool.len() > 1) else {
        return pool;
    };
    let base_name = name(&pool[base]);
    let newest = pool
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            name(candidate)
                .strip_prefix(base_name)
                .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter_map(|(index, candidate)| Some((index, major(version(candidate)?)?)))
        .filter(|(_, candidate)| {
            version(&pool[base])
                .and_then(major)
                .is_some_and(|base| candidate > &base)
        })
        .max_by_key(|(_, major)| *major)
        .map(|(index, _)| index);
    vec![pool.swap_remove(newest.unwrap_or(base))]
}

/// The major version number: `2` for `2.31.4`.
fn major(version: &str) -> Option<u64> {
    let digits: String = version.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Walk the sources in order. In each source, an installable exact name
/// wins, then an installable main command. When no source has one, an
/// unavailable exact name wins.
fn select(
    exact: &[Candidate],
    commands: impl Fn(Tier) -> Result<Vec<Candidate>, CatalogError>,
    name: &str,
) -> Result<Option<Choice>, CatalogError> {
    for tier in TIERS {
        if exact
            .iter()
            .any(|candidate| candidate.tier == tier && candidate.available)
        {
            return pick(exact, true, name);
        }
        if let Some(mut choice) = pick(&commands(tier)?, true, name)? {
            choice.by_command = true;
            choice.alternatives = Vec::new();
            for candidate in exact.iter().filter(|candidate| candidate.available) {
                let alternative = short_id(&candidate.id);
                if candidate.id != choice.id && !choice.alternatives.contains(&alternative) {
                    choice.alternatives.push(alternative);
                }
            }
            return Ok(Some(choice));
        }
    }
    pick(exact, false, name)
}

fn pick(
    candidates: &[Candidate],
    available_only: bool,
    name: &str,
) -> Result<Option<Choice>, CatalogError> {
    let pool: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| candidate.available || !available_only)
        .collect();
    let Some(tier) = pool.iter().map(|candidate| candidate.tier).min() else {
        return Ok(None);
    };
    let mut chosen: Vec<&CatalogId> = pool
        .iter()
        .filter(|candidate| candidate.tier == tier)
        .map(|candidate| &candidate.id)
        .collect();
    chosen.dedup();
    let [id] = chosen.as_slice() else {
        return Err(CatalogError::Ambiguous {
            name: name.to_string(),
            choices: chosen.iter().map(|id| short_id(id)).collect(),
        });
    };
    let mut alternatives: Vec<String> = Vec::new();
    for candidate in candidates {
        let alternative = short_id(&candidate.id);
        if candidate.available && candidate.tier != tier && !alternatives.contains(&alternative) {
            alternatives.push(alternative);
        }
    }
    Ok(Some(Choice {
        id: (*id).clone(),
        alternatives,
        by_command: false,
    }))
}

/// Exact Nixpkgs attributes through Nix, at the current source revision.
fn nix_exact(
    nix: &Nix,
    source: &str,
    name: &str,
    system: &str,
) -> Result<Vec<Candidate>, CatalogError> {
    let identity = nix.source_identity(source).map_err(CatalogError::from)?;
    let reference = identity.locked_url.as_deref().unwrap_or(source);
    let paths: Vec<Vec<String>> = attribute_paths(name, system)
        .iter()
        .map(|attribute| attribute.split('.').map(str::to_string).collect())
        .filter(|path: &Vec<String>| path.get(1).is_some_and(|target| target == system))
        .collect();
    let nodes = nix
        .exact_metadata(reference, &paths)
        .map_err(|error| unusable(name, error))?;
    let attributes: Vec<String> = nodes
        .into_iter()
        .filter(|node| matches!(node.value, nix::MetadataValue::Package { .. }))
        .map(|node| node.path.join("."))
        .collect();
    let attributes: Vec<&str> = attributes.iter().map(String::as_str).collect();
    super::search::unique_exposed_forms(&attributes, Some(system))
        .into_iter()
        .map(|attribute| {
            Ok(Candidate {
                tier: Tier::Nixpkgs,
                id: CatalogId::nixpkgs(attribute).map_err(CatalogError::Source)?,
                available: true,
            })
        })
        .collect()
}

/// A failed evaluation of one package becomes one short cause.
pub(super) fn unusable(name: &str, error: NixError) -> CatalogError {
    match error.evaluation_cause().map(str::to_string) {
        Some(cause) => CatalogError::Unusable {
            name: name.to_string(),
            cause,
        },
        None => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(tier: Tier, id: &str, available: bool) -> Candidate {
        Candidate {
            tier,
            id: match super::super::parse_id(id).unwrap() {
                super::super::ParsedId::Qualified(id) => id,
                _ => panic!("qualified fixture"),
            },
            available,
        }
    }

    #[test]
    fn the_first_source_wins_and_names_the_others() {
        let exact = [
            candidate(Tier::Cask, "cask:homebrew/cask/slack", true),
            candidate(Tier::Nixpkgs, "nixpkgs:slack", true),
        ];
        let choice = pick(&exact, true, "slack").unwrap().unwrap();
        assert_eq!(short_id(&choice.id), "nixpkgs:slack");
        assert_eq!(choice.alternatives, ["cask:slack"]);
    }

    #[test]
    fn unavailable_matches_do_not_count_until_nothing_else_matches() {
        let exact = [
            candidate(Tier::Nixpkgs, "nixpkgs:firefox", true),
            candidate(Tier::Cask, "cask:homebrew/cask/firefox", false),
        ];
        let choice = select(&exact, |_| Ok(Vec::new()), "firefox")
            .unwrap()
            .unwrap();
        assert_eq!(short_id(&choice.id), "nixpkgs:firefox");
        assert_eq!(choice.alternatives, Vec::<String>::new());

        let only = [candidate(Tier::Cask, "cask:homebrew/cask/firefox", false)];
        let choice = select(&only, |_| Ok(Vec::new()), "firefox")
            .unwrap()
            .unwrap();
        assert_eq!(short_id(&choice.id), "cask:firefox");
    }

    #[test]
    fn two_matches_in_one_source_stay_ambiguous() {
        let exact = [
            candidate(Tier::Tap, "cask:acme/tools/probe", true),
            candidate(Tier::Tap, "cask:other/tools/probe", true),
        ];
        assert!(matches!(
            pick(&exact, true, "probe"),
            Err(CatalogError::Ambiguous { choices, .. }) if choices.len() == 2
        ));
    }

    #[test]
    fn a_command_in_an_earlier_source_wins_over_a_later_name() {
        let commands = |tier| {
            Ok(if tier == Tier::Nixpkgs {
                vec![candidate(Tier::Nixpkgs, "nixpkgs:ripgrep", true)]
            } else {
                Vec::new()
            })
        };
        let alias = [candidate(Tier::Formula, "brew:homebrew/core/ripgrep", true)];
        let choice = select(&alias, commands, "rg").unwrap().unwrap();
        assert_eq!(short_id(&choice.id), "nixpkgs:ripgrep");
        assert!(choice.by_command);
        assert_eq!(choice.alternatives, ["brew:ripgrep"]);

        let name = [candidate(Tier::Nixpkgs, "nixpkgs:rg", true)];
        let choice = select(&name, commands, "rg").unwrap().unwrap();
        assert_eq!(short_id(&choice.id), "nixpkgs:rg");
        assert!(!choice.by_command);
    }

    #[test]
    fn preferred_names_are_top_level_and_shortest_common_starts() {
        let names = vec!["nodejs_22", "nodejs", "nodejs-slim"];
        assert_eq!(prefer(names, |name| name, |_| None), ["nodejs"]);
        let names = vec!["vscode", "vscodium"];
        assert_eq!(prefer(names, |name| name, |_| None), ["vscode", "vscodium"]);
        let names = vec!["haskellPackages.rg", "ripgrep"];
        assert_eq!(prefer(names, |name| name, |_| None), ["ripgrep"]);
        let versioned = vec![("awscli", "1.44.21"), ("awscli2", "2.31.4")];
        assert_eq!(
            prefer(versioned, |row| row.0, |row| Some(row.1)),
            [("awscli2", "2.31.4")]
        );
        let versioned = vec![("python3", "3.14.8"), ("python313", "3.13.9")];
        assert_eq!(
            prefer(versioned, |row| row.0, |row| Some(row.1)),
            [("python3", "3.14.8")]
        );
        let versioned = vec![("gnupg", "2.4.9"), ("gnupg1", "1.4.23")];
        assert_eq!(
            prefer(versioned, |row| row.0, |row| Some(row.1)),
            [("gnupg", "2.4.9")]
        );
    }
}
