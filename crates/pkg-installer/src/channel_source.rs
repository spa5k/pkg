//! Runtime selection of the release channel URLs.
//!
//! One base URL selects both channel endpoints: `metadata/` and `targets/`
//! are joined onto the base. The base URL is a delivery detail only; the
//! embedded trusted root still authenticates all channel content.

use pkg_channel::validate_https_repository_url;
use url::Url;

/// The channel base URL does not produce a valid pair of channel URLs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelSourceError {
    /// The base URL is not parsable or a derived URL fails the HTTPS checks.
    InvalidBaseUrl,
}

impl std::fmt::Display for ChannelSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the channel base URL is not a valid HTTPS base URL")
    }
}

impl std::error::Error for ChannelSourceError {}

/// Derives the metadata and targets URLs from one channel base URL.
///
/// The base is normalized to end with exactly one `/` before the join, so
/// `https://channel.test/test/n` and `https://channel.test/test/n/` produce
/// the same pair.
///
/// # Errors
/// Returns [`ChannelSourceError::InvalidBaseUrl`] when the base is not a
/// parsable URL or when either derived URL is not an HTTPS repository URL
/// that ends with `/`.
pub fn derive_channel_urls(base: &str) -> Result<(Url, Url), ChannelSourceError> {
    let normalized = format!("{}/", base.trim_end_matches('/'));
    let base = Url::parse(&normalized).map_err(|_| ChannelSourceError::InvalidBaseUrl)?;
    let metadata = joined(&base, "metadata/")?;
    let targets = joined(&base, "targets/")?;
    if !metadata.path().ends_with('/')
        || !targets.path().ends_with('/')
        || validate_https_repository_url(&metadata).is_err()
        || validate_https_repository_url(&targets).is_err()
    {
        return Err(ChannelSourceError::InvalidBaseUrl);
    }
    Ok((metadata, targets))
}

fn joined(base: &Url, segment: &str) -> Result<Url, ChannelSourceError> {
    base.join(segment)
        .map_err(|_| ChannelSourceError::InvalidBaseUrl)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests may unwrap")]
mod tests {
    use super::*;

    #[test]
    fn https_base_yields_both_channel_urls() {
        let (metadata, targets) = derive_channel_urls("https://channel.test/test/n/").unwrap();
        assert_eq!(metadata.as_str(), "https://channel.test/test/n/metadata/");
        assert_eq!(targets.as_str(), "https://channel.test/test/n/targets/");
    }

    #[test]
    fn http_base_is_rejected() {
        assert_eq!(
            derive_channel_urls("http://channel.test/test/n/"),
            Err(ChannelSourceError::InvalidBaseUrl)
        );
    }

    #[test]
    fn base_without_trailing_slash_joins_the_same_way() {
        let bare = derive_channel_urls("https://channel.test/test/n");
        let slashed = derive_channel_urls("https://channel.test/test/n/");
        assert_eq!(bare, slashed);
    }

    #[test]
    fn repeated_trailing_slashes_collapse_to_one() {
        let (metadata, _) = derive_channel_urls("https://channel.test/test/n///").unwrap();
        assert_eq!(metadata.as_str(), "https://channel.test/test/n/metadata/");
    }

    #[test]
    fn empty_base_is_rejected() {
        assert_eq!(
            derive_channel_urls(""),
            Err(ChannelSourceError::InvalidBaseUrl)
        );
        assert_eq!(
            derive_channel_urls("/metadata/"),
            Err(ChannelSourceError::InvalidBaseUrl)
        );
    }

    #[test]
    fn embedded_credentials_are_rejected() {
        assert_eq!(
            derive_channel_urls("https://user@channel.test/test/n/"),
            Err(ChannelSourceError::InvalidBaseUrl)
        );
    }
}
