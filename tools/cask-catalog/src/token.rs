//! Catalog source and token identities shared by the generator and client.

/// Whether a source is one safe owner/tap pair.
#[must_use]
pub fn is_valid_source(source: &str) -> bool {
    source.split_once('/').is_some_and(|(owner, tap)| {
        [owner, tap].into_iter().all(|part| {
            !part.is_empty()
                && !matches!(part, "." | "..")
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    })
}

/// Build the full catalog identity from its source and token.
#[must_use]
pub fn full_entry_id(source: &str, token: &str) -> String {
    format!("{source}/{token}")
}

/// Split a valid full identity without allocating its source or token.
#[must_use]
pub fn split_entry_id(id: &str) -> Option<(&str, &str)> {
    let (source, token) = id.rsplit_once('/')?;
    (is_valid_source(source) && is_valid_token(token)).then_some((source, token))
}

/// Homebrew-style token charset accepted as a catalog key.
///
/// Contract: one label starting with a lowercase letter or digit, then
/// lowercase letters, digits, `+`, `.`, `_`, `-`, `@` (versioned tokens
/// such as `1password-cli@1` are real data). No uppercase and no `..`
/// path component. Tokens are opaque keys; consumers that use them as
/// Nix attribute segments must quote them as one segment.
#[must_use = "the result states whether the token can be a catalog key"]
pub fn is_valid_token(token: &str) -> bool {
    let Some(first) = token.chars().next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    if !token.chars().skip(1).all(|c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '.' | '_' | '-' | '@')
    }) {
        return false;
    }
    // No `..` component: never usable as one path segment.
    !token.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_real_token_shapes_and_rejects_unsafe_ones() {
        for ok in [
            "iterm2",
            "1password-cli",
            "1password-cli@beta",
            "4k-video-downloader+",
            "xournal++",
            "font-0xproto",
            "a+b",
            "v2.1",
        ] {
            assert!(is_valid_token(ok), "{ok} must be valid");
        }
        for bad in ["", "Foo", "..", "a..b", "-lead", "a/b", "a b", "@x"] {
            assert!(!is_valid_token(bad), "{bad} must be invalid");
        }
    }
}
