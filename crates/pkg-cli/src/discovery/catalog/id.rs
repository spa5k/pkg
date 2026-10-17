//! Catalog ID parsing, validation, and source normalization.
//!
//! Parse user input and normalize source references without running processes
//! or contacting sources.

use crate::nix::{self, full_entry_id, split_entry_id};

use super::cask::package_attribute;
use super::routing::Routing;

/// The kind and checked payload of a catalog identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogIdKind {
    /// An official Formula converted to a native Nix package.
    Formula(String),
    /// An attribute in the configured Nixpkgs source.
    Nixpkgs(String),
    /// A token in one cask catalog: the official `homebrew/cask` source or
    /// one locally imported tap (`owner/tap`).
    Cask {
        /// The catalog source identity, for example `homebrew/cask` or
        /// `somebody/apps`.
        source: String,
        /// The bare cask token.
        token: String,
    },
    /// An explicit public GitHub flake reference with an attribute.
    Explicit {
        /// The flake reference, for example `github:owner/repo`.
        reference: String,
        /// The attribute after `#`.
        attribute: String,
    },
}

/// A routed catalog identity whose payload passed its kind's validator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogId(CatalogIdKind);

impl CatalogId {
    /// Check and build an official Formula identity.
    /// # Errors
    /// Returns an error when `name` is not a valid Homebrew Formula name.
    pub fn formula(name: &str) -> Result<Self, String> {
        if !super::brew::valid_formula_name(name) {
            return Err(String::from("brew: needs a valid Homebrew formula name"));
        }
        Ok(Self(CatalogIdKind::Formula(name.to_string())))
    }

    /// Check and build a Nixpkgs attribute identity.
    /// # Errors
    /// Returns an error when `attribute` is not a valid attribute path.
    pub fn nixpkgs(attribute: &str) -> Result<Self, String> {
        validate_attr_path(attribute, "nixpkgs:")?;
        Ok(Self(CatalogIdKind::Nixpkgs(attribute.to_string())))
    }

    /// Check and build a cask identity.
    /// # Errors
    /// Returns an error when the source or token is invalid.
    pub fn cask(source: &str, token: &str) -> Result<Self, String> {
        if !nix::valid_catalog_source(source) {
            return Err(format!("`{source}` is not a valid cask source"));
        }
        validate_cask_token(token, "cask:")?;
        Ok(Self(CatalogIdKind::Cask {
            source: source.to_string(),
            token: token.to_string(),
        }))
    }

    /// Check and build an explicit GitHub flake identity.
    /// # Errors
    /// Returns an error when the reference is not a public GitHub reference or the attribute is invalid.
    pub fn explicit(reference: &str, attribute: &str) -> Result<Self, String> {
        let Some(rest) = reference.strip_prefix("github:") else {
            return Err(String::from("only public github: references are supported"));
        };
        validate_github_reference(rest)?;
        validate_attr_path(attribute, "`github:` installables ")?;
        Ok(Self(CatalogIdKind::Explicit {
            reference: reference.to_string(),
            attribute: attribute.to_string(),
        }))
    }

    /// Borrow the checked identity kind and its payloads.
    #[must_use]
    pub const fn kind(&self) -> &CatalogIdKind {
        &self.0
    }

    /// The source-qualified display name.
    #[must_use]
    pub fn qualified(&self) -> String {
        match self.kind() {
            CatalogIdKind::Nixpkgs(attr) => format!("nixpkgs:{attr}"),
            CatalogIdKind::Formula(name) => format!("brew:homebrew/core/{name}"),
            CatalogIdKind::Cask { source, token } => {
                format!("cask:{}", full_entry_id(source, token))
            }
            CatalogIdKind::Explicit {
                reference,
                attribute,
            } => format!("{reference}#{attribute}"),
        }
    }

    /// The moving source reference and attribute this identity names.
    ///
    /// This is the one place that knows where each identity form lives;
    /// every caller derives installables and lookups from it. Cask entries
    /// resolve to one quoted attribute segment of their catalog source:
    /// `packages.<system>."owner/tap/token@1_d_2"`, where the full id is
    /// encoded by the shared backend helper (dots become `_d_`,
    /// underscores `_u_`). The official source uses the
    /// configured casks flake; an imported tap uses its stable local
    /// `current` reference, so a native upgrade follows the tap forward
    /// while rollback keeps the previously locked outputs.
    ///
    /// An unknown tap source is an error naming `pkg tap add`, never a
    /// guess and never an attempt to evaluate the string as Nix code.
    /// # Errors
    ///
    /// Returns an error when a cask source is not present in the routing table.
    pub fn source_and_attribute(
        &self,
        routing: &Routing,
        system: &str,
    ) -> Result<(String, String), String> {
        match self.kind() {
            CatalogIdKind::Nixpkgs(attr) => Ok((routing.sources.nixpkgs.clone(), attr.clone())),
            CatalogIdKind::Formula(name) => Ok((
                routing.sources.formulae.clone(),
                package_attribute(system, &format!("homebrew/core/{name}")),
            )),
            CatalogIdKind::Cask { source, token } => {
                let reference = routing.cask_reference(source)?;
                Ok((
                    reference,
                    package_attribute(system, &full_entry_id(source, token)),
                ))
            }
            CatalogIdKind::Explicit {
                reference,
                attribute,
            } => Ok((reference.clone(), attribute.clone())),
        }
    }
}

impl std::str::FromStr for CatalogId {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match parse_id(input)? {
            ParsedId::Qualified(id) => Ok(id),
            ParsedId::Bare(_) | ParsedId::BareCask(_) => {
                Err(String::from("catalog ID must be source-qualified"))
            }
        }
    }
}

impl std::fmt::Display for CatalogId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.qualified())
    }
}

/// How a user-supplied ID parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedId {
    /// A source-qualified or explicit identity.
    Qualified(CatalogId),
    /// A bare `cask:token` resolved across official and saved tap catalogs.
    /// Reject matches in more than one source, including excluded entries.
    BareCask(String),
    /// A bare name that needs disambiguation across all sources.
    Bare(String),
}

/// Parse and validate a user-supplied ID without contacting any source.
///
/// Supported forms: `nixpkgs:attr`, `cask:token`, the full cask identity
/// `owner/tap/token` (with or without the `cask:` prefix), bare names, and
/// public GitHub flake installables `github:owner/repo[#ref]#attribute`.
/// Qualified attributes must be nonempty dot-separated identifiers; cask
/// tokens follow the generator token rule; full cask identities must be a
/// valid `owner/tap` source plus a valid bare token. Anything else that
/// looks like an explicit source is refused with a reason.
/// # Errors
///
/// Returns an error when the input is empty, malformed, or uses an invalid source-qualified ID.
pub fn parse_id(input: &str) -> Result<ParsedId, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err(String::from("ID must not be empty"));
    }
    if let Some(name) = input.strip_prefix("brew:") {
        let name = name.strip_prefix("homebrew/core/").unwrap_or(name);
        return CatalogId::formula(name).map(ParsedId::Qualified);
    }
    if let Some(attr) = input.strip_prefix("nixpkgs:") {
        return CatalogId::nixpkgs(attr).map(ParsedId::Qualified);
    }
    let cask = input.strip_prefix("cask:");
    if let Some(token) = cask
        && !token.contains('/')
    {
        validate_cask_token(token, "cask:")?;
        return Ok(ParsedId::BareCask(token.to_string()));
    }
    if let Some(rest) = input.strip_prefix("github:") {
        let Some((reference, attribute)) = rest.split_once('#') else {
            return Err(format!(
                "`{input}` needs an attribute after `#`, \
                 for example github:owner/repo#packages.aarch64-darwin.default"
            ));
        };
        return CatalogId::explicit(&format!("github:{reference}"), attribute)
            .map(ParsedId::Qualified)
            .map_err(|error| format!("`{input}`: {error}"));
    }
    if cask.is_none() && (input.contains('#') || input.contains("://")) {
        return Err(format!(
            "`{input}` is not a supported explicit source; \
             only public github: references are supported"
        ));
    }
    // A slash-bearing name can only be a full cask entry identity; Nixpkgs
    // attributes never contain one, so the form is refused with the rule
    // instead of becoming a bare name that quietly matches nothing.
    let entry = cask.unwrap_or(input);
    if entry.contains('/') {
        return match split_entry_id(entry) {
            Some((source, token)) => CatalogId::cask(source, token).map(ParsedId::Qualified),
            None => Err(format!(
                "`{input}` is not a valid cask entry ID; \
                 the full form is owner/tap/token, for example somebody/apps/tool"
            )),
        };
    }
    Ok(ParsedId::Bare(input.to_string()))
}

/// Validate a cask token.
///
/// Use the generator's rule, `[a-z0-9][a-z0-9+._@-]*`, without `..`
/// components. This accepts tokens such as `1password-cli@beta` and `xournal++`.
fn validate_cask_token(token: &str, prefix: &str) -> Result<(), String> {
    if token.is_empty() {
        return Err(format!("{prefix} needs a nonempty token"));
    }
    if !nix::valid_catalog_token(token) {
        return Err(format!(
            "`{token}` is not a valid cask token; \
             tokens may use lowercase letters, digits, `+`, `.`, `_`, `-`, and `@`"
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

fn validate_github_reference(reference: &str) -> Result<(), String> {
    let segments: Vec<&str> = reference.split('/').collect();
    if segments.len() < 2
        || segments.len() > 3
        || segments.iter().any(|segment| segment.is_empty())
        || reference.contains('#')
        || reference.chars().any(char::is_whitespace)
    {
        return Err(String::from(
            "use github:owner/repo or github:owner/repo/ref",
        ));
    }
    Ok(())
}

/// Canonical source identity of a reference, for installed matching.
///
/// References match when they name the same repository and flake subdirectory.
/// Keep `dir=` and ignore other parameters, including the reference and revision.
/// Keep a tap's stable `current` path unchanged when its symlink target changes.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Sources;

    #[test]
    fn parses_valid_ids_and_refuses_malformed_ones() {
        assert_eq!(
            parse_id("nixpkgs:ripgrep"),
            Ok(ParsedId::Qualified(
                CatalogId::nixpkgs("ripgrep").expect("valid ID")
            ))
        );
        assert_eq!(
            parse_id("nixpkgs:legacyPackages.aarch64-darwin.fd"),
            Ok(ParsedId::Qualified(
                CatalogId::nixpkgs("legacyPackages.aarch64-darwin.fd").expect("valid ID"),
            ))
        );
        // A bare cask token stays unresolved: sources are compared later.
        assert_eq!(
            parse_id("cask:iterm2"),
            Ok(ParsedId::BareCask(String::from("iterm2")))
        );
        // Full cask identities parse with and without the `cask:` prefix.
        let full = CatalogId::cask("somebody/apps", "tool").expect("valid ID");
        assert_eq!(
            parse_id("cask:somebody/apps/tool"),
            Ok(ParsedId::Qualified(full.clone()))
        );
        assert_eq!(
            parse_id("somebody/apps/tool"),
            Ok(ParsedId::Qualified(full))
        );
        assert_eq!(
            parse_id("cask:homebrew/cask/iterm2"),
            Ok(ParsedId::Qualified(
                CatalogId::cask("homebrew/cask", "iterm2").expect("valid ID"),
            ))
        );
        assert_eq!(
            parse_id("github:hraban/mac-app-util#default"),
            Ok(ParsedId::Qualified(
                CatalogId::explicit("github:hraban/mac-app-util", "default").expect("valid ID"),
            ))
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
            "cask:somebody/apps",
            "cask:somebody/apps/tool/extra",
            "somebody/apps",
            "somebody/apps/a..b",
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
    fn checked_ids_reject_invalid_payloads_and_round_trip_qualified_forms() {
        assert!(CatalogId::formula("bad/name").is_err());
        assert!(CatalogId::nixpkgs("packages..fd").is_err());
        assert!(CatalogId::cask("bad/source/extra", "tool").is_err());
        assert!(CatalogId::cask("somebody/apps", "Tool").is_err());
        assert!(CatalogId::explicit("github:owner/repo", "packages..fd").is_err());
        assert!(CatalogId::explicit("https://example.com/repo", "default").is_err());
        assert!(CatalogId::explicit("github:owner/repo#rev", "default").is_err());
        assert!(CatalogId::explicit("github:owner /repo", "default").is_err());

        for text in [
            "brew:homebrew/core/ripgrep",
            "nixpkgs:legacyPackages.aarch64-darwin.fd",
            "cask:somebody/apps/tool@beta",
            "github:owner/repo/rev#packages.aarch64-darwin.default",
        ] {
            let id: CatalogId = text.parse().expect("valid qualified ID");
            assert_eq!(id.to_string(), text);
            assert_eq!(id.qualified().parse::<CatalogId>().expect("round trip"), id);
        }
        assert!("ripgrep".parse::<CatalogId>().is_err());
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
    fn cask_tokens_follow_the_generated_identifier_rule() {
        // `@` and `+` tokens are real catalog identifiers and must parse.
        assert_eq!(
            parse_id("cask:1password-cli@beta"),
            Ok(ParsedId::BareCask(String::from("1password-cli@beta")))
        );
        assert_eq!(
            parse_id("cask:xournal++"),
            Ok(ParsedId::BareCask(String::from("xournal++")))
        );
        // The rule is lowercase: display-case input is refused, and a
        // token can never smuggle a nested path.
        assert!(parse_id("cask:Iterm2").is_err());
        assert!(parse_id("cask:a..b").is_err());
        assert!(parse_id("cask:to ken").is_err());
        assert!(parse_id("cask:").is_err());
    }

    fn routing() -> Routing {
        Routing::new(
            &Sources::default(),
            [(
                String::from("somebody/apps"),
                String::from("path:/state/pkg/taps/somebody/apps/current"),
            )]
            .into_iter()
            .collect(),
        )
    }

    #[test]
    fn cask_identities_install_through_one_quoted_attribute() {
        let routing = routing();
        let official = CatalogId::cask("homebrew/cask", "iterm2").expect("valid ID");
        assert_eq!(
            official
                .source_and_attribute(&routing, "aarch64-darwin")
                .expect("routes"),
            (
                String::from("github:spa5k/pkg/main?dir=nix/casks"),
                String::from("packages.aarch64-darwin.\"homebrew/cask/iterm2\"")
            )
        );
        // A token with special characters stays inside the one quoted full
        // identity segment; the identity never becomes a nested path.
        let versioned = CatalogId::cask("somebody/apps", "firefox@beta").expect("valid ID");
        assert_eq!(
            versioned
                .source_and_attribute(&routing, "x86_64-linux")
                .expect("routes"),
            (
                String::from("path:/state/pkg/taps/somebody/apps/current"),
                String::from("packages.x86_64-linux.\"somebody/apps/firefox@beta\"")
            )
        );
        // A token with dots encodes inside the one quoted full identity
        // segment; the identity never becomes a nested path.
        let dotted = CatalogId::cask("somebody/apps", "tool@1.2").expect("valid ID");
        assert_eq!(
            dotted
                .source_and_attribute(&routing, "x86_64-linux")
                .expect("routes"),
            (
                String::from("path:/state/pkg/taps/somebody/apps/current"),
                String::from("packages.x86_64-linux.\"somebody/apps/tool@1_d_2\"")
            )
        );
        // Other identity forms ignore the cask routing.
        let nixpkgs = CatalogId::nixpkgs("ripgrep").expect("valid ID");
        assert_eq!(
            nixpkgs
                .source_and_attribute(&routing, "aarch64-darwin")
                .expect("routes"),
            (
                String::from("github:NixOS/nixpkgs/nixpkgs-unstable"),
                String::from("ripgrep")
            )
        );
    }

    #[test]
    fn unknown_tap_sources_name_the_add_command() {
        let routing = routing();
        let unknown = CatalogId::cask("nobody/tools", "tool").expect("valid ID");
        let error = unknown
            .source_and_attribute(&routing, "x86_64-linux")
            .expect_err("unknown sources must not route");
        assert!(error.contains("nobody/tools"), "{error}");
        assert!(error.contains("pkg tap add nobody/tools"), "{error}");
    }
}
