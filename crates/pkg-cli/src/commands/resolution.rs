//! Package Resolution owns source selection, aliases, metadata, and install gates.

use super::{CommandError, Session, TapState};
use crate::catalog::{self, CatalogId, CatalogIdKind, ParsedId};
use crate::nix;
use indexmap::IndexMap;
use std::collections::BTreeMap;

/// Collected data for one info result.
#[derive(Debug)]
pub(super) struct PackageMetadata {
    /// The requested attribute or token.
    pub(super) attribute: String,
    /// The actual full matched native attribute, when a derivation lookup
    /// ran.
    pub(super) matched_attribute: Option<String>,
    /// Evaluated package metadata, when a derivation lookup ran.
    pub(super) meta: Option<nix::SearchMeta>,
    /// Source provenance for the result.
    pub(super) report: catalog::SourceReport,
    /// Cask classification, when the ID is a cask token.
    pub(super) cask: Option<CaskDetails>,
    pub(super) availability: Option<catalog::Availability>,
    pub(super) formula_kind: Option<String>,
}

pub(super) struct ResolvedPackage {
    pub(super) resolved: catalog::CatalogId,
    pub(super) data: PackageMetadata,
    pub(super) reference: String,
    pub(super) installable: String,
    pub(super) system: String,
}

pub(super) struct Resolution<'a> {
    session: &'a Session,
    pub(super) system: String,
    routing: catalog::Routing,
    taps: Option<TapState>,
    casks: catalog::CatalogOnce<'a>,
    formulae: Option<catalog::brew::FormulaView>,
    host: catalog::CaskHost,
}

impl<'a> Resolution<'a> {
    pub(super) fn new(session: &'a Session) -> Result<Self, CommandError> {
        Ok(Self {
            session,
            system: session.nix.system()?,
            routing: catalog::Routing::new(&session.config.sources, BTreeMap::new()),
            taps: None,
            casks: catalog::CatalogOnce::new(&session.nix, &session.config.sources.casks),
            formulae: None,
            host: catalog::CaskHost::default(),
        })
    }

    fn taps(&mut self) -> Result<&mut TapState, CommandError> {
        match &mut self.taps {
            Some(state) => Ok(state),
            empty @ None => {
                let state = TapState::load(self.session)?;
                self.routing = state.routing.clone();
                Ok(empty.insert(state))
            }
        }
    }

    fn resolve(&mut self, parsed: ParsedId, installing: bool) -> Result<CatalogId, CommandError> {
        let cask_only = matches!(parsed, ParsedId::BareCask(_));
        let mut id = match parsed {
            ParsedId::Qualified(id) => id,
            ParsedId::Bare(name) | ParsedId::BareCask(name) => {
                let saved = self.taps()?.saved_all()?;
                let result = catalog::resolve_bare(
                    &self.session.nix,
                    (!cask_only).then_some((
                        self.routing.sources.nixpkgs.as_str(),
                        self.session.paths.cache_dir.as_path(),
                    )),
                    &saved,
                    &name,
                    &self.system,
                    &mut self.casks,
                );
                result.map_err(|error| {
                    if installing {
                        super::search::install_resolution_error(
                            self.session,
                            &name,
                            cask_only.then_some("cask"),
                            error,
                        )
                    } else {
                        error.into()
                    }
                })?
            }
        };
        if let CatalogIdKind::Formula(name) = id.kind() {
            let view = match &mut self.formulae {
                Some(view) => view,
                empty @ None => empty.insert(catalog::brew::FormulaView::load(
                    &self.session.nix,
                    &self.session.config.sources.formulae,
                )?),
            };
            id = CatalogId::formula(&view.resolve(&self.system, name)?)?;
        }
        if matches!(id.kind(), CatalogIdKind::Cask { source, .. } if source != nix::OFFICIAL_CASK_SOURCE)
        {
            self.taps()?;
        }
        Ok(id)
    }

    fn catalog_data(
        &mut self,
        id: &CatalogId,
        reference: &str,
        attribute: &str,
    ) -> Result<Option<PackageMetadata>, CommandError> {
        match id.kind() {
            CatalogIdKind::Formula(name) => {
                let view = self
                    .formulae
                    .as_ref()
                    .ok_or_else(|| String::from("formula catalog did not load"))?;
                let record = view
                    .catalog
                    .index
                    .record(&self.system, "homebrew/core", name);
                let mut report = catalog::report_for(reference, &view.catalog.identity);
                report.display = Some(String::from("Homebrew Formulae"));
                Ok(Some(PackageMetadata {
                    attribute: attribute.to_owned(),
                    matched_attribute: Some(catalog::native_package_attribute(
                        &self.system,
                        &format!("homebrew/core/{name}"),
                    )),
                    meta: record.map(|entry| catalog::catalog_meta(name, entry)),
                    report,
                    cask: None,
                    availability: Some(view.availability(&self.host, &self.system, name)?),
                    formula_kind: record.and_then(|entry| entry.kind.clone()),
                }))
            }
            CatalogIdKind::Cask { source, token } => {
                let view = if source == nix::OFFICIAL_CASK_SOURCE {
                    self.casks.load()?.clone()
                } else {
                    catalog::CatalogView::from_saved(self.taps()?.saved_one(source)?)
                };
                Ok(Some(cask_info(
                    &view,
                    &self.host,
                    &self.system,
                    reference,
                    source,
                    token,
                )?))
            }
            _ => Ok(None),
        }
    }

    pub(super) fn inspect(&mut self, parsed: ParsedId) -> Result<ResolvedPackage, CommandError> {
        let resolved = self.resolve(parsed, false)?;
        let (reference, attribute) = resolved.source_and_attribute(&self.routing, &self.system)?;
        let installable = format!("{reference}#{attribute}");
        let data = if let Some(data) = self.catalog_data(&resolved, &reference, &attribute)? {
            data
        } else {
            let (exact, report) = catalog::exact_lookup(&self.session.nix, &reference, &attribute).map_err(|error| match error {
                catalog::CatalogError::NotFound(_) => CommandError::Message(format!("`{}` has no exact match in `{reference}`; pkg does not report package identities it cannot evaluate", resolved.qualified())),
                catalog::CatalogError::Ambiguous { name, choices } => CommandError::Message(format!("`{name}` is ambiguous in `{reference}`; choose one of: {}", choices.iter().map(|choice| request_qualified(&resolved, choice)).collect::<Vec<_>>().join(", "))),
                error => error.into(),
            })?;
            PackageMetadata {
                attribute,
                matched_attribute: Some(exact.attribute),
                meta: Some(exact.meta),
                report,
                cask: None,
                availability: None,
                formula_kind: None,
            }
        };
        Ok(ResolvedPackage {
            resolved,
            data,
            reference,
            installable,
            system: self.system.clone(),
        })
    }

    pub(super) fn install_batch(
        &mut self,
        parsed: Vec<ParsedId>,
    ) -> Result<IndexMap<String, String>, CommandError> {
        let mut installables = IndexMap::new();
        for request in parsed {
            let id = self.resolve(request, true)?;
            let (reference, attribute) = id.source_and_attribute(&self.routing, &self.system)?;
            if let Some(data) = self.catalog_data(&id, &reference, &attribute)?
                && let Some(availability) = data.availability
            {
                availability
                    .require()
                    .map_err(|reason| format!("{}: {reason}", id.qualified()))?;
            }
            let installable = format!("{reference}#{attribute}");
            installables.insert(installable, id.qualified());
        }
        Ok(installables)
    }
}

/// One ambiguity choice as a CLI ID that round-trips through `info` and
/// `install` for the request it came from.
///
/// Keep the catalog's full attribute path when short names would be ambiguous.
fn request_qualified(resolved: &catalog::CatalogId, attribute: &str) -> String {
    match resolved.kind() {
        catalog::CatalogIdKind::Nixpkgs(_) => format!("nixpkgs:{attribute}"),
        catalog::CatalogIdKind::Formula(name) => format!("brew:homebrew/core/{name}"),
        catalog::CatalogIdKind::Explicit { reference, .. } => {
            format!("{reference}#{attribute}")
        }
        catalog::CatalogIdKind::Cask { source, token } => format!("cask:{source}/{token}"),
    }
}

/// Build info data for one cask identity from the official generated index
/// or the source's saved catalog capture.
///
/// The official source's index is evaluated once per command run; an
/// imported tap answers entirely from its saved capture. No package
/// derivation is evaluated for cask metadata. Eligible, excluded, and
/// unknown tokens all report their recorded state, and a system outside the
/// catalog targets reports the target list.
fn cask_info(
    view: &catalog::CatalogView,
    host: &catalog::CaskHost,
    system: &str,
    reference: &str,
    source: &str,
    token: &str,
) -> Result<PackageMetadata, catalog::CatalogError> {
    let attribute = catalog::package_attribute(system, &nix::full_entry_id(source, token));
    let record = view.index.record(system, source, token);
    let meta = record.map(|entry| catalog::catalog_meta(token, entry));
    let targeted = view.index.targeted(system);
    let cask = if targeted {
        match record {
            Some(entry) => match entry.status {
                nix::EntryStatus::Eligible => CaskDetails::Eligible {
                    kind: entry.kind.clone(),
                    homepage: entry.homepage.clone(),
                },
                nix::EntryStatus::Excluded => CaskDetails::Excluded {
                    reason: entry.reason.clone().unwrap_or_default(),
                    detail: entry.detail.clone(),
                    homepage: entry.homepage.clone(),
                },
            },
            None => CaskDetails::Unknown,
        }
    } else {
        CaskDetails::UnsupportedPlatform {
            system: system.to_owned(),
            targets: view.index.targets().to_vec(),
        }
    };
    Ok(PackageMetadata {
        attribute,
        matched_attribute: None,
        meta,
        report: if targeted {
            catalog::report_for(reference, &view.identity)
        } else {
            catalog::SourceReport::skipped_platform(reference, view.index.targets())
        },
        availability: Some(host.availability(&view.index, system, source, token)?),
        formula_kind: None,
        cask: Some(cask),
    })
}

/// The recorded cask state for text and JSON output.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub(super) enum CaskDetails {
    UnsupportedPlatform {
        system: String,
        targets: Vec<String>,
    },
    Eligible {
        kind: Option<String>,
        homepage: Option<String>,
    },
    Excluded {
        reason: String,
        detail: Option<String>,
        #[serde(skip)]
        homepage: Option<String>,
    },
    Unknown,
}

/// Check each upgrade against its installed original source before mutation.
pub(super) fn gate_upgrade(
    session: &Session,
    installed: &BTreeMap<String, nix::ProfileEntry>,
    movable: &[String],
) -> Result<(), CommandError> {
    let mut resolution = None;
    let mut views = BTreeMap::new();
    for key in movable {
        let entry = &installed[key];
        let Some((system, source, token)) = cask_identity(&entry.attr_path) else {
            continue;
        };
        let formula = source == "homebrew/core";
        let saved =
            crate::tap::store::source_of_reference(&session.paths.state_home, &entry.original_url);
        let nixpkgs = catalog::canonical_source(&entry.original_url)
            == catalog::canonical_source(&session.config.sources.nixpkgs);
        if nixpkgs && (formula || saved.is_none())
            || saved.as_deref().is_some_and(|saved| saved != source)
        {
            continue;
        }
        let resolver = match &mut resolution {
            Some(resolver) => resolver,
            empty @ None => empty.insert(Resolution::new(session)?),
        };
        let prefix = if formula { "brew" } else { "cask" };
        if system != resolver.system {
            return Err(format!(
                "{prefix}:{source}/{token} was installed for {system}; this host is {}",
                resolver.system
            )
            .into());
        }
        let view = match views.entry((formula, entry.original_url.clone())) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(slot) => slot.insert(if saved.is_some() {
                catalog::CatalogView::from_saved(resolver.taps()?.saved_one(&source)?)
            } else if formula {
                catalog::brew::FormulaView::load(&session.nix, &entry.original_url)?.catalog
            } else {
                catalog::CatalogView::load(&session.nix, &entry.original_url)?
            }),
        };
        resolver
            .host
            .availability(&view.index, &system, &source, &token)?
            .require()
            .map_err(|reason| format!("{prefix}:{source}/{token}: {reason}"))?;
    }
    Ok(())
}

/// The exact cask identity of one native attribute path.
///
/// The path must be exactly `packages.{system}.{encoded full id}`: the
/// final segment decodes through the shared strict inverse, the id
/// splits as one whole `owner/tap/token`. Exactly three segments and
/// strict underscore escapes retain the whole recorded path.
fn cask_identity(attr_path: &str) -> Option<(String, String, String)> {
    let mut parts = attr_path.split('.');
    if parts.next()? != "packages" {
        return None;
    }
    let system = parts.next()?.to_string();
    let segment = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let id = cask_catalog::decode_package_attribute(segment).ok()?;
    let (source, token) = nix::split_entry_id(&id)?;
    Some((system, source.to_string(), token.to_string()))
}
