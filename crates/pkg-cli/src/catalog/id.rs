//! Catalog ID parsing, validation, and source normalization.
//!
//! Pure functions over user input: how an ID parses, which form it names,
//! and how two references collapse to one canonical source. No process
//! runs here and no source is contacted.

use crate::nix::{self, full_entry_id, split_entry_id};

use super::cask::package_attribute;
use super::routing::Routing;

/// A routed catalog identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogId {
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

impl CatalogId {
    /// The source-qualified display name.
    #[must_use]
    pub fn qualified(&self) -> String {
        match self {
            Self::Nixpkgs(attr) => format!("nixpkgs:{attr}"),
            Self::Cask { source, token } => {
                format!("cask:{}", full_entry_id(source, token))
            }
            Self::Explicit { attribute, .. } => format!("flake:{attribute}"),
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
    pub fn source_and_attribute(
        &self,
        routing: &Routing,
        system: &str,
    ) -> Result<(String, String), String> {
        match self {
            Self::Nixpkgs(attr) => Ok((routing.nixpkgs.clone(), attr.clone())),
            Self::Cask { source, token } => {
                let reference = routing.cask_reference(source)?;
                Ok((
                    reference,
                    package_attribute(system, &full_entry_id(source, token)),
                ))
            }
            Self::Explicit {
                reference,
                attribute,
            } => Ok((reference.clone(), attribute.clone())),
        }
    }

    /// The Nix installable for this identity.
    ///
    /// Installation always uses the original moving reference; locking is
    /// native behavior at install time.
    pub fn installable(&self, routing: &Routing, system: &str) -> Result<String, String> {
        let (reference, attribute) = self.source_and_attribute(routing, system)?;
        Ok(format!("{reference}#{attribute}"))
    }
}

/// How a user-supplied ID parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedId {
    /// A source-qualified or explicit identity.
    Qualified(CatalogId),
    /// A bare cask token (`cask:token`): resolved across every saved cask
    /// catalog, official and taps alike, and refused when it matches more
    /// than one source — even when some matches are excluded entries.
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
pub fn parse_id(input: &str) -> Result<ParsedId, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err(String::from("ID must not be empty"));
    }
    if let Some(attr) = input.strip_prefix("nixpkgs:") {
        validate_attr_path(attr, "nixpkgs:")?;
        return Ok(ParsedId::Qualified(CatalogId::Nixpkgs(attr.to_string())));
    }
    if let Some(rest) = input.strip_prefix("cask:") {
        if rest.contains('/') {
            return match split_entry_id(rest) {
                Some((source, token)) => Ok(ParsedId::Qualified(CatalogId::Cask { source, token })),
                None => Err(format!(
                    "`{input}` is not a valid cask entry ID; \
                     the full form is owner/tap/token, for example somebody/apps/tool"
                )),
            };
        }
        validate_cask_token(rest, "cask:")?;
        return Ok(ParsedId::BareCask(rest.to_string()));
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
    // A slash-bearing name can only be a full cask entry identity; Nixpkgs
    // attributes never contain one, so the form is refused with the rule
    // instead of becoming a bare name that quietly matches nothing.
    if input.contains('/') {
        return match split_entry_id(input) {
            Some((source, token)) => Ok(ParsedId::Qualified(CatalogId::Cask { source, token })),
            None => Err(format!(
                "`{input}` is not a valid catalog ID; \
                 a cask entry ID is owner/tap/token, for example somebody/apps/tool"
            )),
        };
    }
    Ok(ParsedId::Bare(input.to_string()))
}

/// Validate a cask token.
///
/// The rule is the generator's token identifier rule
/// (`[a-z0-9][a-z0-9+._@-]*` without `..` components): `@` versioned
/// tokens such as `1password-cli@beta` and `+` tokens such as `xournal++`
/// are real catalog identifiers and must parse, while a token can never
/// shape the attribute path it installs through.
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
/// repository are never conflated. A stable tap `current` path reference is
/// already canonical: the symlink may move between generations, and the
/// canonical identity deliberately stays the stable per-source reference.
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
        // A bare cask token stays unresolved: sources are compared later.
        assert_eq!(
            parse_id("cask:iterm2"),
            Ok(ParsedId::BareCask(String::from("iterm2")))
        );
        // Full cask identities parse with and without the `cask:` prefix.
        let full = CatalogId::Cask {
            source: String::from("somebody/apps"),
            token: String::from("tool"),
        };
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
            Ok(ParsedId::Qualified(CatalogId::Cask {
                source: String::from("homebrew/cask"),
                token: String::from("iterm2"),
            }))
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
        let official = CatalogId::Cask {
            source: String::from("homebrew/cask"),
            token: String::from("iterm2"),
        };
        assert_eq!(
            official
                .installable(&routing, "aarch64-darwin")
                .expect("routes"),
            "github:spa5k/pkg/main?dir=nix/casks#packages.aarch64-darwin.\"homebrew/cask/iterm2\""
        );
        // A token with special characters stays inside the one quoted full
        // identity segment; the identity never becomes a nested path.
        let versioned = CatalogId::Cask {
            source: String::from("somebody/apps"),
            token: String::from("firefox@beta"),
        };
        assert_eq!(
            versioned
                .installable(&routing, "x86_64-linux")
                .expect("routes"),
            "path:/state/pkg/taps/somebody/apps/current#packages.x86_64-linux.\"somebody/apps/firefox@beta\""
        );
        // A token with dots encodes inside the one quoted full identity
        // segment; the identity never becomes a nested path.
        let dotted = CatalogId::Cask {
            source: String::from("somebody/apps"),
            token: String::from("tool@1.2"),
        };
        assert_eq!(
            dotted
                .installable(&routing, "x86_64-linux")
                .expect("routes"),
            "path:/state/pkg/taps/somebody/apps/current#packages.x86_64-linux.\"somebody/apps/tool@1_d_2\""
        );
        // Other identity forms ignore the cask routing.
        let nixpkgs = CatalogId::Nixpkgs(String::from("ripgrep"));
        assert_eq!(
            nixpkgs
                .installable(&routing, "aarch64-darwin")
                .expect("routes"),
            "github:NixOS/nixpkgs/nixpkgs-unstable#ripgrep"
        );
    }

    #[test]
    fn unknown_tap_sources_name_the_add_command() {
        let routing = routing();
        let unknown = CatalogId::Cask {
            source: String::from("nobody/tools"),
            token: String::from("tool"),
        };
        let error = unknown
            .installable(&routing, "x86_64-linux")
            .expect_err("unknown sources must not route");
        assert!(error.contains("nobody/tools"), "{error}");
        assert!(error.contains("pkg tap add nobody/tools"), "{error}");
    }
}
