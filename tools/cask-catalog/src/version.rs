//! Numeric macOS versions and inclusive bounds shared with the client.

/// Parse dot-separated ASCII digits. Reject empty components and overflow.
#[must_use]
pub fn parse(text: &str) -> Option<Vec<u64>> {
    if text.is_empty() {
        return None;
    }
    text.split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse::<u64>().ok()
        })
        .collect()
}

/// Compare numeric versions with missing components equal to zero.
#[must_use]
pub fn less_than(a: &str, b: &str) -> Option<bool> {
    Some(compare(&parse(a)?, &parse(b)?) == std::cmp::Ordering::Less)
}

fn compare(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let length = a.len().max(b.len());
    a.iter()
        .copied()
        .chain(std::iter::repeat(0))
        .take(length)
        .cmp(b.iter().copied().chain(std::iter::repeat(0)).take(length))
}

/// Check inclusive bounds. Reject an invalid host or bound.
#[must_use]
pub fn in_range(host: &str, min: Option<&str>, max: Option<&str>) -> bool {
    let Some(host) = parse(host) else {
        return false;
    };
    let bound_order = |bound: &str| parse(bound).map(|bound| compare(&host, &bound));
    min.is_none_or(|min| bound_order(min).is_some_and(|order| order != std::cmp::Ordering::Less))
        && max.is_none_or(|max| {
            bound_order(max).is_some_and(|order| order != std::cmp::Ordering::Greater)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_comparison_zero_fills_and_respects_both_bounds() {
        // Zero fill: 15 == 15.0.0.
        assert!(in_range("15", Some("15.0.0"), None));
        assert!(in_range("15.7.7", Some("15"), Some("15.7.7")));
        assert!(!in_range("15.7.6", Some("15.7.7"), None));
        assert!(!in_range("26.0", None, Some("15.7.7")));
        assert!(in_range("14.9", None, Some("15")));
        // No bounds constrains nothing, but the host is still strict.
        assert!(in_range("15", None, None));
        assert!(!in_range("anything-unparseable", None, None));
    }

    #[test]
    fn oversized_and_signed_versions_fail_closed() {
        // An impossibly large bound or host must never collapse to zero
        // and sneak past the range (the previous parser accepted these).
        let huge = "99999999999999999999";
        assert!(!in_range(huge, None, Some("15")));
        assert!(!in_range("15", Some(huge), None));
        assert!(!in_range("15", Some("15"), Some(huge)));
        // Signs, whitespace, and empty components are rejected too.
        for bad in ["", ".", "15.", ".7", " 15", "15 ", "+15", "-15", "15a"] {
            assert!(parse(bad).is_none(), "{bad:?}");
            assert!(!in_range(bad, Some("15"), None), "{bad:?}");
            assert!(!in_range("15", Some(bad), None), "{bad:?}");
        }
    }
}
