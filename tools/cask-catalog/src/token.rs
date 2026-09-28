//! Token validity: catalog map keys and Nix attribute names.

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

/// Split invalid tokens out of a token list, keeping input order.
#[must_use]
pub fn classify_invalid<'a>(tokens: &[&'a str]) -> (Vec<&'a str>, Vec<&'a str>) {
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for token in tokens {
        if is_valid_token(token) {
            valid.push(*token);
        } else {
            invalid.push(*token);
        }
    }
    (valid, invalid)
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

    #[test]
    fn splits_valid_from_invalid() {
        let (valid, invalid) = classify_invalid(&["iterm2", "Bad Token", "1password@7", "zoom"]);
        assert_eq!(valid, ["iterm2", "1password@7", "zoom"]);
        assert_eq!(invalid, ["Bad Token"]);
    }
}
