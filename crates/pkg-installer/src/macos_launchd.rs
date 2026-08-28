//! Fixed launchd lifecycle for the two product services.

use crate::MacOsError;

const LAUNCHCTL: &str = "/bin/launchctl";

const JOBS: [(&str, &str); 2] = [
    (
        "org.pkg.root-helper",
        "/Library/LaunchDaemons/org.pkg.root-helper.plist",
    ),
    (
        "org.pkg.nix-broker",
        "/Library/LaunchDaemons/org.pkg.nix-broker.plist",
    ),
];

pub struct MacOsLaunchdManager {
    activated: Vec<&'static str>,
}

impl MacOsLaunchdManager {
    pub(crate) const fn new() -> Self {
        Self {
            activated: Vec::new(),
        }
    }

    pub(crate) fn classify_activation() -> Result<bool, MacOsError> {
        let active = JOBS
            .iter()
            .map(|(label, _)| is_active(label))
            .collect::<Result<Vec<_>, _>>()?;
        if active.iter().all(|value| *value) {
            Ok(true)
        } else if active.iter().all(|value| !*value) {
            Ok(false)
        } else {
            Err(MacOsError::backend_failure())
        }
    }

    pub(crate) fn activate(&mut self) -> Result<bool, MacOsError> {
        if Self::classify_activation()? {
            return Ok(false);
        }
        for (label, plist) in JOBS {
            let target = format!("system/{label}");
            run_status(&["enable", &target])?;
            run_status(&["bootstrap", "system", plist])?;
            self.activated.push(label);
        }
        Self::verify_active()?;
        Ok(true)
    }

    pub(crate) fn verify_active() -> Result<(), MacOsError> {
        if JOBS.iter().all(|(label, _)| is_active(label) == Ok(true)) {
            Ok(())
        } else {
            Err(MacOsError::backend_failure())
        }
    }

    pub(crate) fn rollback(&mut self) -> Result<(), MacOsError> {
        let mut failed = false;
        for label in self.activated.iter().rev() {
            failed |= bootout(label).is_err();
        }
        if failed {
            return Err(MacOsError::backend_failure());
        }
        self.activated.clear();
        Ok(())
    }

    pub(crate) fn deactivate_verified() -> Result<(), MacOsError> {
        let mut failed = false;
        for (label, _) in JOBS.iter().rev() {
            if is_active(label)? && bootout(label).is_err() {
                failed |= is_active(label)?;
            }
            let target = format!("system/{label}");
            failed |= run_status(&["disable", &target]).is_err();
        }
        if failed || Self::require_offline().is_err() {
            Err(MacOsError::backend_failure())
        } else {
            Ok(())
        }
    }

    pub(crate) fn require_offline() -> Result<(), MacOsError> {
        if JOBS.iter().any(|(label, _)| is_active(label) != Ok(false)) {
            return Err(MacOsError::backend_failure());
        }
        let (code, output) =
            crate::linux_accounts::run_capture_status(LAUNCHCTL, &["print-disabled", "system"])
                .map_err(|_| MacOsError::backend_failure())?;
        if code != Some(0) || !JOBS.iter().all(|(label, _)| disabled_in(&output, label)) {
            return Err(MacOsError::backend_failure());
        }
        Ok(())
    }
}

pub fn verify_macos_services_absent() -> Result<(), MacOsError> {
    MacOsLaunchdManager::require_offline()
}

fn is_active(label: &str) -> Result<bool, MacOsError> {
    let target = format!("system/{label}");
    let (code, _) = crate::linux_accounts::run_capture_status(LAUNCHCTL, &["print", &target])
        .map_err(|_| MacOsError::backend_failure())?;
    match code {
        Some(0) => Ok(true),
        Some(113) => Ok(false),
        _ => Err(MacOsError::backend_failure()),
    }
}

fn bootout(label: &str) -> Result<(), MacOsError> {
    let target = format!("system/{label}");
    run_status(&["bootout", &target])
}

fn run_status(arguments: &[&str]) -> Result<(), MacOsError> {
    crate::linux_accounts::run_status(LAUNCHCTL, arguments)
        .map_err(|_| MacOsError::backend_failure())
}

fn disabled_in(output: &[u8], label: &str) -> bool {
    std::str::from_utf8(output).is_ok_and(|text| {
        let prefix = format!("\"{label}\" => ");
        let values = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix(&prefix))
            .collect::<Vec<_>>();
        matches!(values.as_slice(), ["true" | "disabled"])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_contract_has_unique_fixed_labels_and_absolute_plists() {
        let labels = JOBS
            .iter()
            .map(|(label, _)| *label)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(labels.len(), JOBS.len());
        assert!(
            JOBS.iter()
                .all(|(_, path)| path.starts_with("/Library/LaunchDaemons/"))
        );
    }

    #[test]
    fn disabled_parser_accepts_only_one_approved_entry() {
        let output = b"disabled services = {\n\t\"org.pkg.root-helper\" => true\n}";
        assert!(disabled_in(output, "org.pkg.root-helper"));
        assert!(!disabled_in(output, "org.pkg.nix-broker"));
        assert!(!disabled_in(
            b"\"org.pkg.root-helper\" => false",
            "org.pkg.root-helper"
        ));
        assert!(!disabled_in(
            b"\"org.pkg.root-helper\" => true\n\"org.pkg.root-helper\" => false",
            "org.pkg.root-helper"
        ));
        assert!(!disabled_in(
            b"\"org.pkg.root-helper\" => true trailing",
            "org.pkg.root-helper"
        ));
        assert!(disabled_in(
            b"\"org.pkg.root-helper\" => disabled",
            "org.pkg.root-helper"
        ));
        assert!(!disabled_in(
            b"\"org.pkg.root-helper\" => enabled",
            "org.pkg.root-helper"
        ));
        assert!(!disabled_in(
            b"\"org.pkg.root-helper\" => unknown",
            "org.pkg.root-helper"
        ));
    }
}
