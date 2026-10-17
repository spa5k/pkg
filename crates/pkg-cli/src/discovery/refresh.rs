//! Refresh disposable search data without changing installed packages.

use crate::{
    catalog,
    commands::{CommandError, Session},
};

/// Return whether any source failed. Cancellation stops the operation.
pub fn refresh(session: &Session) -> Result<bool, CommandError> {
    let system = session.nix.system()?;
    catalog::record_search_system(&session.paths.cache_dir, &system)?;
    eprintln!("Preparing local package search. The first run can take several minutes.");
    let mut failed = false;
    eprintln!("Checking Homebrew Formulae…");
    match catalog::brew::prepare(
        &session.nix,
        &session.paths.cache_dir,
        &session.config.sources.formulae,
        &system,
    ) {
        Ok(count) => eprintln!("Ready: Homebrew Formulae search data ({count} formulae)."),
        Err(error) if error.signal().is_some() => return Err(error.into()),
        Err(error) => {
            failed = true;
            eprintln!(
                "Failed: Homebrew Formulae: {}. Previous complete data was kept.",
                crate::commands::concise_cause(&error.to_string())
            );
        }
    }
    for (kind, source) in super::configured_sources(&session.config).into_iter().rev() {
        eprintln!("Checking {}…", kind.label());
        let result = session
            .nix
            .refresh_source_identity(source)
            .map_err(catalog::CatalogError::from)
            .and_then(|identity| match kind {
                catalog::SourceKind::Nixpkgs => catalog::prepare_source(
                    &session.nix,
                    &session.paths.cache_dir,
                    source,
                    &system,
                    &identity,
                ),
                catalog::SourceKind::Cask => catalog::prepare_catalog(
                    &session.nix,
                    &session.paths.cache_dir,
                    source,
                    &system,
                    &identity,
                ),
            });
        match result {
            Ok(()) => eprintln!("Ready: {} search data.", kind.label()),
            Err(error) if error.signal().is_some() => return Err(error.into()),
            Err(error) => {
                failed = true;
                eprintln!(
                    "Failed: {}: {}. Previous complete data was kept.",
                    kind.label(),
                    crate::commands::concise_cause(&error.to_string())
                );
            }
        }
    }
    Ok(failed)
}
