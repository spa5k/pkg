//! Package discovery, source resolution, and disposable search data.

pub mod catalog;
mod info;
mod refresh;
mod resolution;
mod search;

pub(crate) use info::run as info;
pub(crate) use refresh::refresh;
pub(crate) use resolution::{gate_upgrade, resolve_install};
pub(crate) use search::run as search;

use crate::config::Config;

/// The configured sources in query order: Nixpkgs first, then casks.
fn configured_sources(config: &Config) -> [(catalog::SourceKind, &str); 2] {
    [
        (
            catalog::SourceKind::Nixpkgs,
            config.sources.nixpkgs.as_str(),
        ),
        (catalog::SourceKind::Cask, config.sources.casks.as_str()),
    ]
}
