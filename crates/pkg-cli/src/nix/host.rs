//! Host macOS version reading for the cask runtime range gate.
//!
//! The version comes from the ABSOLUTE `/usr/bin/sw_vers` path through the
//! shared child signal boundary, so an interrupted read is classified and
//! pkg stays alive. No environment override exists in production: the
//! native macOS parent tests this against the real host.

use super::process::Outcome;

/// The absolute path of the host version tool.
const SW_VERS: &str = "/usr/bin/sw_vers";

/// Parse one numeric version component strictly.
///
/// This is the ONE strict parser owner for client range decode and host
/// compare: a component must be non-empty ASCII digits that fit `u64`.
/// Empty components, signs, whitespace, other characters, and numeric
/// overflow are all rejected, so an impossibly large bound can never be
/// zero-filled into a passable zero.
pub fn parse_numeric_version(text: &str) -> Option<Vec<u64>> {
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

/// Read the host macOS product version, for example `15.7.7`.
///
/// Returns `Err` when the tool cannot run or answers anything but a
/// single non-empty NUMERIC version line: success output is validated
/// through the same strict parser the bounds use. The caller fails
/// closed on that error. This function must only be called for a macOS
/// cask gate decision — never for Linux, Nixpkgs, or untargeted
/// catalogs.
pub fn macos_product_version() -> Result<String, String> {
    let args = vec![String::from("-productVersion")];
    let (outcome, stdout) =
        super::process::run_direct_captured(std::path::Path::new(SW_VERS), &args)?;
    match outcome {
        Outcome::Success => {}
        Outcome::Interrupted { .. } => {
            return Err(String::from("the version read was interrupted"));
        }
        Outcome::Failed { status, .. } => {
            return Err(format!("{SW_VERS} failed: {status}"));
        }
    }
    let version = stdout.trim();
    if version.is_empty() || version.contains('\n') || version.contains(' ') {
        return Err(format!(
            "{SW_VERS} answered an unusable version {version:?}"
        ));
    }
    if parse_numeric_version(version).is_none() {
        return Err(format!(
            "{SW_VERS} answered a non-numeric version {version:?}"
        ));
    }
    Ok(version.to_string())
}

/// Whether the host version satisfies the declared range.
///
/// Both bounds and the host version go through the ONE strict parser:
/// any unparseable side fails the range closed instead of collapsing to
/// zero. Valid shorter versions are zero-filled for the comparison.
#[must_use]
pub fn in_range(host: &str, min: Option<&str>, max: Option<&str>) -> bool {
    let Some(host) = parse_numeric_version(host) else {
        return false;
    };
    let compare = |bound: &str| -> Option<std::cmp::Ordering> {
        let bound = parse_numeric_version(bound)?;
        let len = host.len().max(bound.len());
        let fill = |mut v: Vec<u64>| {
            v.resize(len, 0);
            v
        };
        Some(fill(host.clone()).cmp(&fill(bound)))
    };
    min.is_none_or(|min| compare(min).is_some_and(|order| order != std::cmp::Ordering::Less))
        && max.is_none_or(|max| {
            compare(max).is_some_and(|order| order != std::cmp::Ordering::Greater)
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
            assert!(parse_numeric_version(bad).is_none(), "{bad:?}");
            assert!(!in_range(bad, Some("15"), None), "{bad:?}");
            assert!(!in_range("15", Some(bad), None), "{bad:?}");
        }
    }
}
