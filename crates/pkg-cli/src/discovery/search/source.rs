//! Search source groups and query prefixes.

use crate::cli::SearchOptions;

use crate::commands::CommandError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SearchSource {
    All,
    Nixpkgs,
    Formulae,
    Homebrew,
    Casks,
    Cask(String),
}

impl SearchSource {
    pub(super) fn parse(source: Option<&str>) -> Self {
        match source {
            None => Self::All,
            Some("nixpkgs") => Self::Nixpkgs,
            Some("brew" | "homebrew/core") => Self::Formulae,
            Some("homebrew") => Self::Homebrew,
            Some("cask") => Self::Casks,
            Some(source) => Self::Cask(source.to_string()),
        }
    }

    pub(super) fn includes(&self, source: &str) -> bool {
        match self {
            Self::All => true,
            Self::Nixpkgs => source == "nixpkgs",
            Self::Formulae => source == "brew",
            Self::Homebrew => source != "nixpkgs",
            Self::Casks => !matches!(source, "nixpkgs" | "brew"),
            Self::Cask(selected) => selected == source,
        }
    }

    pub(super) fn tap(&self) -> Option<&str> {
        match self {
            Self::Cask(source) if source != "homebrew/cask" => Some(source),
            _ => None,
        }
    }

    pub(super) fn needs_registry(&self) -> bool {
        matches!(self, Self::All | Self::Homebrew | Self::Casks) || self.tap().is_some()
    }

    pub(super) fn official_cask_only(&self) -> bool {
        matches!(self, Self::Cask(source) if source == "homebrew/cask")
    }

    fn label(&self) -> &str {
        match self {
            Self::All => "all sources",
            Self::Nixpkgs => "nixpkgs",
            Self::Formulae => "brew",
            Self::Homebrew => "homebrew",
            Self::Casks => "cask",
            Self::Cask(source) => source,
        }
    }
}

pub(super) fn search_input(
    options: &SearchOptions,
) -> Result<(String, SearchSource), CommandError> {
    let text = if options.matching.regex {
        options.query.as_str()
    } else {
        options.query.trim()
    };
    let explicit = SearchSource::parse(options.source.as_deref());
    let (text, implied) = match text.split_once(':').filter(|_| !options.matching.regex) {
        Some(("nixpkgs", text)) => (text, SearchSource::Nixpkgs),
        Some(("brew", text)) => (
            text.strip_prefix("homebrew/core/").unwrap_or(text),
            SearchSource::Formulae,
        ),
        Some(("homebrew", text)) => (text, SearchSource::Homebrew),
        Some(("cask", text)) => {
            let mut parts = text.splitn(3, '/');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(owner), Some(tap), Some(token)) => {
                    (token, SearchSource::Cask(format!("{owner}/{tap}")))
                }
                _ => (text, SearchSource::Casks),
            }
        }
        _ => (text, SearchSource::All),
    };
    let source = match (&implied, &explicit) {
        (SearchSource::All, _) | (SearchSource::Casks, SearchSource::Cask(_)) => explicit,
        (_, SearchSource::All) | (SearchSource::Cask(_), SearchSource::Casks) => implied,
        _ if implied == explicit => implied,
        _ => {
            eprintln!(
                "The query selects `{}`, but --source selects `{}`.",
                implied.label(),
                explicit.label()
            );
            return Err(CommandError::Reported(std::process::ExitCode::from(
                crate::commands::USAGE,
            )));
        }
    };
    Ok((text.to_string(), source))
}
