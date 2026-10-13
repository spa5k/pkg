//! Host availability shared by cask discovery and mutation gates.

use std::cell::OnceCell;

use crate::nix;

use super::CatalogError;

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
    pub(super) fn search_badge(
        self,
        include_unavailable: bool,
        detail: Option<&str>,
    ) -> Option<super::SupportBadge> {
        match self {
            Self::Available => Some(super::SupportBadge::Eligible),
            Self::Unavailable { reason } if include_unavailable => {
                Some(super::SupportBadge::Excluded {
                    reason,
                    detail: detail.map(str::to_owned),
                })
            }
            Self::Unknown { reason } if include_unavailable => {
                Some(super::SupportBadge::Unknown { reason })
            }
            _ => None,
        }
    }

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
    macos: OnceCell<Result<String, std::sync::Arc<nix::HostVersionError>>>,
}

impl CaskHost {
    /// Judge a record using its catalog status, platform, and macOS bounds.
    /// # Errors
    /// Returns the retained error if a host-version probe is cancelled.
    pub fn availability(
        &self,
        index: &nix::CatalogIndex,
        system: &str,
        source: &str,
        token: &str,
    ) -> Result<Availability, CatalogError> {
        if !index.targeted(system) {
            return Ok(Availability::Unavailable {
                reason: format!(
                    "catalog targets {} only; this system is {system}",
                    index.targets().join(", ")
                ),
            });
        }
        let Some(entry) = index.record(system, source, token) else {
            return Ok(Availability::Unknown {
                reason: format!("not in the generated catalog for {system}"),
            });
        };
        Ok(match entry.status {
            nix::EntryStatus::Excluded => Availability::Unavailable {
                reason: format!(
                    "{}{}",
                    entry.reason.as_deref().unwrap_or_default(),
                    entry
                        .detail
                        .as_deref()
                        .map(|text| format!(": {text}"))
                        .unwrap_or_default()
                ),
            },
            nix::EntryStatus::Eligible => {
                if !system.ends_with("-darwin")
                    || entry.min_macos.is_none() && entry.max_macos.is_none()
                {
                    return Ok(Availability::Available);
                }
                match self
                    .macos
                    .get_or_init(|| nix::macos_product_version().map_err(std::sync::Arc::new))
                {
                    Ok(host) => {
                        macos_range(host, entry.min_macos.as_deref(), entry.max_macos.as_deref())
                    }
                    Err(error) if error.signal().is_some() => {
                        return Err(CatalogError::Host(error.clone()));
                    }
                    Err(detail) => Availability::Unknown {
                        reason: format!("host macOS version cannot be read: {detail}"),
                    },
                }
            }
        })
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
    fn cancelled_host_probes_are_errors_only_when_the_record_needs_them() {
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/catalog-index.json")).unwrap();
        let index = nix::decode_catalog_index(&raw).unwrap();
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            for error in [
                nix::HostVersionError::Interrupted(signal),
                nix::HostVersionError::Process(pkg_process::RunError {
                    cancelled: Some(signal),
                    source: std::io::Error::from_raw_os_error(libc::ECHILD),
                }),
            ] {
                let host = CaskHost {
                    macos: OnceCell::from(Err(std::sync::Arc::new(error))),
                };
                let error = host
                    .availability(&index, "aarch64-darwin", "homebrew/cask", "iterm2")
                    .expect_err("cancelled host requirement");
                assert_eq!(error.signal(), Some(signal));
                assert!(matches!(
                    host.availability(&index, "x86_64-linux", "homebrew/cask", "iterm2")
                        .unwrap(),
                    Availability::Unavailable { .. }
                ));
                assert!(matches!(
                    host.availability(&index, "aarch64-darwin", "homebrew/cask", "missing")
                        .unwrap(),
                    Availability::Unknown { .. }
                ));
            }
        }
    }

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
