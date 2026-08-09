use std::sync::Arc;

use crate::policy::ChannelError;

const MAX_TRUSTED_ROOT_BYTES: usize = 64 * 1024;

/// The root-of-trust bytes embedded in the `pkg` binary.
///
/// There is intentionally no constructor that reads first-use trust from disk.
#[derive(Debug, Clone)]
pub struct TrustedRoot(Arc<[u8]>);

impl TrustedRoot {
    /// Validates and owns embedded TUF root bytes.
    pub fn from_embedded(bytes: &'static [u8]) -> Result<Self, ChannelError> {
        if bytes.is_empty() || bytes.len() > MAX_TRUSTED_ROOT_BYTES {
            return Err(ChannelError::InvalidTrustedRoot);
        }
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| ChannelError::InvalidTrustedRoot)?;
        if !value.is_object() {
            return Err(ChannelError::InvalidTrustedRoot);
        }
        Ok(Self(Arc::from(bytes)))
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.0
    }
}
