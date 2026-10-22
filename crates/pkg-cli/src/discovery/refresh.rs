//! Refresh disposable search data without changing installed packages.

use crate::{
    catalog,
    commands::{CommandError, Session},
    output,
};

/// Return whether any source failed. Cancellation stops the operation.
///
/// An interactive terminal sees one status line that updates in place.
/// Logs and pipes get one line before each source, because one source can
/// take minutes. Failures always print.
pub fn refresh(session: &Session) -> Result<bool, CommandError> {
    let system = session.nix.system()?;
    catalog::record_search_system(&session.paths.cache_dir, &system)?;
    let mut failed = false;
    output::status("Preparing search data… Homebrew Formulae");
    output::log("Preparing search data: Homebrew Formulae…");
    match catalog::brew::prepare(
        &session.nix,
        &session.paths.cache_dir,
        &session.config.sources.formulae,
        &system,
    ) {
        Ok(_) => {}
        Err(error) if error.signal().is_some() => {
            output::clear_status();
            return Err(error.into());
        }
        Err(error) => {
            failed = true;
            output::note(&format!(
                "Failed: Homebrew Formulae: {}. Search data did not change.",
                crate::commands::plain_detail(&error.to_string())
            ));
        }
    }
    for (kind, source) in super::configured_sources(&session.config).into_iter().rev() {
        output::status(&format!("Preparing search data… {}", title(kind)));
        output::log(&format!("Preparing search data: {}…", title(kind)));
        let result = match kind {
            catalog::SourceKind::Nixpkgs => {
                catalog::prepare_nixpkgs(&session.nix, &session.paths.cache_dir, source, &system)
            }
            catalog::SourceKind::Cask => session
                .nix
                .refresh_source_identity(source)
                .map_err(catalog::CatalogError::from)
                .and_then(|identity| {
                    catalog::prepare_catalog(
                        &session.nix,
                        &session.paths.cache_dir,
                        source,
                        &system,
                        &identity,
                    )
                }),
        };
        match result {
            Ok(()) => {}
            Err(error) if error.signal().is_some() => {
                output::clear_status();
                return Err(error.into());
            }
            Err(error) => {
                failed = true;
                output::note(&format!(
                    "Failed: {}: {}. Search data did not change.",
                    title(kind),
                    crate::commands::plain_detail(&error.to_string())
                ));
            }
        }
    }
    output::clear_status();
    Ok(failed)
}

/// The source name in progress and failure lines.
const fn title(kind: catalog::SourceKind) -> &'static str {
    match kind {
        catalog::SourceKind::Nixpkgs => "Nixpkgs",
        catalog::SourceKind::Cask => "Homebrew Casks",
    }
}
