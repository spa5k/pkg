use crate::{AcquiredInstall, AcquiredOutput};

/// Acquired outputs that retain exact expected-path equality.
#[derive(Debug)]
pub struct VerifiedInstall {
    outputs: Vec<AcquiredOutput>,
}
impl VerifiedInstall {
    /// Returns trusted outputs in deterministic order.
    #[must_use]
    pub fn outputs(&self) -> &[AcquiredOutput] {
        &self.outputs
    }
}

/// Rechecks the expected/acquired identity binding before staging.
pub fn verify_acquired(acquired: AcquiredInstall) -> Result<VerifiedInstall, AcquiredInstall> {
    if acquired
        .outputs()
        .iter()
        .any(|output| output.planned().store_path() != output.substitute().store_path())
    {
        Err(acquired)
    } else {
        Ok(VerifiedInstall {
            outputs: acquired.into_outputs(),
        })
    }
}
