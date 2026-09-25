//! Bounded, read-only startup observation. A negative ownership result is final.

use std::time::{Duration, Instant};

use pkg_nix::BrokerOperationKind;

use crate::broker::{BrokerClientError, BrokerClientErrorCode, BrokerLifecycleClient};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_INTERVAL: Duration = Duration::from_millis(100);

pub(super) struct Health {
    pub(super) version: String,
    pub(super) managed_ownership: bool,
}

enum Failure {
    Startup,
    Final,
}

impl From<BrokerClientError> for Failure {
    fn from(error: BrokerClientError) -> Self {
        match error.code() {
            BrokerClientErrorCode::Unavailable | BrokerClientErrorCode::TransportFailure => {
                Self::Startup
            }
            _ => Self::Final,
        }
    }
}

pub(super) fn observe(on_wait: impl FnMut()) -> Option<Health> {
    observe_until(
        Instant::now() + STARTUP_TIMEOUT,
        BrokerLifecycleClient::connect_default_until,
        on_wait,
    )
}

fn observe_until(
    deadline: Instant,
    mut connect: impl FnMut(Instant) -> Result<BrokerLifecycleClient, BrokerClientError>,
    mut on_wait: impl FnMut(),
) -> Option<Health> {
    let mut notified = false;
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        match connect(deadline).map_err(Failure::from).and_then(probe) {
            Ok(health) => return Some(health),
            Err(Failure::Startup) => {}
            Err(Failure::Final) => return None,
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return None;
        }
        if !notified {
            on_wait();
            notified = true;
        }
        std::thread::sleep(RETRY_INTERVAL.min(remaining));
    }
}

fn probe(mut broker: BrokerLifecycleClient) -> Result<Health, Failure> {
    let handle = broker.begin(BrokerOperationKind::Doctor)?;
    let result: Result<_, BrokerClientError> = (|| {
        let version = broker.version(handle.clone())?;
        let managed_ownership = broker.verify_managed_ownership(handle.clone())?;
        Ok(Health {
            version: version.nix_version().as_str().to_owned(),
            managed_ownership,
        })
    })();
    match result {
        Ok(health) => {
            // A cleanup failure must not turn an ownership refusal into a retry.
            broker.complete(handle).map_err(|error| {
                if health.managed_ownership {
                    Failure::from(error)
                } else {
                    Failure::Final
                }
            })?;
            Ok(health)
        }
        Err(error) => {
            let _ = broker.cancel(handle);
            Err(Failure::from(error))
        }
    }
}

#[cfg(test)]
mod tests;
