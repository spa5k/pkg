//! Host availability shared by cask discovery and mutation gates.

use std::cell::OnceCell;

use crate::nix;

use super::{CaskStatus, CatalogView};

/// Whether a catalog record can be installed on the current host.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Availability {
    /// Catalog and host requirements allow installation.
    Available,
    /// A known requirement prevents installation.
    Unavailable {
        /// The recorded exclusion or host requirement.
        reason: String,
    },
    /// Required metadata or host state cannot be read.
    Unknown {
        /// Why the decision cannot be made.
        reason: String,
    },
}

impl Availability {
    /// Refuse unavailable or unknown records before a profile mutation.
    /// # Errors
    ///
    /// Returns the recorded reason when the record is unavailable or its status is unknown.
    pub fn require(&self) -> Result<(), String> {
        match self {
            Self::Available => Ok(()),
            Self::Unavailable { reason } | Self::Unknown { reason } => Err(reason.clone()),
        }
    }
}

/// Read host macOS metadata at most once during a command.
#[derive(Default)]
pub struct CaskHost {
    macos: OnceCell<Result<String, String>>,
}

impl CaskHost {
    /// Judge a record using its catalog status, platform, and macOS bounds.
    #[must_use]
    pub fn availability(
        &self,
        view: &CatalogView,
        system: &str,
        source: &str,
        token: &str,
    ) -> Availability {
        if !view.targeted(system) {
            return Availability::Unavailable {
                reason: format!(
                    "catalog targets {} only; this system is {system}",
                    view.targets().join(", ")
                ),
            };
        }
        match view.entry(system, source, token) {
            CaskStatus::Excluded { reason, detail } => Availability::Unavailable {
                reason: format!(
                    "{reason}{}",
                    detail.map(|text| format!(": {text}")).unwrap_or_default()
                ),
            },
            CaskStatus::Unknown => Availability::Unknown {
                reason: format!("not in the generated catalog for {system}"),
            },
            CaskStatus::Eligible => {
                let Some(entry) = view.record(system, source, token) else {
                    return Availability::Unknown {
                        reason: String::from("catalog record is missing"),
                    };
                };
                if !system.ends_with("-darwin")
                    || entry.min_macos.is_none() && entry.max_macos.is_none()
                {
                    return Availability::Available;
                }
                match self.macos.get_or_init(nix::macos_product_version) {
                    Ok(host) => {
                        macos_range(host, entry.min_macos.as_deref(), entry.max_macos.as_deref())
                    }
                    Err(detail) => Availability::Unknown {
                        reason: format!("host macOS version cannot be read: {detail}"),
                    },
                }
            }
        }
    }
}

fn macos_range(host: &str, min: Option<&str>, max: Option<&str>) -> Availability {
    if nix::in_range(host, min, max) {
        return Availability::Available;
    }
    let range = match (min, max) {
        (Some(min), Some(max)) => format!("macOS >= {min} and <= {max}"),
        (Some(min), None) => format!("macOS >= {min}"),
        (None, Some(max)) => format!("macOS <= {max}"),
        (None, None) => return Availability::Available,
    };
    Availability::Unavailable {
        reason: format!("requires {range}; this host runs macOS {host}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_range_has_inclusive_numeric_bounds() {
        assert!(matches!(
            macos_range("15.10", Some("15.9"), Some("15.10")),
            Availability::Available
        ));
        assert!(matches!(
            macos_range("15.8", Some("15.9"), None),
            Availability::Unavailable { .. }
        ));
        assert!(matches!(
            macos_range("16.0", None, Some("15.9")),
            Availability::Unavailable { .. }
        ));
    }
}
