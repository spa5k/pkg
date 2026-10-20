//! Refresh disposable search data without changing installed packages.

use crate::{
    catalog,
    commands::{CommandError, Session},
    output,
};

/// Return whether any source failed. Cancellation stops the operation.
///
/// An interactive terminal sees one status line that updates in place.
/// Logs and pipes get one line per source result.
pub fn refresh(session: &Session) -> Result<bool, CommandError> {
    let system = session.nix.system()?;
    catalog::record_search_system(&session.paths.cache_dir, &system)?;
    let mut failed = false;
    output::status("Preparing search data… Homebrew Formulae");
    match catalog::brew::prepare(
        &session.nix,
        &session.paths.cache_dir,
        &session.config.sources.formulae,
        &system,
    ) {
        Ok(count) => output::log(&format!(
            "Ready: Homebrew Formulae search data ({count} formulae)."
        )),
        Err(error) if error.signal().is_some() => {
            output::clear_status();
            return Err(error.into());
        }
        Err(error) => {
            failed = true;
            output::note(&format!(
                "Failed: Homebrew Formulae: {}. Previous complete data was kept.",
                crate::commands::concise_cause(&error.to_string())
            ));
        }
    }
    for (kind, source) in super::configured_sources(&session.config).into_iter().rev() {
        output::status(&format!("Preparing search data… {}", kind.label()));
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
            Ok(()) => output::log(&format!("Ready: {} search data.", kind.label())),
            Err(error) if error.signal().is_some() => {
                output::clear_status();
                return Err(error.into());
            }
            Err(error) => {
                failed = true;
                output::note(&format!(
                    "Failed: {}: {}. Previous complete data was kept.",
                    kind.label(),
                    crate::commands::concise_cause(&error.to_string())
                ));
            }
        }
    }
    output::clear_status();
    Ok(failed)
}
