//! Journaled platform backends that run inside one provision attempt.

use super::provision::*;
use super::*;
pub(super) struct LinuxBundleBackend<'a, 'j, P> {
    inner: &'a mut dyn LinuxInstallBackend,
    request: &'a InstallerProvisionRequest<'a>,
    provisioner: &'a mut P,
    outcome: Option<BootstrapOutcome>,
    journal: Option<LinuxJournalTransaction<'j>>,
}

pub(super) struct LinuxJournalTransaction<'a> {
    storage: &'a dyn LinuxJournalPersistence,
    journal: LinuxInstallJournal,
}

pub(super) trait LinuxJournalPersistence {
    fn replace(&self, journal: &LinuxInstallJournal) -> Result<(), InstallError>;
}

impl LinuxJournalPersistence for LinuxInstallJournalStorage {
    fn replace(&self, journal: &LinuxInstallJournal) -> Result<(), InstallError> {
        Self::replace(self, journal).map_err(|_| InstallError::backend_failure())
    }
}

impl LinuxJournalTransaction<'_> {
    pub(super) fn begin(
        &mut self,
        mutation: LinuxInstallMutation,
        presence: LinuxAssetPresence,
    ) -> Result<(), InstallError> {
        if presence == LinuxAssetPresence::Absent {
            self.journal
                .intend(mutation)
                .map_err(|_| InstallError::backend_failure())?;
            self.persist()?;
        }
        Ok(())
    }

    pub(super) fn complete(
        &mut self,
        mutation: LinuxInstallMutation,
        presence: LinuxAssetPresence,
        changed: bool,
    ) -> Result<(), InstallError> {
        if changed != (presence == LinuxAssetPresence::Absent) {
            return Err(InstallError::backend_failure());
        }
        if changed {
            self.journal
                .complete_created()
                .map_err(|_| InstallError::backend_failure())?;
        } else {
            self.journal
                .record_preexisting(mutation)
                .map_err(|_| InstallError::backend_failure())?;
        }
        self.persist()
    }

    pub(super) fn begin_services(&mut self) -> Result<(), InstallError> {
        self.journal
            .intend_services()
            .map_err(|_| InstallError::backend_failure())?;
        self.persist()
    }

    pub(super) fn record_preexisting(
        &mut self,
        mutation: LinuxInstallMutation,
    ) -> Result<(), InstallError> {
        self.journal
            .record_preexisting(mutation)
            .map_err(|_| InstallError::backend_failure())?;
        self.persist()
    }

    pub(super) fn complete_rollback(
        &mut self,
        mutation: &LinuxInstallMutation,
    ) -> Result<(), InstallError> {
        self.journal
            .complete_recovery_action(mutation)
            .map_err(|_| InstallError::rollback_incomplete())?;
        self.persist()
            .map_err(|_| InstallError::rollback_incomplete())
    }

    pub(super) fn commit(&mut self) -> Result<(), InstallError> {
        self.journal
            .commit()
            .map_err(|_| InstallError::rollback_incomplete())?;
        self.storage
            .replace(&self.journal)
            .map_err(|_| InstallError::rollback_incomplete())
    }

    pub(super) fn persist(&self) -> Result<(), InstallError> {
        self.storage.replace(&self.journal)
    }
}

impl<P: BundleProvisioner> LinuxInstallBackend for LinuxBundleBackend<'_, '_, P> {
    fn install_mode(&self) -> crate::LinuxInstallMode {
        self.inner.install_mode()
    }

    fn preflight_product_mutation(&mut self) -> Result<(), InstallError> {
        if let Some(transaction) = self.journal.as_ref()
            && transaction.journal.fresh_services_deactivated()
        {
            return self
                .inner
                .preflight_fresh_recovery_mutation(&transaction.journal);
        }
        self.inner.preflight_product_mutation()
    }

    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), InstallError> {
        self.inner.bind_authenticated_nix_config(config)
    }

    fn bind_authenticated_release_identity(
        &mut self,
        system: System,
        digest: Digest,
    ) -> Result<(), InstallError> {
        self.inner
            .bind_authenticated_release_identity(system, digest)
    }

    fn preflight_privilege(&mut self) -> Result<(), InstallError> {
        self.inner.preflight_privilege()
    }
    fn preflight_clean_host(&mut self, system: System) -> Result<(), InstallError> {
        self.inner.preflight_clean_host(system)
    }
    fn classify_asset(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<crate::LinuxAssetPresence, InstallError> {
        self.inner.classify_asset(asset)
    }
    fn classify_managed_runtime(&mut self) -> Result<LinuxAssetPresence, InstallError> {
        self.inner.classify_managed_runtime()
    }
    fn classify_services(&mut self) -> Result<LinuxAssetPresence, InstallError> {
        self.inner.classify_services()
    }
    fn services_need_mutation(&self, prior_active: bool) -> bool {
        self.inner.services_need_mutation(prior_active)
    }
    fn recover_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        self.inner.recover_asset(asset)
    }
    fn recover_repair_assets(&mut self) -> Result<(), InstallError> {
        self.inner.recover_repair_assets()
    }
    fn recover_fresh_services(&mut self) -> Result<(), InstallError> {
        self.inner.recover_fresh_services()
    }
    fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
        let mutation = asset_mutation(asset);
        let presence = self.inner.classify_asset(asset)?;
        begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
        self.inner.preflight_product_mutation()?;
        let changed = self.inner.ensure_asset(asset)?;
        complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn install_systemd_unit(
        &mut self,
        asset: LinuxInstallAsset,
        contents: &'static str,
    ) -> Result<bool, InstallError> {
        let mutation = asset_mutation(asset);
        let presence = self.inner.classify_asset(asset)?;
        begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
        self.inner.preflight_product_mutation()?;
        let changed = self.inner.install_systemd_unit(asset, contents)?;
        complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn provision_managed_runtime(&mut self) -> Result<bool, InstallError> {
        let mutation = LinuxInstallMutation::ManagedRuntime;
        let presence = self.inner.classify_managed_runtime()?;
        if presence == LinuxAssetPresence::ExactPresent
            && self
                .provisioner
                .reuse_existing()
                .map_err(linux_provision_error)?
        {
            self.outcome = Some(BootstrapOutcome::Existing);
            begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
            complete_linux_mutation(&mut self.journal, mutation, presence, false)?;
            return Ok(false);
        }
        self.provisioner
            .reauthenticate_linux(self.request, self.inner)
            .map_err(linux_provision_error)?;
        self.provisioner
            .preflight_workspace(self.request)
            .map_err(linux_provision_error)?;
        begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
        self.outcome = Some(
            self.provisioner
                .provision(self.request)
                .map_err(linux_provision_error)?,
        );
        let changed = presence == LinuxAssetPresence::Absent;
        complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(true)
    }
    fn rollback_managed_runtime(&mut self) -> Result<(), InstallError> {
        self.preflight_product_mutation()
            .map_err(|_| InstallError::rollback_incomplete())?;
        if !self
            .outcome
            .as_ref()
            .is_some_and(BootstrapOutcome::has_accepted_base_nix)
        {
            let Some(outcome) = self.outcome.take() else {
                return Err(InstallError::rollback_incomplete());
            };
            outcome.rollback_linux()?;
        }
        self.journal.as_mut().map_or(Ok(()), |journal| {
            if journal.journal.recovery_actions().iter().any(|action| {
                matches!(
                    action,
                    crate::LinuxInstallRecoveryAction::RevalidateIntended(
                        LinuxInstallMutation::ManagedRuntime
                    ) | crate::LinuxInstallRecoveryAction::RevertCreated(
                        LinuxInstallMutation::ManagedRuntime
                    )
                )
            }) {
                journal.complete_rollback(&LinuxInstallMutation::ManagedRuntime)
            } else {
                Ok(())
            }
        })
    }
    fn validate_base_nix(&mut self) -> Result<(), InstallError> {
        self.inner.validate_base_nix()
    }
    fn accept_base_nix_handoff(&mut self) -> Result<(), InstallError> {
        match self.outcome.as_ref() {
            Some(BootstrapOutcome::DeterminatePending { handoff, .. }) => handoff
                .accept_after_installed_state_proof()
                .map_err(|_| InstallError::backend_failure()),
            #[cfg(test)]
            Some(BootstrapOutcome::DeterminateTestPending(handoff)) => handoff
                .accept_after_installed_state_proof()
                .map_err(|_| InstallError::backend_failure()),
            Some(_) => Ok(()),
            None => Err(InstallError::backend_failure()),
        }
    }
    fn activate_services(&mut self) -> Result<bool, InstallError> {
        if let Some(BootstrapOutcome::DeterminatePending { bundle, .. }) = self.outcome.as_mut() {
            bundle
                .commit_authenticated_channel()
                .map_err(|_| InstallError::backend_failure())?;
        }
        let mutation = LinuxInstallMutation::Services;
        let presence = self.inner.classify_services()?;
        let prior_active = presence == LinuxAssetPresence::ExactPresent;
        let needed = self.inner.services_need_mutation(prior_active);
        if needed {
            self.journal
                .as_mut()
                .ok_or_else(InstallError::backend_failure)?
                .begin_services()?;
        }
        let changed = self.inner.activate_services()?;
        if changed != needed {
            return Err(InstallError::backend_failure());
        }
        if needed {
            let transaction = self
                .journal
                .as_mut()
                .ok_or_else(InstallError::backend_failure)?;
            transaction
                .journal
                .complete_created()
                .map_err(|_| InstallError::backend_failure())?;
            transaction.persist()?;
        } else if let Some(transaction) = self.journal.as_mut() {
            transaction.record_preexisting(mutation)?;
        }
        Ok(changed)
    }
    fn rollback_services(&mut self) -> Result<(), InstallError> {
        self.inner.rollback_services()?;
        if let Some(transaction) = self.journal.as_mut() {
            transaction
                .journal
                .mark_fresh_services_deactivated()
                .map_err(|_| InstallError::rollback_incomplete())?;
            transaction.persist()?;
            transaction.complete_rollback(&LinuxInstallMutation::Services)?;
        }
        Ok(())
    }
    fn finish_fresh_services_rollback(&mut self) -> Result<(), InstallError> {
        self.inner.finish_fresh_services_rollback()
    }
    fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
        self.inner.check_managed_daemon()
    }
    fn publish_ownership_receipt(&mut self) -> Result<bool, InstallError> {
        let asset = crate::linux_install_assets()
            .iter()
            .copied()
            .find(|asset| asset.id() == "uninstall-manifest")
            .ok_or_else(InstallError::backend_failure)?;
        let mutation = asset_mutation(asset);
        let presence = self.inner.classify_ownership_receipt(asset)?;
        // An exact receipt is verified and reused by the production backend.
        // It is not rewritten on reinstall.
        begin_linux_mutation(&mut self.journal, mutation.clone(), presence)?;
        self.inner.preflight_product_mutation()?;
        let outcome = self
            .outcome
            .take()
            .ok_or_else(InstallError::backend_failure)?;
        match outcome {
            BootstrapOutcome::DeterminatePending { bundle, handoff } => {
                let changed = match publish_determinate_receipt(
                    self.inner,
                    &mut self.journal,
                    mutation,
                    presence,
                ) {
                    Ok(changed) => changed,
                    Err(error) => {
                        self.outcome =
                            Some(BootstrapOutcome::DeterminatePending { bundle, handoff });
                        return Err(error);
                    }
                };
                self.outcome = Some(BootstrapOutcome::DeterminateComplete);
                Ok(changed)
            }
            #[cfg(test)]
            BootstrapOutcome::Stub(rolled_back) => {
                let result = self.inner.publish_ownership_receipt();
                self.outcome = Some(BootstrapOutcome::Stub(rolled_back));
                let changed = result?;
                complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
                Ok(changed)
            }
            #[cfg(test)]
            BootstrapOutcome::DeterminateTestPending(handoff) => {
                let result =
                    publish_determinate_receipt(self.inner, &mut self.journal, mutation, presence);
                self.outcome = Some(if result.is_ok() {
                    BootstrapOutcome::DeterminateComplete
                } else {
                    BootstrapOutcome::DeterminateTestPending(handoff)
                });
                result
            }
            BootstrapOutcome::DeterminateComplete => Err(InstallError::backend_failure()),
            BootstrapOutcome::Existing => {
                let result = self.inner.publish_ownership_receipt();
                self.outcome = Some(BootstrapOutcome::Existing);
                let changed = result?;
                complete_linux_mutation(&mut self.journal, mutation, presence, changed)?;
                Ok(changed)
            }
        }
    }
    fn finalize_ownership_receipt(&mut self) -> Result<(), InstallError> {
        self.inner.finalize_ownership_receipt()
    }
    fn rollback_asset(&mut self, asset: LinuxInstallAsset) -> Result<(), InstallError> {
        self.preflight_product_mutation()
            .map_err(|_| InstallError::rollback_incomplete())?;
        self.inner.rollback_asset(asset)?;
        let mutation = asset_mutation(asset);
        self.journal
            .as_mut()
            .map_or(Ok(()), |journal| journal.complete_rollback(&mutation))
    }
}

pub(super) fn determinate_succeeded(outcome: DeterminateProcessOutcome) -> bool {
    outcome.terminal == DeterminateTerminal::Exited(0)
}

pub(super) fn publish_determinate_receipt(
    backend: &mut dyn LinuxInstallBackend,
    journal: &mut Option<LinuxJournalTransaction<'_>>,
    mutation: LinuxInstallMutation,
    presence: LinuxAssetPresence,
) -> Result<bool, InstallError> {
    let changed = backend.publish_ownership_receipt()?;
    complete_linux_mutation(journal, mutation, presence, changed)?;
    Ok(changed)
}

pub(super) fn run_with_new_determinate_handoff<T>(
    handoff: &DeterminateHandoff,
    start: impl FnOnce() -> Result<T, BundleProvisionError>,
) -> Result<T, BundleProvisionError> {
    match handoff.state().map_err(|_| BundleProvisionError::Failed)? {
        DeterminateHandoffState::NotStarted => {
            handoff
                .record_started()
                .map_err(|_| BundleProvisionError::Failed)?;
            start()
        }
        DeterminateHandoffState::Started | DeterminateHandoffState::Accepted => {
            Err(BundleProvisionError::Failed)
        }
    }
}

pub(super) fn asset_mutation(asset: LinuxInstallAsset) -> LinuxInstallMutation {
    LinuxInstallMutation::Asset {
        id: asset.id().to_owned(),
    }
}

pub(super) fn begin_linux_mutation(
    journal: &mut Option<LinuxJournalTransaction<'_>>,
    mutation: LinuxInstallMutation,
    presence: LinuxAssetPresence,
) -> Result<(), InstallError> {
    journal
        .as_mut()
        .map_or(Ok(()), |journal| journal.begin(mutation, presence))
}

pub(super) fn complete_linux_mutation(
    journal: &mut Option<LinuxJournalTransaction<'_>>,
    mutation: LinuxInstallMutation,
    presence: LinuxAssetPresence,
    changed: bool,
) -> Result<(), InstallError> {
    journal.as_mut().map_or(Ok(()), |journal| {
        journal.complete(mutation, presence, changed)
    })
}

pub(super) fn install_linux_with_provisioner_journaled<'a, P: BundleProvisioner>(
    system: System,
    request: &'a InstallerProvisionRequest<'a>,
    backend: &'a mut dyn LinuxInstallBackend,
    provisioner: &'a mut P,
    storage: &dyn LinuxJournalPersistence,
    journal: LinuxInstallJournal,
) -> Result<(LinuxInstallReport, BootstrapOutcome), InstallError> {
    if !matches!(system, System::X8664Linux | System::Aarch64Linux) {
        return Err(InstallError::backend_failure());
    }
    let mode = journal.mode();
    if backend.install_mode() != mode {
        return Err(InstallError::recovery_mode_mismatch());
    }
    if mode == crate::LinuxInstallMode::OfflineRepair {
        backend.preflight_clean_host(system)?;
    }
    let mut adapter = LinuxBundleBackend {
        inner: backend,
        request,
        provisioner,
        outcome: None,
        journal: Some(LinuxJournalTransaction { storage, journal }),
    };
    let report = match crate::installer::install_linux_journaled_preflighted(&mut adapter) {
        Ok(report) => report,
        Err(_) if mode == crate::LinuxInstallMode::OfflineRepair => {
            return Err(InstallError::rollback_incomplete());
        }
        Err(_)
            if mode == crate::LinuxInstallMode::FreshInstall
                && adapter
                    .outcome
                    .as_ref()
                    .is_some_and(BootstrapOutcome::has_accepted_base_nix) =>
        {
            return Err(InstallError::fresh_recovery_retained());
        }
        Err(error) => return Err(error),
    };
    adapter
        .journal
        .as_mut()
        .ok_or_else(InstallError::rollback_incomplete)?
        .commit()?;
    let committed = adapter
        .journal
        .as_ref()
        .ok_or_else(InstallError::rollback_incomplete)?;
    finalize_committed_linux_install(&committed.journal, adapter.inner)?;
    let outcome = adapter
        .outcome
        .take()
        .ok_or_else(InstallError::backend_failure)?;
    Ok((report, outcome))
}

pub(super) fn finalize_committed_linux_install(
    journal: &LinuxInstallJournal,
    backend: &mut dyn LinuxInstallBackend,
) -> Result<(), InstallError> {
    if !journal.is_committed() {
        return Err(InstallError::rollback_incomplete());
    }
    let mode = journal.mode();
    let system = journal
        .system()
        .map_err(|_| InstallError::rollback_incomplete())?;
    backend
        .preflight_recovery(mode, system)
        .and_then(|()| backend.finalize_ownership_receipt())
        .map_err(|_| InstallError::rollback_incomplete())
}

#[cfg(test)]
pub(super) fn install_linux_with_provisioner<'a, P: BundleProvisioner>(
    system: System,
    request: &'a InstallerProvisionRequest<'a>,
    backend: &'a mut dyn LinuxInstallBackend,
    provisioner: &'a mut P,
) -> Result<(LinuxInstallReport, BootstrapOutcome), InstallError> {
    let mut adapter = LinuxBundleBackend {
        inner: backend,
        request,
        provisioner,
        outcome: None,
        journal: None,
    };
    let report = crate::installer::install_linux(system, &mut adapter)?;
    let outcome = adapter
        .outcome
        .take()
        .ok_or_else(InstallError::backend_failure)?;
    Ok((report, outcome))
}

pub(super) struct MacOsBundleBackend<'a, 'j, P> {
    inner: &'a mut dyn MacOsInstallBackend,
    request: &'a InstallerProvisionRequest<'a>,
    provisioner: &'a mut P,
    outcome: Option<BootstrapOutcome>,
    journal: Option<MacOsJournalTransaction<'j>>,
    store_created: bool,
}

pub(super) struct MacOsJournalTransaction<'a> {
    storage: &'a dyn MacOsJournalPersistence,
    journal: MacOsInstallJournal,
}

pub(super) trait MacOsJournalPersistence {
    fn replace(&self, journal: &MacOsInstallJournal) -> Result<(), MacOsError>;
}

impl MacOsJournalPersistence for MacOsInstallJournalStorage {
    fn replace(&self, journal: &MacOsInstallJournal) -> Result<(), MacOsError> {
        Self::replace(self, journal).map_err(|_| MacOsError::backend_failure())
    }
}

impl MacOsJournalTransaction<'_> {
    pub(super) fn begin(
        &mut self,
        mutation: MacOsInstallMutation,
        presence: MacOsAssetPresence,
    ) -> Result<(), MacOsError> {
        if self
            .journal
            .mutation_state(&mutation)
            .map_err(|_| MacOsError::backend_failure())?
            == Some(crate::MacOsInstallMutationState::PreExisting)
        {
            return (presence == MacOsAssetPresence::ExactPresent)
                .then_some(())
                .ok_or_else(MacOsError::backend_failure);
        }
        if presence == MacOsAssetPresence::Absent {
            self.journal
                .intend(mutation)
                .map_err(|_| MacOsError::backend_failure())?;
            self.persist()?;
        }
        Ok(())
    }

    pub(super) fn complete(
        &mut self,
        mutation: MacOsInstallMutation,
        presence: MacOsAssetPresence,
        changed: bool,
    ) -> Result<(), MacOsError> {
        if self
            .journal
            .mutation_state(&mutation)
            .map_err(|_| MacOsError::backend_failure())?
            == Some(crate::MacOsInstallMutationState::PreExisting)
        {
            return (!changed && presence == MacOsAssetPresence::ExactPresent)
                .then_some(())
                .ok_or_else(MacOsError::backend_failure);
        }
        if changed != (presence == MacOsAssetPresence::Absent) {
            return Err(MacOsError::backend_failure());
        }
        if changed {
            self.journal
                .complete_created()
                .map_err(|_| MacOsError::backend_failure())?;
        } else {
            self.journal
                .record_preexisting(mutation)
                .map_err(|_| MacOsError::backend_failure())?;
        }
        self.persist()
    }

    pub(super) fn begin_replacement(
        &mut self,
        mutation: MacOsInstallMutation,
        prior_digest: Option<Digest>,
    ) -> Result<(), MacOsError> {
        self.journal
            .intend_replacement(mutation, prior_digest)
            .map_err(|_| MacOsError::backend_failure())?;
        self.persist()
    }

    pub(super) fn complete_replacement(&mut self, changed: bool) -> Result<(), MacOsError> {
        if changed {
            self.journal
                .complete_replaced()
                .map_err(|_| MacOsError::backend_failure())?;
        } else {
            self.journal
                .complete_unchanged_replacement()
                .map_err(|_| MacOsError::backend_failure())?;
        }
        self.persist()
    }

    pub(super) fn complete_rollback(
        &mut self,
        mutation: &MacOsInstallMutation,
    ) -> Result<(), MacOsError> {
        if self
            .journal
            .recovery_actions()
            .iter()
            .any(|action| match action {
                crate::MacOsInstallRecoveryAction::RevalidateIntended(current)
                | crate::MacOsInstallRecoveryAction::RevertCreated(current)
                | crate::MacOsInstallRecoveryAction::RollForwardReplaced(current)
                | crate::MacOsInstallRecoveryAction::RestoreReplaced(current, _) => {
                    *current == mutation
                }
            })
        {
            self.journal
                .complete_recovery_action(mutation)
                .map_err(|_| MacOsError::rollback_incomplete())?;
            self.persist()?;
        }
        Ok(())
    }

    pub(super) fn commit(&mut self) -> Result<(), MacOsError> {
        self.journal
            .commit()
            .map_err(|_| MacOsError::rollback_incomplete())?;
        self.storage
            .replace(&self.journal)
            .map_err(|_| MacOsError::rollback_incomplete())
    }

    pub(super) fn persist(&self) -> Result<(), MacOsError> {
        self.storage.replace(&self.journal)
    }
}

impl<P: BundleProvisioner> MacOsInstallBackend for MacOsBundleBackend<'_, '_, P> {
    fn install_mode(&self) -> crate::MacOsInstallMode {
        self.inner.install_mode()
    }

    fn preflight_product_mutation(&mut self) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()
    }
    fn bind_authenticated_installer_payloads(
        &mut self,
        payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<(), MacOsError> {
        self.inner.bind_authenticated_installer_payloads(payloads)
    }

    fn bind_authenticated_nix_config(
        &mut self,
        config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), MacOsError> {
        self.inner.bind_authenticated_nix_config(config)
    }

    fn bind_authenticated_release_identity(
        &mut self,
        system: System,
        release_identity_digest: Digest,
    ) -> Result<(), MacOsError> {
        self.inner
            .bind_authenticated_release_identity(system, release_identity_digest)
    }

    fn begin_authenticated_recovery(
        &mut self,
        mode: crate::MacOsInstallMode,
    ) -> Result<(), MacOsError> {
        self.inner.begin_authenticated_recovery(mode)
    }

    fn preflight_privilege(&mut self) -> Result<(), MacOsError> {
        self.inner.preflight_privilege()
    }
    fn preflight_clean_host(&mut self, system: System) -> Result<(), MacOsError> {
        self.inner.preflight_clean_host(system)
    }
    fn broker_uid(&mut self) -> Result<u32, MacOsError> {
        self.inner.broker_uid()
    }
    fn classify_asset(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<MacOsAssetPresence, MacOsError> {
        self.inner.classify_asset(asset)
    }
    fn classify_managed_runtime(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        self.inner.classify_managed_runtime()
    }
    fn classify_services(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        self.inner.classify_services()
    }
    fn classify_ownership_receipt(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        self.inner.classify_ownership_receipt()
    }
    fn recover_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.recover_asset(asset)
    }
    fn recover_services(&mut self) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.recover_services()
    }
    fn recover_ownership_receipt(&mut self) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.recover_ownership_receipt()?;
        if let Some(journal) = self.journal.as_mut() {
            journal.complete_rollback(&MacOsInstallMutation::OwnershipReceipt)?;
        }
        Ok(())
    }
    fn verify_release_bundle(&mut self) -> Result<(), MacOsError> {
        self.inner.verify_release_bundle()
    }
    fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        let mutation = macos_asset_mutation(asset);
        let observed = self.inner.classify_asset(asset)?;
        let presence = if self.store_created && asset.id() == "nix-root" {
            if observed != MacOsAssetPresence::ExactPresent {
                return Err(MacOsError::backend_failure());
            }
            MacOsAssetPresence::Absent
        } else {
            observed
        };
        self.inner.preflight_product_mutation()?;
        let replacing = self.install_mode() != crate::MacOsInstallMode::FreshInstall
            && asset.kind() == crate::MacOsAssetKind::File
            && presence == MacOsAssetPresence::ExactPresent;
        if replacing {
            let prior = self.inner.prior_file_digest(asset)?;
            self.journal
                .as_mut()
                .ok_or_else(MacOsError::backend_failure)?
                .begin_replacement(mutation.clone(), prior)?;
        } else {
            begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        }
        let changed = self.inner.ensure_asset(asset)?;
        if replacing {
            self.journal
                .as_mut()
                .ok_or_else(MacOsError::backend_failure)?
                .complete_replacement(changed)?;
        } else {
            complete_macos_mutation(&mut self.journal, mutation, presence, changed)?;
        }
        Ok(changed)
    }
    fn install_launchd_plist(
        &mut self,
        asset: MacOsInstallAsset,
        contents: &'static str,
    ) -> Result<bool, MacOsError> {
        let mutation = macos_asset_mutation(asset);
        let presence = self.inner.classify_asset(asset)?;
        self.inner.preflight_product_mutation()?;
        let replacing = self.install_mode() != crate::MacOsInstallMode::FreshInstall
            && presence == MacOsAssetPresence::ExactPresent;
        if replacing {
            let prior = self.inner.prior_file_digest(asset)?;
            self.journal
                .as_mut()
                .ok_or_else(MacOsError::backend_failure)?
                .begin_replacement(mutation.clone(), prior)?;
        } else {
            begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        }
        let changed = self.inner.install_launchd_plist(asset, contents)?;
        if replacing {
            self.journal
                .as_mut()
                .ok_or_else(MacOsError::backend_failure)?
                .complete_replacement(changed)?;
        } else {
            complete_macos_mutation(&mut self.journal, mutation, presence, changed)?;
        }
        Ok(changed)
    }
    fn install_nix_config(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        let mutation = macos_asset_mutation(asset);
        let presence = self.inner.classify_asset(asset)?;
        begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        let changed = self.inner.install_nix_config(asset)?;
        complete_macos_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn provision_managed_runtime(&mut self) -> Result<bool, MacOsError> {
        let mutation = MacOsInstallMutation::ManagedRuntime;
        let presence = self.inner.classify_managed_runtime()?;
        if presence == MacOsAssetPresence::ExactPresent
            && self
                .provisioner
                .reuse_existing()
                .map_err(macos_provision_error)?
        {
            self.outcome = Some(BootstrapOutcome::Existing);
            begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
            complete_macos_mutation(&mut self.journal, mutation, presence, false)?;
            return Ok(false);
        }
        self.provisioner
            .reauthenticate_macos(self.request, self.inner)
            .map_err(macos_provision_error)?;
        self.provisioner
            .preflight_workspace(self.request)
            .map_err(macos_provision_error)?;
        begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        self.outcome = Some(
            self.provisioner
                .provision(self.request)
                .map_err(macos_provision_error)?,
        );
        let changed = presence == MacOsAssetPresence::Absent;
        complete_macos_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn rollback_managed_runtime(&mut self) -> Result<(), MacOsError> {
        if self
            .outcome
            .as_ref()
            .is_some_and(BootstrapOutcome::has_accepted_base_nix)
        {
            if let Some(journal) = self.journal.as_mut() {
                journal.complete_rollback(&MacOsInstallMutation::ManagedRuntime)?;
            }
            return Ok(());
        }
        self.outcome
            .take()
            .map_or(Ok(()), BootstrapOutcome::rollback_macos)?;
        if let Some(journal) = self.journal.as_mut() {
            journal.complete_rollback(&MacOsInstallMutation::ManagedRuntime)?;
        }
        Ok(())
    }
    fn accept_base_nix_handoff(&mut self) -> Result<(), MacOsError> {
        match self.outcome.as_ref() {
            Some(BootstrapOutcome::DeterminatePending { handoff, .. }) => {
                handoff
                    .accept_after_installed_state_proof()
                    .map_err(|_| MacOsError::backend_failure())?;
            }
            #[cfg(test)]
            Some(BootstrapOutcome::DeterminateTestPending(handoff)) => {
                handoff
                    .accept_after_installed_state_proof()
                    .map_err(|_| MacOsError::backend_failure())?;
            }
            Some(BootstrapOutcome::Existing) => {}
            #[cfg(test)]
            Some(BootstrapOutcome::Stub(_)) => {}
            Some(_) | None => return Err(MacOsError::backend_failure()),
        }
        // The inner backend owns the preexisting-asset policy. A handoff
        // accepted during this run must let the vendor-created /nix pass the
        // post-acceptance nix-root classification.
        self.inner.accept_base_nix_handoff()
    }
    fn verify_installed_code(&mut self) -> Result<(), MacOsError> {
        self.inner.verify_installed_code()
    }
    fn activate_services(&mut self) -> Result<bool, MacOsError> {
        let mutation = MacOsInstallMutation::Services;
        let presence = self.inner.classify_services()?;
        begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        let changed = self.inner.activate_services()?;
        complete_macos_mutation(&mut self.journal, mutation, presence, changed)?;
        Ok(changed)
    }
    fn rollback_services(&mut self) -> Result<(), MacOsError> {
        self.inner.rollback_services()?;
        if let Some(journal) = self.journal.as_mut() {
            journal.complete_rollback(&MacOsInstallMutation::Services)?;
        }
        Ok(())
    }
    fn check_managed_daemon(&mut self) -> Result<(), MacOsError> {
        self.inner.check_managed_daemon()
    }
    fn observe_build_readiness(
        &mut self,
        system: System,
    ) -> Result<MacOsBuildReadiness, MacOsError> {
        self.inner.observe_build_readiness(system)
    }
    #[allow(
        clippy::too_many_lines,
        reason = "one receipt publication walks a closed journal mutation sequence"
    )]
    fn publish_ownership_receipt(&mut self) -> Result<bool, MacOsError> {
        let mutation = MacOsInstallMutation::OwnershipReceipt;
        let presence = self.inner.classify_ownership_receipt()?;
        self.inner.preflight_product_mutation()?;
        let replacing = self.install_mode() != crate::MacOsInstallMode::FreshInstall
            && presence == MacOsAssetPresence::ExactPresent;
        if replacing {
            let prior = self.inner.prior_ownership_receipt_digest()?;
            self.journal
                .as_mut()
                .ok_or_else(MacOsError::backend_failure)?
                .begin_replacement(mutation.clone(), prior)?;
        } else {
            begin_macos_mutation(&mut self.journal, mutation.clone(), presence)?;
        }
        let outcome = self
            .outcome
            .take()
            .ok_or_else(MacOsError::backend_failure)?;
        match outcome {
            BootstrapOutcome::DeterminatePending {
                mut bundle,
                handoff,
            } => {
                let receipt_created = match self.inner.publish_ownership_receipt() {
                    Ok(created) => created,
                    Err(error) => {
                        self.outcome =
                            Some(BootstrapOutcome::DeterminatePending { bundle, handoff });
                        return Err(error);
                    }
                };
                if bundle.commit_authenticated_channel().is_err() {
                    self.outcome = Some(BootstrapOutcome::DeterminatePending { bundle, handoff });
                    return Err(MacOsError::backend_failure());
                }
                if let Err(error) = complete_macos_receipt(
                    &mut self.journal,
                    mutation,
                    presence,
                    receipt_created,
                    replacing,
                ) {
                    self.outcome = Some(BootstrapOutcome::DeterminatePending { bundle, handoff });
                    return Err(error);
                }
                self.outcome = Some(BootstrapOutcome::DeterminateComplete);
                Ok(receipt_created)
            }
            #[cfg(test)]
            BootstrapOutcome::Stub(rolled_back) => {
                let result = self.inner.publish_ownership_receipt();
                self.outcome = Some(BootstrapOutcome::Stub(rolled_back));
                let changed = result?;
                complete_macos_receipt(&mut self.journal, mutation, presence, changed, replacing)?;
                Ok(changed)
            }
            BootstrapOutcome::DeterminateComplete => Err(MacOsError::backend_failure()),
            #[cfg(test)]
            BootstrapOutcome::DeterminateTestPending(handoff) => {
                let receipt_created = match self.inner.publish_ownership_receipt() {
                    Ok(created) => created,
                    Err(error) => {
                        self.outcome = Some(BootstrapOutcome::DeterminateTestPending(handoff));
                        return Err(error);
                    }
                };
                if let Err(error) = complete_macos_receipt(
                    &mut self.journal,
                    mutation,
                    presence,
                    receipt_created,
                    replacing,
                ) {
                    self.outcome = Some(BootstrapOutcome::DeterminateTestPending(handoff));
                    return Err(error);
                }
                self.outcome = Some(BootstrapOutcome::DeterminateComplete);
                Ok(receipt_created)
            }
            BootstrapOutcome::Existing => {
                // Match the Linux Existing arm: the accepted channel state is
                // already committed, so a repeat install only verifies and
                // reuses the exact ownership receipt.
                let changed = match self.inner.publish_ownership_receipt() {
                    Ok(changed) => changed,
                    Err(error) => {
                        self.outcome = Some(BootstrapOutcome::Existing);
                        return Err(error);
                    }
                };
                self.outcome = Some(BootstrapOutcome::Existing);
                complete_macos_receipt(&mut self.journal, mutation, presence, changed, replacing)?;
                Ok(changed)
            }
        }
    }
    fn rollback_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.rollback_asset(asset)?;
        if let Some(journal) = self.journal.as_mut() {
            journal.complete_rollback(&macos_asset_mutation(asset))?;
        }
        Ok(())
    }

    fn prior_file_digest(
        &mut self,
        asset: MacOsInstallAsset,
    ) -> Result<Option<Digest>, MacOsError> {
        self.inner.prior_file_digest(asset)
    }

    fn recover_replaced_asset(
        &mut self,
        asset: MacOsInstallAsset,
        prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.recover_replaced_asset(asset, prior_digest)
    }

    fn roll_forward_replaced_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.roll_forward_replaced_asset(asset)
    }

    fn finalize_replaced_asset(&mut self, asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.finalize_replaced_asset(asset)
    }

    fn prior_ownership_receipt_digest(&mut self) -> Result<Option<Digest>, MacOsError> {
        self.inner.prior_ownership_receipt_digest()
    }

    fn recover_replaced_ownership_receipt(
        &mut self,
        prior_digest: Digest,
    ) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.recover_replaced_ownership_receipt(prior_digest)
    }

    fn roll_forward_replaced_ownership_receipt(&mut self) -> Result<(), MacOsError> {
        self.inner.preflight_product_mutation()?;
        self.inner.roll_forward_replaced_ownership_receipt()
    }

    fn classify_store_volume(&mut self) -> Result<MacOsAssetPresence, MacOsError> {
        self.inner.classify_store_volume()
    }
    fn provision_store_volume(&mut self) -> Result<bool, MacOsError> {
        self.inner.provision_store_volume()
    }
    fn rollback_store_volume(&mut self) -> Result<(), MacOsError> {
        self.inner.rollback_store_volume()
    }
    fn recover_store_volume(&mut self) -> Result<(), MacOsError> {
        self.inner.recover_store_volume()
    }
}

pub(super) fn complete_macos_receipt(
    journal: &mut Option<MacOsJournalTransaction<'_>>,
    mutation: MacOsInstallMutation,
    presence: MacOsAssetPresence,
    changed: bool,
    replacing: bool,
) -> Result<(), MacOsError> {
    if replacing {
        journal
            .as_mut()
            .ok_or_else(MacOsError::backend_failure)?
            .complete_replacement(changed)
    } else {
        complete_macos_mutation(journal, mutation, presence, changed)
    }
}

pub(super) fn macos_asset_mutation(asset: MacOsInstallAsset) -> MacOsInstallMutation {
    MacOsInstallMutation::Asset {
        id: asset.id().to_owned(),
    }
}

pub(super) fn begin_macos_mutation(
    journal: &mut Option<MacOsJournalTransaction<'_>>,
    mutation: MacOsInstallMutation,
    presence: MacOsAssetPresence,
) -> Result<(), MacOsError> {
    journal
        .as_mut()
        .map_or(Ok(()), |journal| journal.begin(mutation, presence))
}

pub(super) fn complete_macos_mutation(
    journal: &mut Option<MacOsJournalTransaction<'_>>,
    mutation: MacOsInstallMutation,
    presence: MacOsAssetPresence,
    changed: bool,
) -> Result<(), MacOsError> {
    journal.as_mut().map_or(Ok(()), |journal| {
        journal.complete(mutation, presence, changed)
    })
}

pub(super) const fn linux_provision_error(error: BundleProvisionError) -> InstallError {
    match error {
        BundleProvisionError::Failed => InstallError::backend_failure(),
        BundleProvisionError::RollbackIncomplete => InstallError::rollback_incomplete(),
    }
}

pub(super) const fn macos_provision_error(error: BundleProvisionError) -> MacOsError {
    match error {
        BundleProvisionError::Failed => MacOsError::backend_failure(),
        BundleProvisionError::RollbackIncomplete => MacOsError::rollback_incomplete(),
    }
}

#[cfg(test)]
pub(super) fn install_macos_with_provisioner<'a, P: BundleProvisioner>(
    system: System,
    request: &'a InstallerProvisionRequest<'a>,
    backend: &'a mut dyn MacOsInstallBackend,
    provisioner: &'a mut P,
) -> Result<(MacOsInstallReport, BootstrapOutcome), MacOsError> {
    let mut adapter = MacOsBundleBackend {
        inner: backend,
        request,
        provisioner,
        outcome: None,
        journal: None,
        store_created: false,
    };
    let report = install_macos(system, &mut adapter)?;
    let outcome = adapter
        .outcome
        .take()
        .ok_or_else(MacOsError::backend_failure)?;
    Ok((report, outcome))
}

pub(super) fn install_macos_with_provisioner_journaled<'a, P: BundleProvisioner>(
    system: System,
    request: &'a InstallerProvisionRequest<'a>,
    backend: &'a mut dyn MacOsInstallBackend,
    provisioner: &'a mut P,
    storage: &dyn MacOsJournalPersistence,
    journal: MacOsInstallJournal,
) -> Result<(MacOsInstallReport, BootstrapOutcome), MacOsError> {
    let mut adapter = MacOsBundleBackend {
        inner: backend,
        request,
        provisioner,
        outcome: None,
        journal: Some(MacOsJournalTransaction { storage, journal }),
        store_created: false,
    };
    let mode = adapter
        .journal
        .as_ref()
        .ok_or_else(MacOsError::rollback_incomplete)?
        .journal
        .mode();
    let report = match install_macos(system, &mut adapter) {
        Ok(report) => report,
        Err(_) if mode == crate::MacOsInstallMode::OfflineRepair => {
            return Err(MacOsError::rollback_incomplete());
        }
        Err(_)
            if mode == crate::MacOsInstallMode::FreshInstall
                && adapter
                    .outcome
                    .as_ref()
                    .is_some_and(BootstrapOutcome::has_accepted_base_nix) =>
        {
            return Err(MacOsError::rollback_incomplete());
        }
        Err(error) => return Err(error),
    };
    adapter
        .journal
        .as_mut()
        .ok_or_else(MacOsError::rollback_incomplete)?
        .commit()?;
    for asset in crate::macos_product_install_assets()
        .filter(|asset| asset.kind() == crate::MacOsAssetKind::File)
    {
        adapter
            .inner
            .finalize_replaced_asset(asset)
            .map_err(|_| MacOsError::rollback_incomplete())?;
    }
    let outcome = adapter
        .outcome
        .take()
        .ok_or_else(MacOsError::backend_failure)?;
    Ok((report, outcome))
}
