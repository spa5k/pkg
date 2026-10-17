//! Relevance and visibility rules shared by every local source.

use super::{SearchResult, exposed_attribute};

/// The first relevance band reserved for approximate names.
pub const FUZZY_RANK: u32 = 1 << 24;

/// A validated discovery query with conservative name matching.
pub struct SearchQuery {
    pattern: Option<regex::Regex>,
    literal: Option<String>,
    terms: Vec<String>,
    broad: bool,
    fuzzy: bool,
    /// Include unavailable matches in addition to exact unavailable names.
    pub unavailable: bool,
}

impl SearchQuery {
    /// Compile explicit regex input before reading sources.
    /// # Errors
    ///
    /// Returns an error when the literal query is empty or the regular expression is invalid.
    pub fn new(text: &str, regex: bool, unavailable: bool) -> Result<Self, regex::Error> {
        let text = if regex { text } else { text.trim() };
        if !regex && text.is_empty() {
            return Err(regex::Error::Syntax(String::from(
                "enter a package name or search words",
            )));
        }
        let pattern = if regex {
            Some(
                regex::RegexBuilder::new(text)
                    .case_insensitive(true)
                    .build()?,
            )
        } else {
            None
        };
        let literal = (!regex).then(|| text.to_lowercase());
        let terms = literal
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        Ok(Self {
            pattern,
            literal,
            terms,
            broad: false,
            fuzzy: true,
            unavailable,
        })
    }

    /// Include nested development packages, variants, and substring matches.
    #[must_use]
    pub fn broad(mut self, enabled: bool) -> Self {
        self.broad = enabled;
        self
    }

    /// Enable name-only typo alternatives when literal results are absent.
    #[must_use]
    pub fn fuzzy(mut self, enabled: bool) -> Self {
        self.fuzzy = enabled;
        self
    }

    pub(super) fn exact(&self, values: &[&str]) -> bool {
        self.literal
            .as_ref()
            .is_some_and(|query| values.iter().any(|value| value.to_lowercase() == *query))
    }

    pub(super) fn includes_hidden(&self) -> bool {
        self.broad || self.pattern.is_some()
    }

    pub(super) fn library_query(&self) -> bool {
        self.terms.iter().any(|term| {
            matches!(
                term.as_str(),
                "lib"
                    | "libs"
                    | "library"
                    | "libraries"
                    | "sdk"
                    | "binding"
                    | "bindings"
                    | "headers"
                    | "c"
                    | "c++"
                    | "font"
                    | "fonts"
                    | "data"
                    | "theme"
                    | "icons"
                    | "plugin"
                    | "plugins"
                    | "extension"
            )
        })
    }

    /// Match one row and return its relevance order, smaller first.
    #[must_use]
    pub fn rank(&self, row: &SearchResult) -> Option<u32> {
        if let Some(pattern) = &self.pattern {
            return [&row.id, &row.attribute, &row.name, &row.description]
                .iter()
                .any(|value| pattern.is_match(value))
                .then_some(0);
        }
        let identity = row
            .id
            .split_once(':')
            .map_or(row.id.as_str(), |(_, name)| name);
        let name = if row.source == "nixpkgs" {
            exposed_attribute(&row.attribute, &row.system)
        } else {
            identity.rsplit('/').next().unwrap_or(identity)
        };
        let description = row.description.to_lowercase();
        if !self.visible(row, name, identity, &description) {
            return None;
        }
        if self.exact(&[&row.id, identity, name]) {
            return Some(0);
        }
        let names = [name.to_lowercase(), row.name.to_lowercase()];
        if self.exact(&[&row.name]) {
            return Some(100_000 + preference(row, &names[0], &description, &self.terms));
        }
        let mut worst = 0;
        let mut sum = 0;
        for term in &self.terms {
            let tier = names
                .iter()
                .filter_map(|name| match_text(name, term, true, self.broad))
                .min()
                .or_else(|| match_text(&description, term, false, self.broad));
            let Some(mut tier) = tier else {
                return self.typo_rank(&names);
            };
            if tier == 1
                && term.chars().count() <= 3
                && !names.iter().any(|name| {
                    name.strip_prefix(term).is_some_and(|rest| {
                        rest.chars()
                            .next()
                            .is_none_or(|character| !character.is_alphanumeric())
                    })
                })
                && match_text(&description, term, false, self.broad).is_none()
            {
                tier = 3;
            }
            worst = worst.max(tier);
            sum += tier;
        }
        let length = u32::try_from(names.iter().map(String::len).min().unwrap_or(0))
            .unwrap_or(u32::MAX)
            .min(1023);
        Some(
            worst * 1_000_000
                + preference(row, &names[0], &description, &self.terms)
                + sum.min(99) * 1024
                + length,
        )
    }

    fn visible(&self, row: &SearchResult, name: &str, identity: &str, description: &str) -> bool {
        if self.broad || self.exact(&[&row.id, identity, name]) {
            return true;
        }
        let query = self.literal.as_deref().unwrap_or("");
        if row.source == "nixpkgs" && development_namespace(name) && !query.contains('.') {
            return false;
        }
        if matches!(row.source.as_str(), "nixpkgs" | "brew")
            && declared_library(description)
            && !self.library_query()
        {
            return false;
        }
        if name.contains('@') && !query.contains('@') {
            return false;
        }
        let lower = name.to_lowercase();
        ![
            "-unwrapped",
            "_unwrapped",
            "-debug",
            "-dev",
            "-doc",
            "-man",
            "-tests",
            "-test",
        ]
        .iter()
        .any(|suffix| lower.ends_with(suffix) && !query.contains(suffix))
    }

    fn typo_rank(&self, names: &[String]) -> Option<u32> {
        if !self.fuzzy || self.terms.len() != 1 {
            return None;
        }
        let term = &self.terms[0];
        let length = term.chars().count();
        if !(4..=48).contains(&length) || !term.chars().all(char::is_alphanumeric) {
            return None;
        }
        let max = if length >= 8 { 2 } else { 1 };
        names
            .iter()
            .flat_map(|name| std::iter::once(name.as_str()).chain(words(name)))
            .filter_map(|name| edit_distance(term, name, max))
            .min()
            .map(|distance| FUZZY_RANK + distance * 1024)
    }
}

fn preference(row: &SearchResult, name: &str, description: &str, terms: &[String]) -> u32 {
    if terms.len() == 1 {
        let query = &terms[0];
        if let Some(rest) = name.strip_prefix(query) {
            let family = rest
                .trim_start_matches(['-', '_'])
                .trim_end_matches(|character: char| character.is_ascii_digit());
            if matches!(family, "cli" | "client") {
                let major = row
                    .version
                    .split('.')
                    .next()
                    .and_then(|part| part.parse::<u32>().ok())
                    .unwrap_or(0);
                return (255 - major.min(255)) * 32;
            }
        }
    }
    if words(description).any(|word| {
        matches!(word, "library" | "bindings" | "sdk") && !terms.iter().any(|term| term == word)
    }) {
        return 300_000;
    }
    if terms
        .iter()
        .all(|term| match_text(name, term, true, false).is_some())
    {
        100_000
    } else {
        200_000
    }
}

fn development_namespace(attribute: &str) -> bool {
    let Some((namespace, _)) = attribute.split_once('.') else {
        return false;
    };
    let namespace = namespace.to_ascii_lowercase();
    [
        "python",
        "pypy",
        "haskell",
        "ocaml",
        "perl",
        "lua",
        "ruby",
        "nodepackages",
        "lisp",
        "beam",
        "erlang",
    ]
    .iter()
    .any(|prefix| namespace.starts_with(prefix))
        || namespace.ends_with("dependencies")
        || namespace.ends_with("packagesets")
        || matches!(
            namespace.as_str(),
            "rpackages" | "rustpackages" | "emacspackages" | "vimplugins" | "neovimplugins"
        )
}

fn declared_library(description: &str) -> bool {
    let library = words(description)
        .any(|word| matches!(word, "library" | "libraries" | "bindings" | "headers"))
        || ["c99 implementation", "c++ wrapper", "c++ interface"]
            .iter()
            .any(|prefix| description.starts_with(prefix))
        || description.contains("abstraction layer");
    library
        && !words(description).any(|word| {
            matches!(
                word,
                "tool"
                    | "tools"
                    | "cli"
                    | "utility"
                    | "compiler"
                    | "browser"
                    | "editor"
                    | "application"
                    | "program"
            )
        })
        && !description.contains("command line")
        && !description.contains("command-line")
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
}

fn match_text(text: &str, term: &str, name: bool, broad: bool) -> Option<u32> {
    if name && text.starts_with(term) {
        return Some(1);
    }
    let length = term.chars().count();
    if text.match_indices(term).any(|(start, matched)| {
        let before = text[..start].chars().next_back();
        let after = text[start + matched.len()..].chars().next();
        before.is_none_or(|character| !character.is_alphanumeric())
            && (length >= 4 || after.is_none_or(|character| !character.is_alphanumeric()))
    }) {
        return Some(if name { 2 } else { 4 });
    }
    if (broad || name && length >= 4) && text.contains(term) {
        return Some(if name { 3 } else { 5 });
    }
    None
}

fn edit_distance(left: &str, right: &str, max: u32) -> Option<u32> {
    let right_len = right.chars().take(51).count();
    if right_len > 50 || left.chars().count().abs_diff(right_len) > usize::try_from(max).ok()? {
        return None;
    }
    u32::try_from(strsim::osa_distance(left, right))
        .ok()
        .filter(|distance| *distance <= max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, name: &str, description: &str) -> SearchResult {
        let source = id.split_once(':').unwrap().0;
        SearchResult {
            id: id.to_string(),
            attribute: format!(
                "legacyPackages.x86_64-linux.{}",
                id.split_once(':').unwrap().1
            ),
            name: name.to_string(),
            version: String::new(),
            description: description.to_string(),
            source: source.to_string(),
            reference: None,
            revision: None,
            system: String::from("x86_64-linux"),
            stale: false,
            support: None,
            install_command: None,
        }
    }

    #[test]
    fn typo_distance_keeps_unicode_transpositions_and_input_bounds() {
        assert_eq!(edit_distance("éab", "aéb", 1), Some(1));
        assert_eq!(edit_distance("éab", "aéb", 0), None);
        assert_eq!(edit_distance("ab", "bca", 2), None);
        assert_eq!(edit_distance("ab", "bca", 3), Some(3));
        assert_eq!(edit_distance(&"é".repeat(50), &"é".repeat(50), 0), Some(0));
        assert_eq!(edit_distance(&"é".repeat(51), &"é".repeat(51), 0), None);
    }

    #[test]
    fn aws_tools_precede_related_apps_and_short_substring_noise_is_hidden() {
        let query = SearchQuery::new("aws", false, false).unwrap();
        let mut rows = [
            row("nixpkgs:claws-mail", "claws-mail", "Email client"),
            row(
                "nixpkgs:python312Packages.awscli",
                "awscli",
                "AWS command line interface",
            ),
            row(
                "nixpkgs:awscli2",
                "awscli2",
                "Universal Command Line Interface for AWS",
            ),
            row(
                "cask:homebrew/cask/aws-vault-binary",
                "AWS Vault",
                "Store AWS credentials",
            ),
            row(
                "cask:homebrew/cask/corretto@11",
                "AWS Corretto JDK",
                "Java runtime",
            ),
            row("nixpkgs:draw-tool", "draw-tool", "Draws diagrams"),
        ]
        .into_iter()
        .filter_map(|row| query.rank(&row).map(|rank| (rank, row.id)))
        .collect::<Vec<_>>();
        rows.sort();
        assert_eq!(
            rows.iter().map(|(_, id)| id.as_str()).collect::<Vec<_>>(),
            ["nixpkgs:awscli2", "cask:homebrew/cask/aws-vault-binary"]
        );
        let mut old = row("nixpkgs:awscli", "awscli", "Unified AWS tool");
        old.version = String::from("1.44.21");
        let mut current = row("nixpkgs:awscli2", "awscli2", "Unified AWS tool");
        current.version = String::from("2.35.11");
        assert!(query.rank(&current).unwrap() < query.rank(&old).unwrap());
        assert!(
            query.rank(&current).unwrap()
                < query
                    .rank(&row("nixpkgs:awsls", "awsls", "List AWS resources"))
                    .unwrap()
        );
        assert!(
            query
                .rank(&row(
                    "cask:homebrew/cask/aws-vault-binary",
                    "AWS Vault",
                    "Store AWS credentials"
                ))
                .unwrap()
                < query
                    .rank(&row(
                        "nixpkgs:awstats",
                        "awstats",
                        "Real-time logfile analyzer"
                    ))
                    .unwrap()
        );
        let broad = SearchQuery::new("aws", false, false).unwrap().broad(true);
        assert!(
            broad
                .rank(&row("nixpkgs:claws-mail", "claws-mail", "Email client"))
                .is_some()
        );
        assert!(
            broad
                .rank(&row(
                    "nixpkgs:python312Packages.awscli",
                    "awscli",
                    "AWS tool"
                ))
                .is_some()
        );
    }

    #[test]
    fn short_library_matches_are_hidden_unless_query_is_exact_or_broad() {
        let query = SearchQuery::new("aws", false, false).unwrap();
        let broad = SearchQuery::new("aws", false, false).unwrap().broad(true);
        for (id, description) in [
            ("aws-lc", "Cryptographic library maintained by AWS"),
            ("aws-c-http", "C99 implementation of HTTP"),
            ("aws-crt-cpp", "C++ wrapper around AWS libraries"),
            ("aws-c-cal", "AWS Crypto Abstraction Layer"),
            ("aws-sdk-cpp", "C++ interface for Amazon Web Services"),
            (
                "nixDependencies.aws-sdk-cpp",
                "C++ interface for Amazon Web Services",
            ),
        ] {
            let package = row(&format!("nixpkgs:{id}"), id, description);
            assert!(query.rank(&package).is_none(), "{id}");
            assert!(broad.rank(&package).is_some(), "{id}");
            assert_eq!(
                SearchQuery::new(id, false, false).unwrap().rank(&package),
                Some(0)
            );
        }
        assert!(
            SearchQuery::new("aws sdk", false, false)
                .unwrap()
                .rank(&row(
                    "nixpkgs:aws-sdk-cpp",
                    "aws-sdk-cpp",
                    "C++ interface for Amazon Web Services"
                ))
                .is_some()
        );
    }

    #[test]
    fn explicit_names_reach_hidden_packages_and_all_query_words_must_match() {
        for id in ["python312Packages.awscli", "awscli2-unwrapped"] {
            let query = SearchQuery::new(id, false, false).unwrap();
            assert_eq!(
                query.rank(&row(&format!("nixpkgs:{id}"), "awscli", "AWS tool")),
                Some(0)
            );
        }
        let query = SearchQuery::new("aws credentials", false, false).unwrap();
        assert!(
            query
                .rank(&row(
                    "cask:acme/tools/vault",
                    "Vault",
                    "Store AWS credentials"
                ))
                .is_some()
        );
        assert!(
            query
                .rank(&row(
                    "nixpkgs:awscli2",
                    "awscli2",
                    "AWS command line interface"
                ))
                .is_none()
        );
        let query = SearchQuery::new("iterm2", false, false).unwrap();
        assert_eq!(
            query.rank(&row("cask:homebrew/cask/iterm2", "iTerm2", "Terminal")),
            Some(0)
        );
        let query = SearchQuery::new("terminal", false, false).unwrap();
        assert!(
            query
                .rank(&row(
                    "nixpkgs:kdePackages.konsole",
                    "konsole",
                    "Terminal emulator"
                ))
                .is_some()
        );
        let query = SearchQuery::new("c++", false, false).unwrap();
        assert!(
            query
                .rank(&row("nixpkgs:gcc", "gcc", "C and C++ compiler"))
                .is_some()
        );
        let query = SearchQuery::new(".net", false, false).unwrap();
        assert!(
            query
                .rank(&row(
                    "nixpkgs:dotnet-sdk",
                    "dotnet-sdk",
                    ".NET development kit"
                ))
                .is_some()
        );
    }

    #[test]
    fn typos_use_names_only_and_short_queries_never_use_fuzzy_matching() {
        let query = SearchQuery::new("ripgrpe", false, false).unwrap();
        assert!(
            query
                .rank(&row("nixpkgs:ripgrep", "ripgrep", "Search tool"))
                .unwrap()
                >= FUZZY_RANK
        );
        assert!(
            query
                .rank(&row("nixpkgs:unrelated", "unrelated", "Uses ripgrep"))
                .is_none()
        );
        assert!(
            query
                .fuzzy(false)
                .rank(&row("nixpkgs:ripgrep", "ripgrep", "Search tool"))
                .is_none()
        );
        assert!(
            SearchQuery::new("aws", false, false)
                .unwrap()
                .rank(&row("nixpkgs:abs", "abs", "Tool"))
                .is_none()
        );
        assert!(
            SearchQuery::new("[", false, false)
                .unwrap()
                .rank(&row("nixpkgs:ripgrep", "ripgrep", "Search tool"))
                .is_none()
        );
    }
}
