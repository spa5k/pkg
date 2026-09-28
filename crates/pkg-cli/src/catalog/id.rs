//! Catalog ID parsing, validation, and source normalization.
//!
//! Pure functions over user input: how an ID parses, which form it names,
//! and how two references collapse to one canonical source. No process
//! runs here and no source is contacted.

use crate::config::Sources;

/// A routed catalog identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogId {
    /// An attribute in the configured Nixpkgs source.
    Nixpkgs(String),
    /// A token in the configured Cask product flake.
    Cask(String),
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
            Self::Cask(token) => format!("cask:{token}"),
            Self::Explicit { attribute, .. } => format!("flake:{attribute}"),
        }
    }

    /// The moving source reference and attribute this identity names.
    ///
    /// This is the one place that knows where each identity form lives;
    /// every caller derives installables and lookups from it.
    #[must_use]
    pub fn source_and_attribute(&self, sources: &Sources) -> (String, String) {
        match self {
            Self::Nixpkgs(attr) => (sources.nixpkgs.clone(), attr.clone()),
            Self::Cask(token) => (sources.casks.clone(), token.clone()),
            Self::Explicit {
                reference,
                attribute,
            } => (reference.clone(), attribute.clone()),
        }
    }

    /// The Nix installable for this identity.
    ///
    /// Installation always uses the original moving reference; locking is
    /// native behavior at install time.
    #[must_use]
    pub fn installable(&self, sources: &Sources) -> String {
        let (reference, attribute) = self.source_and_attribute(sources);
        format!("{reference}#{attribute}")
    }
}

/// How a user-supplied ID parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedId {
    /// A source-qualified or explicit identity.
    Qualified(CatalogId),
    /// A bare name that needs disambiguation.
    Bare(String),
}

/// Parse and validate a user-supplied ID without contacting any source.
///
/// Supported forms: `nixpkgs:attr`, `cask:token`, bare names, and public
/// GitHub flake installables `github:owner/repo[#ref]#attribute`. Qualified
/// attributes must be nonempty dot-separated identifiers; explicit GitHub
/// forms need a nonempty owner and repository and no arbitrary path forms.
/// Anything else that looks like an explicit source is refused with a reason.
pub fn parse_id(input: &str) -> Result<ParsedId, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err(String::from("ID must not be empty"));
    }
    if let Some(attr) = input.strip_prefix("nixpkgs:") {
        validate_attr_path(attr, "nixpkgs:")?;
        return Ok(ParsedId::Qualified(CatalogId::Nixpkgs(attr.to_string())));
    }
    if let Some(token) = input.strip_prefix("cask:") {
        validate_cask_token(token, "cask:")?;
        return Ok(ParsedId::Qualified(CatalogId::Cask(token.to_string())));
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
    Ok(ParsedId::Bare(input.to_string()))
}

/// Validate a cask token.
///
/// Homebrew tokens may also use `@` (versioned tokens such as
/// `1password@nightly`) and `.`; refusing them would hide the recorded
/// exclusion reason for exactly those tokens.
fn validate_cask_token(token: &str, prefix: &str) -> Result<(), String> {
    if token.is_empty() {
        return Err(format!("{prefix} needs a nonempty token"));
    }
    if !token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '@' | '.'))
    {
        return Err(format!(
            "`{token}` is not a valid cask token; \
             tokens may use letters, digits, `_`, `-`, `@`, and `.`"
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
/// repository are never conflated.
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
        assert_eq!(
            parse_id("cask:iterm2"),
            Ok(ParsedId::Qualified(CatalogId::Cask(String::from("iterm2"))))
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
    fn cask_tokens_may_carry_known_extra_characters() {
        // `@` tokens must parse so their exclusion reason can be shown.
        assert_eq!(
            parse_id("cask:1password@nightly"),
            Ok(ParsedId::Qualified(CatalogId::Cask(String::from(
                "1password@nightly"
            ))))
        );
        assert!(parse_id("cask:to ken").is_err());
        assert!(parse_id("cask:").is_err());
    }
}
