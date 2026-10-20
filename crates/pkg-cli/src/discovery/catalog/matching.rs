//! Relevance and visibility rules shared by every local source.

use std::borrow::Cow;

use super::{SearchResult, exposed_attribute};

/// The first relevance band reserved for approximate names.
pub const FUZZY_RANK: u32 = 1 << 24;

/// Borrowed fields needed to match a package before creating an output row.
pub(super) struct SearchFields<'a> {
    pub id: &'a str,
    pub attribute: &'a str,
    pub name: &'a str,
    pub version: &'a str,
    pub description: &'a str,
    pub source: &'a str,
    pub system: &'a str,
    /// The main command, when the source declares one.
    pub command: Option<&'a str>,
}

/// Rank added to nested Nixpkgs package sets, so a top-level package of the
/// same match quality comes first.
const NESTED_RANK: u32 = 450_000;

/// The best rank for a match through a Formula alias. It comes after every
/// exact name and every name that starts with the query, so `postgresql`
/// lists Nixpkgs `postgresql` before `postgresql@17`, and `python` lists
/// `python3` before `python@3.15`.
pub(super) const ALIAS_RANK: u32 = 1_400_000;

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
        self.literal.as_ref().is_some_and(|query| {
            values.iter().any(|value| {
                if value.is_ascii() {
                    value.eq_ignore_ascii_case(query)
                } else {
                    value.to_lowercase() == *query
                }
            })
        })
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
        self.rank_fields(&SearchFields {
            id: &row.id,
            attribute: &row.attribute,
            name: &row.name,
            version: &row.version,
            description: &row.description,
            source: &row.source,
            system: &row.system,
            command: row.command.as_deref(),
        })
    }

    /// Whether normal search shows a row of this kind at all.
    ///
    /// Hidden rows are internal package sets, development variants, and
    /// libraries. The query words do not matter here.
    pub(super) fn visible(&self, row: &SearchFields<'_>) -> bool {
        let name = row_name(row);
        self.visible_name(row.source, name)
            && (self.broad
                || !matches!(row.source, "nixpkgs" | "brew")
                || self.library_query()
                || !declared_library(&lowercase(row.description)))
    }

    pub(super) fn rank_fields(&self, row: &SearchFields<'_>) -> Option<u32> {
        if let Some(pattern) = &self.pattern {
            return [row.id, row.attribute, row.name, row.description]
                .iter()
                .any(|value| pattern.is_match(value))
                .then_some(0);
        }
        let identity = row.id.split_once(':').map_or(row.id, |(_, name)| name);
        let name = row_name(row);
        if self.exact(&[row.id, identity, name]) {
            return Some(0);
        }
        let query = self.literal.as_deref().unwrap_or("");
        let nested = row.source == "nixpkgs" && name.contains('.') && !query.contains('.');
        let penalty = if nested { NESTED_RANK } else { 0 };
        if !self.visible_name(row.source, name) {
            return None;
        }
        let description = lowercase(row.description);
        if !self.broad
            && matches!(row.source, "nixpkgs" | "brew")
            && !self.library_query()
            && declared_library(&description)
        {
            return None;
        }
        let names = [lowercase(name), lowercase(row.name)];
        // An exact package name or main command ranks just after exact IDs.
        // A nested package set does not get this rank: `python` must not
        // put a bootstrap or TeX package before `python3`.
        if !nested
            && (self.exact(&[row.name])
                || row.command.is_some_and(|command| self.exact(&[command])))
        {
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
        let length = u32::try_from(names.iter().map(|name| name.len()).min().unwrap_or(0))
            .unwrap_or(u32::MAX)
            .min(1023);
        Some(
            worst * 1_000_000
                + preference(row, &names[0], &description, &self.terms)
                + sum.min(99) * 1024
                + length
                + penalty,
        )
    }

    fn visible_name(&self, source: &str, name: &str) -> bool {
        if self.broad {
            return true;
        }
        let query = self.literal.as_deref().unwrap_or("");
        if source == "nixpkgs" && development_namespace(name) && !query.contains('.') {
            return false;
        }
        if name.contains('@') && !query.contains('@') {
            return false;
        }
        let lower = lowercase(name);
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

    fn typo_rank(&self, names: &[Cow<'_, str>]) -> Option<u32> {
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
            .enumerate()
            .filter(|(index, name)| !names[..*index].iter().any(|previous| previous == *name))
            .flat_map(|(_, name)| {
                std::iter::once(name.as_ref())
                    .chain(words(name).filter(move |word| *word != name.as_ref()))
            })
            .filter_map(|name| edit_distance(term, name, max))
            .min()
            .map(|distance| FUZZY_RANK + distance * 1024)
    }
}

fn lowercase(text: &str) -> Cow<'_, str> {
    if text
        .bytes()
        .all(|byte| byte.is_ascii() && !byte.is_ascii_uppercase())
    {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(text.to_lowercase())
    }
}

fn preference(row: &SearchFields<'_>, name: &str, description: &str, terms: &[String]) -> u32 {
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

/// The short name of a row: the exposed Nixpkgs attribute, or the token.
fn row_name<'a>(row: &SearchFields<'a>) -> &'a str {
    let identity = row.id.split_once(':').map_or(row.id, |(_, name)| name);
    if row.source == "nixpkgs" {
        exposed_attribute(row.attribute, row.system)
    } else {
        identity.rsplit('/').next().unwrap_or(identity)
    }
}

/// Nested Nixpkgs package sets that normal search and bare names skip.
///
/// These sets hold language packages, plugins, editor extensions, tests,
/// bootstrap tools, and packages for other programs. Application sets,
/// such as `kdePackages`, stay visible.
fn development_namespace(attribute: &str) -> bool {
    let Some((namespace, _)) = attribute.split_once('.') else {
        return false;
    };
    let namespace = namespace.to_ascii_lowercase();
    if (namespace.ends_with("packages")
        && !matches!(namespace.as_str(), "kdepackages" | "plasma5packages"))
        || ["plugins", "extensions", "grammars", "modules", "scripts"]
            .iter()
            .any(|suffix| namespace.ends_with(suffix))
        || namespace.contains("bootstrap")
        || namespace.starts_with("pkgs")
        || matches!(
            namespace.as_str(),
            "tests" | "darwin" | "texlive" | "nur" | "linuxkernel" | "windows" | "androidenv"
        )
    {
        return true;
    }
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
    let distance = if left.is_ascii() && right.is_ascii() {
        let mut rows = [[0_usize; 51]; 3];
        let [mut previous_two, mut previous, mut current] = rows.each_mut();
        for column in 0..=right_len {
            previous_two[column] = column;
            previous[column] = column;
        }
        let left = left.as_bytes();
        let right = right.as_bytes();
        for (row, &left_byte) in left.iter().enumerate() {
            current[0] = row + 1;
            for (column, &right_byte) in right.iter().enumerate() {
                current[column + 1] = (current[column] + 1)
                    .min(previous[column + 1] + 1)
                    .min(previous[column] + usize::from(left_byte != right_byte));
                if row > 0
                    && column > 0
                    && left_byte != right_byte
                    && left_byte == right[column - 1]
                    && right_byte == left[row - 1]
                {
                    current[column + 1] = current[column + 1].min(previous_two[column - 1] + 1);
                }
            }
            (previous_two, previous, current) = (previous, current, previous_two);
        }
        previous[right_len]
    } else {
        strsim::osa_distance(left, right)
    };
    u32::try_from(distance)
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
            command: None,
        }
    }

    #[test]
    fn nested_package_sets_follow_top_level_packages() {
        let query = SearchQuery::new("python", false, false).unwrap();
        let python3 = row("nixpkgs:python3", "python3", "High-level language");
        let bootstrap = row(
            "nixpkgs:minimal-bootstrap.python",
            "python",
            "High-level language",
        );
        let tex = row("nixpkgs:texlivePackages.python", "python", "Embed Python");
        let buildbot = row(
            "nixpkgs:buildbotPackages.python",
            "python",
            "High-level language",
        );
        assert!(query.rank(&python3).is_some());
        assert_eq!(query.rank(&bootstrap), None);
        assert_eq!(query.rank(&tex), None);
        assert_eq!(query.rank(&buildbot), None);
        let broad = SearchQuery::new("python", false, false)
            .unwrap()
            .broad(true);
        assert!(broad.rank(&python3) < broad.rank(&bootstrap));
        // Application sets stay visible.
        let kate = row("nixpkgs:kdePackages.kate", "kate", "Text editor");
        assert!(
            SearchQuery::new("kate", false, false)
                .unwrap()
                .rank(&kate)
                .is_some()
        );
    }

    #[test]
    fn a_main_command_ranks_like_an_exact_name() {
        let query = SearchQuery::new("rg", false, false).unwrap();
        let mut ripgrep = row("nixpkgs:ripgrep", "ripgrep", "Line-oriented search tool");
        assert_eq!(query.rank(&ripgrep), None);
        ripgrep.command = Some(String::from("rg"));
        let prefix = row("nixpkgs:rgx", "rgx", "Regex tester");
        assert!(query.rank(&ripgrep) < query.rank(&prefix));
    }

    #[test]
    fn typo_distance_keeps_unicode_transpositions_and_input_bounds() {
        assert_eq!(edit_distance("éab", "aéb", 1), Some(1));
        assert_eq!(edit_distance("éab", "aéb", 0), None);
        assert_eq!(edit_distance("ab", "bca", 2), None);
        assert_eq!(edit_distance("ab", "bca", 3), Some(3));
        assert_eq!(edit_distance(&"a".repeat(50), &"a".repeat(50), 0), Some(0));
        assert_eq!(edit_distance(&"a".repeat(51), &"a".repeat(51), 0), None);
        assert_eq!(edit_distance(&"é".repeat(50), &"é".repeat(50), 0), Some(0));
        assert_eq!(edit_distance(&"é".repeat(51), &"é".repeat(51), 0), None);
    }

    #[test]
    fn ascii_typo_distance_matches_the_reference_for_short_strings() {
        let mut values = vec![String::new()];
        let mut start = 0;
        for _ in 0..5 {
            let end = values.len();
            for index in start..end {
                for character in ['a', 'b', 'c'] {
                    let mut value = values[index].clone();
                    value.push(character);
                    values.push(value);
                }
            }
            start = end;
        }
        assert_eq!(values.len(), 364);
        for left in &values {
            for right in &values {
                let distance = u32::try_from(strsim::osa_distance(left, right)).unwrap();
                for max in 0..=3 {
                    assert_eq!(
                        edit_distance(left, right, max),
                        (distance <= max).then_some(distance),
                        "{left:?} / {right:?} / {max}"
                    );
                }
            }
        }
    }

    #[test]
    fn ascii_typo_distance_matches_the_reference_near_length_limits() {
        for length in 0..=55 {
            let left: String = (0..length)
                .map(|index| ['a', 'b', 'c', 'd', 'e'][index % 5])
                .collect();
            for other_length in 0..=55 {
                let mut right = left.clone();
                right.truncate(other_length);
                while right.len() < other_length {
                    right.push('x');
                }
                let mut transposed = right.as_bytes().to_vec();
                for pair in transposed.as_chunks_mut::<2>().0 {
                    pair.swap(0, 1);
                }
                let transposed = String::from_utf8(transposed).unwrap();
                for right in [&right, &transposed] {
                    let distance = u32::try_from(strsim::osa_distance(&left, right)).unwrap();
                    for max in 0..=3 {
                        let expected = (other_length <= 50 && distance <= max).then_some(distance);
                        assert_eq!(
                            edit_distance(&left, right, max),
                            expected,
                            "{left:?} / {right:?} / {max}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn case_insensitive_matches_keep_unicode_lowercase_expansions() {
        for (query, token) in [
            ("EDITOR", "editor"),
            ("éditeur", "ÉDITEUR"),
            ("İ", "İ"),
            ("i\u{307}", "İ"),
        ] {
            let package = row(&format!("cask:homebrew/cask/{token}"), token, "");
            assert_eq!(
                SearchQuery::new(query, false, false)
                    .unwrap()
                    .rank(&package),
                Some(0),
                "{query} / {token}"
            );
        }
        let package = row("nixpkgs:editor", "Editor", "Éditeur pour ÄPFEL");
        assert!(
            SearchQuery::new("äpfel", false, false)
                .unwrap()
                .fuzzy(false)
                .rank(&package)
                .is_some()
        );
        assert!(
            SearchQuery::new("ÉDIT", false, false)
                .unwrap()
                .fuzzy(false)
                .rank(&row("cask:homebrew/cask/editor", "Éditeur", ""))
                .is_some()
        );
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
