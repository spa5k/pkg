//! Production command adapter over the invoking user's verified local state.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::broker::{BrokerClientError, BrokerClientErrorCode, BrokerLifecycleClient};
use crate::cli::{
    CollisionPolicy, GcArgs, HistoryArgs, InfoArgs, InstallArgs, ListArgs, PackageArgs, RemoveArgs,
    RepairArgs, RollbackArgs, SearchArgs, UpdateArgs, UpgradeArgs,
};
use crate::commands::execute::{CommandResult, CoreOperations, OperationPolicy};
use crate::commands::state::{
    LifecycleEdit, edit_pin_state, list_state, read_history, remove_state, rollback_state,
};
use crate::exit::ExitCode;
use crate::progress::PublicEvent;
use crate::ux::CommandError;
use pkg_core::state::CollisionPolicy as StateCollisionPolicy;
use pkg_core::{
    History, OutputName, OutputSelection, PackageSelector, PinAction, SelectorId, SelectorInput,
    SourceRevision, VersionPreference,
};
use pkg_nix::{
    ApprovalSource, BrokerOperationKind, CacheInstallErrorCode, CacheInstallOutcome, Digest,
    GenerationRootAttestationErrorCode, InstallEvidence, MaintenanceAdapter, MaintenanceError,
    OperationHandle, RemoveRootSetRequest, RepairStorePathsReport, RepairStorePathsRequest,
    RootSet, RootSetAttestationRequest, RootSetReport,
};
use pkg_pipeline::{
    CommitError, InstallGenerationError, InstallGenerationMetadata, StateEditKind,
    StateEditMetadata, discard_unprepared_installs, discard_unprepared_state_edits,
    load_active_snapshot, load_retained_history, pending_install_generation,
    pending_state_edit_generation, pending_state_transition_source, prepare_install_generation,
    prepare_rollback, prepare_state_edit, recover_generation, recover_transitioned_state_edit,
    resume_prepared_install, resume_prepared_state_edit,
};
use pkg_store::{
    GcError, GcPolicy, LeaseError, LeaseIdentity, PruneOutcome, StateLayout, StateLease, plan_gc,
    plan_generation_prune, prune_generation, recover_prunes,
};
use serde_json::{Map, json};

const DEFAULT_KEEP_GENERATIONS: usize = 10;
const DEFAULT_MAX_AGE_DAYS: u64 = 30;

struct BrokerGcMaintenance<'a> {
    broker: Mutex<&'a mut BrokerLifecycleClient>,
    handle: OperationHandle,
}

struct AttestedRootMaintenance {
    report: RootSetReport,
}

impl MaintenanceAdapter for AttestedRootMaintenance {
    fn publish_root_set(&self, _root_set: &RootSet) -> Result<RootSetReport, MaintenanceError> {
        Ok(self.report.clone())
    }

    fn attest_root_set(
        &self,
        _request: &RootSetAttestationRequest,
    ) -> Result<RootSetReport, MaintenanceError> {
        Ok(self.report.clone())
    }

    fn remove_root_set(&self, _request: &RemoveRootSetRequest) -> Result<(), MaintenanceError> {
        Err(MaintenanceError::backend_failure())
    }

    fn repair_store_paths(
        &self,
        _request: &RepairStorePathsRequest,
    ) -> Result<RepairStorePathsReport, MaintenanceError> {
        Err(MaintenanceError::backend_failure())
    }
}

impl MaintenanceAdapter for BrokerGcMaintenance<'_> {
    fn publish_root_set(&self, _root_set: &RootSet) -> Result<RootSetReport, MaintenanceError> {
        Err(MaintenanceError::backend_failure())
    }

    fn attest_root_set(
        &self,
        _request: &RootSetAttestationRequest,
    ) -> Result<RootSetReport, MaintenanceError> {
        Err(MaintenanceError::backend_failure())
    }

    fn remove_root_set(&self, request: &RemoveRootSetRequest) -> Result<(), MaintenanceError> {
        self.broker
            .lock()
            .map_err(|_| MaintenanceError::backend_failure())?
            .remove_generation_roots(self.handle.clone(), request.generation().clone())
            .map_err(|_| MaintenanceError::backend_failure())
    }

    fn repair_store_paths(
        &self,
        _request: &RepairStorePathsRequest,
    ) -> Result<RepairStorePathsReport, MaintenanceError> {
        Err(MaintenanceError::backend_failure())
    }
}

/// Shipped command operations backed by one ownership-validated user state.
///
/// Read-only state, cache-backed install, state-only generation edits,
/// rollback, and GC are live. Local-build fallback, upgrade/repair, and
/// authenticated-index commands remain explicit closed refusals until their
/// transaction coordinators are connected; no command can fall through to raw
/// Nix access.
#[derive(Debug)]
pub struct LocalStateOperations {
    source: Result<StateLayout, CommandError>,
    broker_state_compatible: bool,
}

impl LocalStateOperations {
    /// Opens a state root beneath the caller's trusted home boundary.
    #[must_use]
    pub fn open(trusted_home: &Path, state_root: &Path, owner_uid: u32) -> Self {
        let broker_state_compatible =
            production_state_root(trusted_home).is_some_and(|production| production == state_root);
        let source = StateLayout::initialize(trusted_home, state_root, owner_uid).map_err(|_| {
            CommandError::new(
                ExitCode::StateCorrupt,
                "the per-user package state is missing or unsafe",
                "run `pkg doctor` before managing packages",
            )
        });
        Self {
            source,
            broker_state_compatible,
        }
    }

    fn layout(&self) -> Result<&StateLayout, CommandError> {
        self.source.as_ref().map_err(Clone::clone)
    }

    fn active(&self) -> Result<pkg_core::GenerationSnapshot, CommandError> {
        let layout = self.layout()?;
        let lease = StateLease::try_shared(layout).map_err(state_lease_error)?;
        load_active_snapshot(layout, &lease)
            .map_err(state_read_error)?
            .ok_or_else(|| {
                CommandError::new(
                    ExitCode::ResolveFailed,
                    "no package generation is active",
                    "install a package before using this command",
                )
            })
    }

    fn history_view(&self) -> Result<History, CommandError> {
        let layout = self.layout()?;
        let lease = StateLease::try_shared(layout).map_err(state_lease_error)?;
        load_retained_history(layout, &lease).map_err(state_read_error)
    }

    fn require_broker_state(&self) -> Result<(), CommandError> {
        if self.broker_state_compatible {
            Ok(())
        } else {
            Err(CommandError::new(
                ExitCode::Config,
                "broker-backed mutations require the fixed production state root",
                "remove `--state` and PKG_STATE_DIR, then retry the mutation",
            ))
        }
    }
}

impl CoreOperations for LocalStateOperations {
    fn search(&mut self, _args: &SearchArgs) -> Result<CommandResult, CommandError> {
        Err(index_unavailable())
    }

    fn info(&mut self, _args: &InfoArgs) -> Result<CommandResult, CommandError> {
        Err(index_unavailable())
    }

    fn install(
        &mut self,
        args: &InstallArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.install_packages(args, policy, &mut |_| Ok(()))
    }

    fn install_with_progress(
        &mut self,
        args: &InstallArgs,
        policy: OperationPolicy,
        progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
    ) -> Result<CommandResult, CommandError> {
        self.install_packages(args, policy, progress)
    }

    fn remove(
        &mut self,
        args: &RemoveArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        if policy.dry_run() {
            return Ok(remove_state(self.active()?.state().clone(), args)?
                .into_parts()
                .1);
        }
        require_confirmation(
            policy,
            &format!("Remove {} package(s)?", args.packages().len()),
        )?;
        self.commit_state_edit(StateEditKind::Remove, |state| remove_state(state, args))
    }

    fn list(&mut self, args: &ListArgs) -> Result<CommandResult, CommandError> {
        let active = self.active()?;
        list_state(active.state(), args, None)
    }

    fn outdated(&mut self) -> Result<CommandResult, CommandError> {
        Err(index_unavailable())
    }

    fn update(
        &mut self,
        _args: &UpdateArgs,
        _policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        Err(index_unavailable())
    }

    fn upgrade(
        &mut self,
        _args: &UpgradeArgs,
        _policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        Err(mutation_unavailable())
    }

    fn pin(
        &mut self,
        args: &PackageArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.edit_pin(args, policy, PinAction::Pin)
    }

    fn unpin(
        &mut self,
        args: &PackageArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.edit_pin(args, policy, PinAction::Unpin)
    }

    fn history(
        &mut self,
        args: &HistoryArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        if args.delete().is_some() {
            return if policy.dry_run() {
                self.preview_history_delete(args)
            } else {
                self.commit_history_delete(args, policy)
            };
        }
        read_history(&self.history_view()?, args)
    }

    fn rollback(
        &mut self,
        args: &RollbackArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        if policy.dry_run() {
            let layout = self.layout()?;
            let lease = StateLease::try_shared(layout).map_err(state_lease_error)?;
            let active = load_active_snapshot(layout, &lease)
                .map_err(state_read_error)?
                .ok_or_else(no_active_generation)?;
            let history = load_retained_history(layout, &lease).map_err(state_read_error)?;
            return Ok(rollback_state(&active, &history, args)?.into_parts().1);
        }
        self.commit_rollback(args)
    }

    fn gc(
        &mut self,
        args: &GcArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        if policy.dry_run() {
            self.preview_gc(args)
        } else {
            self.commit_gc(args, policy)
        }
    }

    fn repair(
        &mut self,
        _args: &RepairArgs,
        _policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        Err(mutation_unavailable())
    }
}

impl LocalStateOperations {
    fn install_packages(
        &self,
        args: &InstallArgs,
        policy: OperationPolicy,
        progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
    ) -> Result<CommandResult, CommandError> {
        self.require_broker_state()?;
        require_supported_install_options(args)?;
        let layout = self.layout()?.clone();
        let nonce = secure_nonce()?;
        let selectors = install_selectors(args, &nonce)?;
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;

        if policy.dry_run() {
            return preview_install(&mut broker, selectors);
        }
        self.recover_pending_install(&layout, &mut broker)?;

        let (handle, public_operation_id, evidence, build_approval) =
            acquire_install_evidence(&mut broker, selectors, policy, progress)?;
        let mut local_committed = false;
        let result = (|| {
            emit_phase(progress, &public_operation_id, "stage", "started")?;
            let created_at = utc_now()?;
            let identity = LeaseIdentity::new(handle.as_str(), &nonce, &created_at)
                .map_err(state_lease_error)?;
            let lease = StateLease::try_exclusive(&layout, &identity).map_err(state_lease_error)?;
            let current = load_active_snapshot(&layout, &lease).map_err(state_read_error)?;
            let generation_id = match &current {
                None => "gen-0001".to_owned(),
                Some(_) => {
                    let history =
                        load_retained_history(&layout, &lease).map_err(state_read_error)?;
                    let newest = history.snapshots().first().ok_or_else(mutation_failed)?;
                    next_generation_id(newest.generation().id())?
                }
            };
            let prepared = prepare_install_generation(
                layout.clone(),
                lease,
                current.as_ref(),
                &evidence,
                layout.owner_uid(),
                state_collision_policy(args.collision_policy()),
                InstallGenerationMetadata::new(
                    &generation_id,
                    &created_at,
                    handle.as_str(),
                    build_approval,
                ),
            )
            .map_err(map_install_generation_error)?;
            emit_phase(progress, &public_operation_id, "stage", "completed")?;
            emit_phase(progress, &public_operation_id, "activate", "started")?;
            let report = prepared
                .root_intent()
                .map_err(|_| install_commit_failed())?
                .map(|intent| {
                    broker
                        .publish_build_roots(handle.clone(), intent)
                        .map_err(install_broker_error)
                })
                .transpose()?;
            prepared
                .activate_published(report.as_ref(), &nonce)
                .map_err(|_| install_commit_failed())?
                .finish()
                .map_err(|_| install_commit_failed())?;
            local_committed = true;
            let _ = broker.complete(handle.clone());
            let _ = emit_phase(progress, &public_operation_id, "activate", "completed");
            if let Ok(event) = PublicEvent::committed(&public_operation_id, &generation_id) {
                let _ = progress(event);
            }
            install_result(
                &public_operation_id,
                &generation_id,
                current.as_ref(),
                &evidence,
            )
        })();
        if result.is_err() && !local_committed {
            let _ = broker.cancel(handle);
        }
        result
    }

    fn recover_pending_install(
        &self,
        layout: &StateLayout,
        broker: &mut BrokerLifecycleClient,
    ) -> Result<(), CommandError> {
        let nonce = secure_nonce()?;
        let created_at = utc_now()?;
        let identity = LeaseIdentity::new("recover_install", &nonce, &created_at)
            .map_err(state_lease_error)?;
        let lease = StateLease::try_exclusive(layout, &identity).map_err(state_lease_error)?;
        discard_unprepared_installs(layout, &lease).map_err(state_read_error)?;
        let Some(pending) = pending_install_generation(layout, &lease).map_err(state_read_error)?
        else {
            return Ok(());
        };
        let current = layout.current_generation().map_err(|_| mutation_failed())?;
        let handle = broker
            .begin(BrokerOperationKind::Activate)
            .map_err(broker_error)?;
        if current.as_ref() == Some(&pending) {
            let report = broker
                .attest_generation_roots(handle.clone(), pending.clone())
                .map_err(install_broker_error)?;
            let maintenance = AttestedRootMaintenance { report };
            recover_generation(layout, &lease, &pending, &maintenance)
                .map_err(|_| install_commit_failed())?;
            let _ = broker.complete(handle);
            return Ok(());
        }
        let prepared = resume_prepared_install(layout.clone(), lease, &pending)
            .map_err(|_| install_commit_failed())?;
        let report = match broker.attest_generation_roots(handle.clone(), pending.clone()) {
            Ok(report) => report,
            Err(error)
                if error.generation_root_attestation_code()
                    == Some(GenerationRootAttestationErrorCode::AttestationFailed) =>
            {
                drop(prepared);
                let _ = broker.cancel(handle);
                self.discard_unrooted_install(layout, broker, &pending)?;
                return Ok(());
            }
            Err(error) => return Err(install_broker_error(error)),
        };
        prepared
            .activate_published(Some(&report), &nonce)
            .map_err(|_| install_commit_failed())?
            .finish()
            .map_err(|_| install_commit_failed())?;
        let _ = broker.complete(handle);
        Ok(())
    }

    fn discard_unrooted_install(
        &self,
        layout: &StateLayout,
        broker: &mut BrokerLifecycleClient,
        generation: &pkg_nix::GenerationId,
    ) -> Result<(), CommandError> {
        let handle = broker
            .begin(BrokerOperationKind::Gc)
            .map_err(broker_error)?;
        let result = (|| {
            let lease = self.gc_lease(layout, &handle)?;
            let maintenance = BrokerGcMaintenance {
                broker: Mutex::new(&mut *broker),
                handle: handle.clone(),
            };
            recover_generation(layout, &lease, generation, &maintenance)
                .map_err(|_| install_commit_failed())?;
            drop(maintenance);
            let _ = broker.complete(handle.clone());
            Ok(())
        })();
        if result.is_err() {
            let _ = broker.cancel(handle);
        }
        result
    }

    fn preview_history_delete(&self, args: &HistoryArgs) -> Result<CommandResult, CommandError> {
        let generation = args.delete().ok_or_else(mutation_failed)?;
        let layout = self.layout()?;
        let lease = StateLease::try_shared(layout).map_err(state_lease_error)?;
        let active = load_active_snapshot(layout, &lease)
            .map_err(state_read_error)?
            .ok_or_else(no_active_generation)?;
        let history = load_retained_history(layout, &lease).map_err(state_read_error)?;
        ensure_generation_deletable(&active, &history, generation)?;
        let candidate =
            plan_generation_prune(&active, history.snapshots(), generation, unix_now()?)
                .map_err(|_| gc_failed())?;
        generation_prune_result(candidate.generation_id(), true)
    }

    fn commit_history_delete(
        &self,
        args: &HistoryArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.require_broker_state()?;
        let generation = args.delete().ok_or_else(mutation_failed)?;
        let layout = self.layout()?.clone();
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;
        let recovered = self.recover_pending_prunes(&layout, &mut broker)?;
        self.recover_pending_state_edit(&layout, &mut broker)?;
        if recovered.iter().any(|id| id == generation) {
            return generation_prune_result(generation, false);
        }
        let handle = broker
            .begin(BrokerOperationKind::Gc)
            .map_err(broker_error)?;
        let result = (|| {
            let lease = self.gc_lease(&layout, &handle)?;
            let active = load_active_snapshot(&layout, &lease)
                .map_err(state_read_error)?
                .ok_or_else(no_active_generation)?;
            let history = load_retained_history(&layout, &lease).map_err(state_read_error)?;
            ensure_generation_deletable(&active, &history, generation)?;
            let candidate =
                plan_generation_prune(&active, history.snapshots(), generation, unix_now()?)
                    .map_err(|_| gc_failed())?;
            require_confirmation(policy, &format!("Prune generation {generation}?"))?;
            broker.acquire_gc(handle.clone()).map_err(broker_error)?;
            let maintenance = BrokerGcMaintenance {
                broker: Mutex::new(&mut broker),
                handle: handle.clone(),
            };
            prune_generation(&layout, &lease, &candidate, &maintenance, handle.as_str())
                .map_err(gc_error)?;
            drop(maintenance);
            let _ = broker.complete(handle.clone());
            generation_prune_result(candidate.generation_id(), false)
        })();
        if result.is_err() {
            let _ = broker.cancel(handle);
        }
        result
    }

    fn preview_gc(&self, args: &GcArgs) -> Result<CommandResult, CommandError> {
        let layout = self.layout()?;
        let lease = StateLease::try_shared(layout).map_err(state_lease_error)?;
        let active = load_active_snapshot(layout, &lease)
            .map_err(state_read_error)?
            .ok_or_else(no_active_generation)?;
        let history = load_retained_history(layout, &lease).map_err(state_read_error)?;
        let plan = plan_gc(&active, history.snapshots(), gc_policy(args)?, unix_now()?)
            .map_err(|_| gc_failed())?;
        gc_preview_result(&plan)
    }

    fn commit_gc(
        &self,
        args: &GcArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.require_broker_state()?;
        let layout = self.layout()?.clone();
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;
        let recovered = self.recover_pending_prunes(&layout, &mut broker)?;
        self.recover_pending_state_edit(&layout, &mut broker)?;
        let handle = broker
            .begin(BrokerOperationKind::Gc)
            .map_err(broker_error)?;
        let result = (|| {
            let lease = self.gc_lease(&layout, &handle)?;
            let active = load_active_snapshot(&layout, &lease)
                .map_err(state_read_error)?
                .ok_or_else(no_active_generation)?;
            let history = load_retained_history(&layout, &lease).map_err(state_read_error)?;
            let plan = plan_gc(&active, history.snapshots(), gc_policy(args)?, unix_now()?)
                .map_err(|_| gc_failed())?;
            require_gc_confirmation(policy, &plan)?;
            broker.acquire_gc(handle.clone()).map_err(broker_error)?;
            let maintenance = BrokerGcMaintenance {
                broker: Mutex::new(&mut broker),
                handle: handle.clone(),
            };
            let mut pruned = Vec::new();
            for candidate in plan.candidates() {
                if prune_generation(&layout, &lease, candidate, &maintenance, handle.as_str())
                    .map_err(gc_error)?
                    == PruneOutcome::Pruned
                {
                    pruned.push(candidate.generation_id().to_owned());
                }
            }
            drop(maintenance);
            let report = broker.gc(handle.clone()).map_err(broker_error)?;
            let _ = broker.complete(handle.clone());
            gc_run_result(&pruned, &recovered, &report)
        })();
        if result.is_err() {
            let _ = broker.cancel(handle);
        }
        result
    }

    fn gc_lease(
        &self,
        layout: &StateLayout,
        handle: &OperationHandle,
    ) -> Result<StateLease, CommandError> {
        let nonce = secure_nonce()?;
        let created_at = utc_now()?;
        let identity =
            LeaseIdentity::new(handle.as_str(), &nonce, &created_at).map_err(state_lease_error)?;
        StateLease::try_exclusive(layout, &identity).map_err(state_lease_error)
    }

    fn commit_rollback(&self, args: &RollbackArgs) -> Result<CommandResult, CommandError> {
        self.require_broker_state()?;
        let layout = self.layout()?.clone();
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;
        let _ = self.recover_pending_prunes(&layout, &mut broker)?;
        self.recover_pending_state_edit(&layout, &mut broker)?;
        let handle = broker
            .begin(BrokerOperationKind::Activate)
            .map_err(broker_error)?;
        let mut local_committed = false;
        let result = (|| {
            let nonce = secure_nonce()?;
            let created_at = utc_now()?;
            let identity = LeaseIdentity::new(handle.as_str(), &nonce, &created_at)
                .map_err(state_lease_error)?;
            let lease = StateLease::try_exclusive(&layout, &identity).map_err(state_lease_error)?;
            let source = load_active_snapshot(&layout, &lease)
                .map_err(state_read_error)?
                .ok_or_else(no_active_generation)?;
            let history = load_retained_history(&layout, &lease).map_err(state_read_error)?;
            let newest = history
                .snapshots()
                .first()
                .ok_or_else(no_active_generation)?;
            let generation_id = next_generation_id(newest.generation().id())?;
            let (plan, command_result) = rollback_state(&source, &history, args)?.into_parts();
            // Rollback derives its destination from the retained target's
            // durable roots, not necessarily from the active generation. In
            // particular, an active empty generation has no helper root set.
            let transition_source = pkg_nix::GenerationId::new(plan.target().generation().id())
                .map_err(|_| mutation_failed())?;
            let prepared = prepare_rollback(
                layout.clone(),
                lease,
                &plan,
                &generation_id,
                &created_at,
                handle.as_str(),
            )
            .map_err(|_| mutation_failed())?;
            let intent = prepared
                .root_transition_intent(transition_source)
                .map_err(|_| mutation_failed())?;
            let report = intent
                .map(|intent| {
                    broker
                        .transition_generation_roots(handle.clone(), intent)
                        .map_err(broker_error)
                })
                .transpose()?;
            prepared
                .activate_transitioned(report.as_ref(), &nonce)
                .map_err(|_| mutation_failed())?
                .finish()
                .map_err(|_| mutation_failed())?;
            local_committed = true;
            let _ = broker.complete(handle.clone());
            Ok(command_result)
        })();
        if result.is_err() && !local_committed {
            let _ = broker.cancel(handle);
        }
        result
    }

    fn edit_pin(
        &self,
        args: &PackageArgs,
        policy: OperationPolicy,
        action: PinAction,
    ) -> Result<CommandResult, CommandError> {
        if policy.dry_run() {
            return Ok(
                edit_pin_state(self.active()?.state().clone(), args, action)?
                    .into_parts()
                    .1,
            );
        }
        let kind = match action {
            PinAction::Pin => StateEditKind::Pin,
            PinAction::Unpin => StateEditKind::Unpin,
        };
        self.commit_state_edit(kind, |state| edit_pin_state(state, args, action))
    }

    fn commit_state_edit(
        &self,
        kind: StateEditKind,
        edit: impl FnOnce(pkg_core::lifecycle::LifecycleState) -> Result<LifecycleEdit, CommandError>,
    ) -> Result<CommandResult, CommandError> {
        self.require_broker_state()?;
        let layout = self.layout()?.clone();
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;
        let _ = self.recover_pending_prunes(&layout, &mut broker)?;
        self.recover_pending_state_edit(&layout, &mut broker)?;
        let handle = broker
            .begin(BrokerOperationKind::Activate)
            .map_err(broker_error)?;
        let mut local_committed = false;
        let result = (|| {
            let nonce = secure_nonce()?;
            let created_at = utc_now()?;
            let identity = LeaseIdentity::new(handle.as_str(), &nonce, &created_at)
                .map_err(state_lease_error)?;
            let lease = StateLease::try_exclusive(&layout, &identity).map_err(state_lease_error)?;
            let source = load_active_snapshot(&layout, &lease)
                .map_err(state_read_error)?
                .ok_or_else(no_active_generation)?;
            let history = load_retained_history(&layout, &lease).map_err(state_read_error)?;
            let newest = history
                .snapshots()
                .first()
                .ok_or_else(no_active_generation)?;
            let source_generation = pkg_nix::GenerationId::new(source.generation().id())
                .map_err(|_| mutation_failed())?;
            let generation_id = next_generation_id(newest.generation().id())?;
            let (next, command_result) = edit(source.state().clone())?.into_parts();
            let prepared = prepare_state_edit(
                layout.clone(),
                lease,
                &source,
                next,
                StateEditMetadata::new(&generation_id, &created_at, handle.as_str(), kind),
            )
            .map_err(|_| mutation_failed())?;
            let intent = prepared
                .root_transition_intent(source_generation)
                .map_err(|_| mutation_failed())?;
            let report = intent
                .map(|intent| {
                    broker
                        .transition_generation_roots(handle.clone(), intent)
                        .map_err(broker_error)
                })
                .transpose()?;
            prepared
                .activate_transitioned(report.as_ref(), &nonce)
                .map_err(|_| mutation_failed())?
                .finish()
                .map_err(|_| mutation_failed())?;
            local_committed = true;
            // The local generation switch is the linearization point. A lost
            // completion acknowledgement must not turn an applied edit into a
            // retryable failure or authorize cancellation of its roots.
            let _ = broker.complete(handle.clone());
            Ok(command_result)
        })();
        if result.is_err() && !local_committed {
            let _ = broker.cancel(handle);
        }
        result
    }

    fn recover_pending_state_edit(
        &self,
        layout: &StateLayout,
        broker: &mut BrokerLifecycleClient,
    ) -> Result<(), CommandError> {
        let nonce = secure_nonce()?;
        let created_at = utc_now()?;
        let identity = LeaseIdentity::new("recover_state_edit", &nonce, &created_at)
            .map_err(state_lease_error)?;
        let lease = StateLease::try_exclusive(layout, &identity).map_err(state_lease_error)?;
        discard_unprepared_state_edits(layout, &lease).map_err(state_read_error)?;
        let Some(pending) =
            pending_state_edit_generation(layout, &lease).map_err(state_read_error)?
        else {
            return Ok(());
        };
        if layout
            .current_generation()
            .map_err(|_| mutation_failed())?
            .as_ref()
            == Some(&pending)
        {
            recover_transitioned_state_edit(layout, &lease, &pending)
                .map_err(|_| mutation_failed())?;
            return Ok(());
        }
        let source = pending_state_transition_source(layout, &lease, &pending)
            .map_err(|_| mutation_failed())?;
        let prepared = resume_prepared_state_edit(layout.clone(), lease, &pending)
            .map_err(|_| mutation_failed())?;
        let handle = broker
            .begin(BrokerOperationKind::Activate)
            .map_err(broker_error)?;
        let mut local_committed = false;
        let result = (|| {
            let intent = prepared
                .root_transition_intent(source)
                .map_err(|_| mutation_failed())?;
            let report = intent
                .map(|intent| {
                    broker
                        .transition_generation_roots(handle.clone(), intent)
                        .map_err(broker_error)
                })
                .transpose()?;
            prepared
                .activate_transitioned(report.as_ref(), &nonce)
                .map_err(|_| mutation_failed())?
                .finish()
                .map_err(|_| mutation_failed())?;
            local_committed = true;
            let _ = broker.complete(handle.clone());
            Ok(())
        })();
        if result.is_err() && !local_committed {
            let _ = broker.cancel(handle);
        }
        result
    }

    fn recover_pending_prunes(
        &self,
        layout: &StateLayout,
        broker: &mut BrokerLifecycleClient,
    ) -> Result<Vec<String>, CommandError> {
        let handle = broker
            .begin(BrokerOperationKind::Gc)
            .map_err(broker_error)?;
        let result = (|| {
            let lease = self.gc_lease(layout, &handle)?;
            let maintenance = BrokerGcMaintenance {
                broker: Mutex::new(&mut *broker),
                handle: handle.clone(),
            };
            let recovered = recover_prunes(layout, &lease, &maintenance).map_err(gc_error)?;
            drop(maintenance);
            let _ = broker.complete(handle.clone());
            Ok(recovered)
        })();
        if result.is_err() {
            let _ = broker.cancel(handle);
        }
        result
    }
}

fn require_gc_confirmation(
    policy: OperationPolicy,
    plan: &pkg_store::GcPlan,
) -> Result<(), CommandError> {
    let generations = plan
        .candidates()
        .iter()
        .map(|candidate| candidate.generation_id())
        .collect::<Vec<_>>()
        .join(", ");
    require_confirmation(
        policy,
        &format!(
            "Prune {} generation(s) [{}] and run store GC (estimated reclaimable: {} bytes)?",
            plan.candidates().len(),
            generations,
            plan.estimated_reclaimable_bytes()
        ),
    )
}

fn require_supported_install_options(args: &InstallArgs) -> Result<(), CommandError> {
    if args.channel().is_some() {
        return Err(CommandError::new(
            ExitCode::Config,
            "named channel selection is not available in this technical preview",
            "remove `--channel` to use the signed current channel",
        ));
    }
    Ok(())
}

const fn state_collision_policy(policy: CollisionPolicy) -> StateCollisionPolicy {
    match policy {
        CollisionPolicy::Abort => StateCollisionPolicy::Abort,
        CollisionPolicy::KeepFirst => StateCollisionPolicy::KeepFirst,
        CollisionPolicy::KeepLast => StateCollisionPolicy::KeepLast,
    }
}

fn install_selectors(
    args: &InstallArgs,
    nonce: &str,
) -> Result<Vec<PackageSelector>, CommandError> {
    let outputs = if args.outputs().is_empty() {
        OutputSelection::default_selection()
    } else {
        let outputs = args
            .outputs()
            .iter()
            .map(|output| OutputName::new(output).map_err(|_| invalid_install_selector()))
            .collect::<Result<Vec<_>, _>>()?;
        OutputSelection::explicit(outputs).map_err(|_| invalid_install_selector())?
    };
    let mut seen = BTreeSet::new();
    args.packages()
        .iter()
        .enumerate()
        .map(|(index, package)| {
            if !seen.insert(package.as_str()) {
                return Err(invalid_install_selector());
            }
            let id = SelectorId::new(&format!("sel_{nonce}_{index}"))
                .map_err(|_| invalid_install_selector())?;
            let input = SelectorInput::new(package).map_err(|_| invalid_install_selector())?;
            Ok(PackageSelector::new(
                id,
                input,
                VersionPreference::Any,
                outputs.clone(),
                SourceRevision::CurrentChannel,
            ))
        })
        .collect()
}

fn acquire_install_evidence(
    broker: &mut BrokerLifecycleClient,
    selectors: Vec<PackageSelector>,
    policy: OperationPolicy,
    progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
) -> Result<(OperationHandle, String, InstallEvidence, &'static str), CommandError> {
    let acquire_handle = broker
        .begin(BrokerOperationKind::Acquire)
        .map_err(broker_error)?;
    let public_operation_id = acquire_handle.as_str().to_owned();
    if let Err(error) = emit_phase(progress, &public_operation_id, "acquire", "started") {
        let _ = broker.cancel(acquire_handle);
        return Err(error);
    }
    let mut progress_error = None;
    let outcome = match broker.acquire_install_with_progress(
        acquire_handle.clone(),
        selectors.clone(),
        &mut |update| {
            let event = if update.done() == 0 {
                PublicEvent::download_started(
                    &public_operation_id,
                    update.selector().as_str(),
                    update.total(),
                )
            } else {
                PublicEvent::download_progress(
                    &public_operation_id,
                    update.selector().as_str(),
                    update.done(),
                    update.total(),
                )
            }
            .map_err(|_| ())?;
            if let Err(error) = progress(event) {
                progress_error = Some(error);
                return Err(());
            }
            Ok(())
        },
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = broker.cancel(acquire_handle);
            if let Some(error) = progress_error {
                return Err(error);
            }
            return Err(install_broker_error(error));
        }
    };
    if outcome == CacheInstallOutcome::Acquired {
        return match broker.install_evidence(acquire_handle.clone()) {
            Ok(evidence) => {
                if let Err(error) =
                    emit_phase(progress, &public_operation_id, "acquire", "completed")
                {
                    let _ = broker.cancel(acquire_handle);
                    return Err(error);
                }
                Ok((
                    acquire_handle,
                    public_operation_id,
                    evidence,
                    "not_required",
                ))
            }
            Err(error) => {
                let _ = broker.cancel(acquire_handle);
                Err(install_broker_error(error))
            }
        };
    }
    broker
        .complete(acquire_handle.clone())
        .inspect_err(|_| {
            let _ = broker.cancel(acquire_handle.clone());
        })
        .map_err(install_broker_error)?;
    if let Err(error) = emit_phase(progress, &public_operation_id, "acquire", "completed") {
        let _ = broker.cancel(acquire_handle);
        return Err(error);
    }

    let build_handle = broker
        .begin(BrokerOperationKind::Build)
        .map_err(broker_error)?;
    if let Err(error) = emit_phase(progress, &public_operation_id, "build", "started") {
        let _ = broker.cancel(build_handle);
        return Err(error);
    }
    let result = (|| {
        let preview = broker
            .prepare_build(build_handle.clone(), selectors)
            .map_err(install_broker_error)?;
        if !policy.yes() {
            render_build_preview(&preview)?;
        }
        require_confirmation(policy, "Build the missing packages locally?")?;
        let source = if policy.yes() {
            ApprovalSource::AssumeYes
        } else {
            ApprovalSource::Interactive
        };
        let digest = parse_build_plan_digest(preview.build_plan_digest())?;
        broker
            .approve_build(build_handle.clone(), digest, source)
            .map_err(install_broker_error)?;
        let build_targets = preview
            .local_build_targets()
            .map(|(selector, package_name, version)| {
                (
                    selector.to_owned(),
                    package_name.to_owned(),
                    version.to_owned(),
                )
            })
            .collect::<Vec<_>>();
        for (selector, package_name, version) in &build_targets {
            progress(
                PublicEvent::build_started(&public_operation_id, selector, package_name, version)
                    .map_err(|_| install_commit_failed())?,
            )?;
            progress(
                PublicEvent::build_progress(&public_operation_id, selector, 0.0)
                    .map_err(|_| install_commit_failed())?,
            )?;
        }
        broker
            .execute_build_with_progress(build_handle.clone(), digest, &mut |estimate| {
                let pct = f64::from(estimate.millionths())
                    / f64::from(pkg_nix::BuildProgressEstimate::SCALE);
                for (selector, _, _) in &build_targets {
                    let event = PublicEvent::build_progress(&public_operation_id, selector, pct)
                        .map_err(|_| ())?;
                    progress(event).map_err(|_| ())?;
                }
                Ok(())
            })
            .map_err(install_broker_error)?;
        for (selector, _, _) in &build_targets {
            progress(
                PublicEvent::build_progress(&public_operation_id, selector, 1.0)
                    .map_err(|_| install_commit_failed())?,
            )?;
        }
        let evidence = broker
            .install_evidence(build_handle.clone())
            .map_err(install_broker_error)?;
        emit_phase(progress, &public_operation_id, "build", "completed")?;
        Ok((
            build_handle.clone(),
            public_operation_id.clone(),
            evidence,
            source.as_str(),
        ))
    })();
    if result.is_err() {
        let _ = broker.cancel(build_handle);
    }
    result
}

fn parse_build_plan_digest(value: &str) -> Result<Digest, CommandError> {
    let hex = value
        .strip_prefix("sha256:")
        .ok_or_else(install_commit_failed)?;
    Digest::from_str(&format!("sha256-{hex}")).map_err(|_| install_commit_failed())
}

fn emit_phase(
    progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
    op_id: &str,
    phase: &str,
    status: &str,
) -> Result<(), CommandError> {
    progress(PublicEvent::phase(op_id, phase, status).map_err(|_| install_commit_failed())?)
}

fn render_build_preview(preview: &pkg_nix::BuildPreview) -> Result<(), CommandError> {
    let value = preview
        .to_json_value()
        .map_err(|_| install_commit_failed())?;
    let rendered = serde_json::to_string_pretty(&value).map_err(|_| install_commit_failed())?;
    let mut stderr = io::stderr();
    writeln!(stderr, "Local build required:\n{rendered}")
        .and_then(|()| stderr.flush())
        .map_err(|_| confirmation_required())
}

fn preview_install(
    broker: &mut BrokerLifecycleClient,
    selectors: Vec<PackageSelector>,
) -> Result<CommandResult, CommandError> {
    let handle = broker
        .begin(BrokerOperationKind::Build)
        .map_err(broker_error)?;
    let result = broker
        .prepare_build(handle.clone(), selectors)
        .map_err(install_broker_error)
        .and_then(|preview| {
            let value = preview
                .to_json_value()
                .map_err(|_| install_commit_failed())?;
            CommandResult::new(
                "Install preview is ready. No package was downloaded or activated.",
                Map::from_iter([("dryRun".into(), json!(true)), ("preflight".into(), value)]),
                Vec::new(),
            )
            .map_err(|_| install_commit_failed())
        });
    let _ = broker.cancel(handle);
    result
}

fn install_result(
    op_id: &str,
    generation_id: &str,
    current: Option<&pkg_core::GenerationSnapshot>,
    evidence: &pkg_nix::InstallEvidence,
) -> Result<CommandResult, CommandError> {
    let added = evidence
        .targets()
        .iter()
        .map(|target| {
            json!({
                "selector": target.selector().as_str(),
                "package": target.package_name(),
                "version": target.package_version().as_str(),
                "outputs": target.outputs_to_install().iter().map(OutputName::as_str).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    CommandResult::new(
        format!(
            "Installed {} package(s) as {generation_id}.",
            evidence.targets().len()
        ),
        Map::from_iter([
            ("opId".into(), json!(op_id)),
            (
                "generation".into(),
                json!({
                    "id": generation_id,
                    "parent": current.map(|snapshot| snapshot.generation().id())
                }),
            ),
            ("added".into(), json!(added)),
        ]),
        Vec::new(),
    )
    .map_err(|_| install_commit_failed())
}

fn invalid_install_selector() -> CommandError {
    CommandError::new(
        ExitCode::Usage,
        "a package selector or output name is invalid",
        "use package and output names made from letters, numbers, dots, dashes, and underscores",
    )
}

fn install_broker_error(error: BrokerClientError) -> CommandError {
    let exit = match error.code() {
        BrokerClientErrorCode::InstallAcquisitionRefused => match error.cache_install_code() {
            Some(CacheInstallErrorCode::InvalidIntent) => ExitCode::ResolveFailed,
            Some(CacheInstallErrorCode::AcquisitionFailed) => ExitCode::AcquireNetwork,
            Some(CacheInstallErrorCode::Cancelled) => ExitCode::Cancelled,
            Some(CacheInstallErrorCode::AuthorityUnavailable) | None => ExitCode::EngineUnavailable,
        },
        BrokerClientErrorCode::BuildRefused => ExitCode::BuildFailed,
        BrokerClientErrorCode::BuildRootRefused => ExitCode::Permission,
        BrokerClientErrorCode::GenerationRootAttestationRefused => ExitCode::StateCorrupt,
        _ => ExitCode::EngineUnavailable,
    };
    CommandError::new(
        exit,
        "the managed install transaction was refused",
        "run `pkg doctor`, then retry the install",
    )
}

fn map_install_generation_error(error: InstallGenerationError) -> CommandError {
    match error {
        InstallGenerationError::CurrentChanged => CommandError::new(
            ExitCode::StateLocked,
            "the active package generation changed during installation",
            "retry the install after the other package operation finishes",
        ),
        InstallGenerationError::Stage => CommandError::new(
            ExitCode::StageCollision,
            "package commands collide under the abort policy",
            "remove one conflicting package or select different outputs",
        ),
        _ => install_commit_failed(),
    }
}

fn install_commit_failed() -> CommandError {
    CommandError::new(
        ExitCode::StateCorrupt,
        "the install generation could not be committed safely",
        "run `pkg doctor` before retrying the install",
    )
}

fn ensure_generation_deletable(
    active: &pkg_core::GenerationSnapshot,
    history: &History,
    generation: &str,
) -> Result<(), CommandError> {
    if active.generation().id() == generation {
        return Err(CommandError::new(
            ExitCode::ResolveFailed,
            "the active generation cannot be deleted",
            "roll back or activate another generation first",
        ));
    }
    if history
        .snapshots()
        .iter()
        .all(|snapshot| snapshot.generation().id() != generation)
    {
        return Err(CommandError::new(
            ExitCode::ResolveFailed,
            "the requested generation does not exist",
            "run `pkg history` to list retained generations",
        ));
    }
    Ok(())
}

fn require_confirmation(policy: OperationPolicy, prompt: &str) -> Result<(), CommandError> {
    if policy.yes() {
        return Ok(());
    }
    let stdin = io::stdin();
    let mut stderr = io::stderr();
    if !stdin.is_terminal() || !stderr.is_terminal() {
        return Err(confirmation_required());
    }
    write!(stderr, "{prompt} [y/N] ").map_err(|_| confirmation_required())?;
    stderr.flush().map_err(|_| confirmation_required())?;
    let mut answer = String::new();
    stdin
        .read_line(&mut answer)
        .map_err(|_| confirmation_required())?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CommandError::new(
            ExitCode::Cancelled,
            "the destructive operation was not approved",
            "the newly requested mutation was not started",
        ))
    }
}

fn gc_policy(args: &GcArgs) -> Result<GcPolicy, CommandError> {
    let keep_generations = args
        .keep_generations()
        .map_or(Ok(DEFAULT_KEEP_GENERATIONS), usize::try_from)
        .map_err(|_| gc_failed())?;
    let max_age_days = args.max_age_days().map_or(DEFAULT_MAX_AGE_DAYS, u64::from);
    GcPolicy::new(keep_generations, max_age_days).map_err(|_| gc_failed())
}

fn gc_preview_result(plan: &pkg_store::GcPlan) -> Result<CommandResult, CommandError> {
    let generations = plan
        .candidates()
        .iter()
        .map(|candidate| candidate.generation_id())
        .collect::<Vec<_>>();
    let records = generations
        .iter()
        .map(|generation| {
            Map::from_iter([
                ("type".into(), json!("generation_prune_preview")),
                ("generation".into(), json!(generation)),
            ])
        })
        .collect();
    CommandResult::new(
        format!("{} generation(s) would be pruned", generations.len()),
        Map::from_iter([
            ("dryRun".into(), json!(true)),
            ("generations".into(), json!(generations)),
            (
                "estimatedReclaimableBytes".into(),
                json!(plan.estimated_reclaimable_bytes()),
            ),
        ]),
        records,
    )
    .map_err(|_| gc_failed())
}

fn gc_run_result(
    pruned: &[String],
    recovered: &[String],
    report: &pkg_nix::GcReport,
) -> Result<CommandResult, CommandError> {
    let records = pruned
        .iter()
        .map(|generation| {
            Map::from_iter([
                ("type".into(), json!("generation_pruned")),
                ("generation".into(), json!(generation)),
            ])
        })
        .collect();
    CommandResult::new(
        format!(
            "pruned {} generation(s); collected {} store path(s)",
            pruned.len(),
            report.collected().len()
        ),
        Map::from_iter([
            ("prunedGenerations".into(), json!(pruned)),
            ("recoveredGenerations".into(), json!(recovered)),
            ("collectedPathCount".into(), json!(report.collected().len())),
            ("freedBytes".into(), json!(report.freed_bytes())),
        ]),
        records,
    )
    .map_err(|_| gc_failed())
}

fn generation_prune_result(generation: &str, dry_run: bool) -> Result<CommandResult, CommandError> {
    let action = if dry_run { "would be pruned" } else { "pruned" };
    CommandResult::new(
        format!("generation {generation} {action}"),
        Map::from_iter([
            ("generation".into(), json!(generation)),
            ("dryRun".into(), json!(dry_run)),
        ]),
        vec![],
    )
    .map_err(|_| gc_failed())
}

fn unix_now() -> Result<u64, CommandError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| mutation_failed())
}

fn production_state_root(home: &Path) -> Option<PathBuf> {
    match std::env::consts::OS {
        "linux" => Some(home.join(".local/share/pkg")),
        "macos" => Some(home.join("Library/Application Support/pkg")),
        _ => None,
    }
}

fn secure_nonce() -> Result<String, CommandError> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| mutation_failed())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn next_generation_id(active: &str) -> Result<String, CommandError> {
    let digits = active.strip_prefix("gen-").ok_or_else(mutation_failed)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(mutation_failed());
    }
    let mut next = digits.as_bytes().to_vec();
    let mut carry = true;
    for digit in next.iter_mut().rev() {
        if *digit == b'9' {
            *digit = b'0';
        } else {
            *digit += 1;
            carry = false;
            break;
        }
    }
    if carry {
        next.insert(0, b'1');
    }
    String::from_utf8(next)
        .map(|digits| format!("gen-{digits}"))
        .map_err(|_| mutation_failed())
}

fn utc_now() -> Result<String, CommandError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| mutation_failed())?
        .as_secs();
    format_utc(seconds).ok_or_else(mutation_failed)
}

fn format_utc(seconds: u64) -> Option<String> {
    let days = i64::try_from(seconds / 86_400).ok()?;
    let second_of_day = seconds % 86_400;
    let z = days.checked_add(719_468)?;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    if !(1970..=9999).contains(&year) {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3_600,
        second_of_day % 3_600 / 60,
        second_of_day % 60
    ))
}

fn state_lease_error(error: LeaseError) -> CommandError {
    let exit = if error == LeaseError::Locked {
        ExitCode::StateLocked
    } else {
        ExitCode::StateCorrupt
    };
    CommandError::new(
        exit,
        "the package state could not be read consistently",
        "wait for the active operation, then run `pkg doctor` if this persists",
    )
}

fn state_read_error(_error: CommitError) -> CommandError {
    CommandError::new(
        ExitCode::StateCorrupt,
        "the package generation history failed verification",
        "run `pkg doctor` before making changes",
    )
}

fn no_active_generation() -> CommandError {
    CommandError::new(
        ExitCode::ResolveFailed,
        "no package generation is active",
        "install a package before using this command",
    )
}

fn broker_error(_error: crate::broker::BrokerClientError) -> CommandError {
    CommandError::new(
        ExitCode::EngineUnavailable,
        "the managed package service refused the transaction",
        "run `pkg doctor` to inspect managed broker readiness",
    )
}

fn mutation_failed() -> CommandError {
    CommandError::new(
        ExitCode::StateCorrupt,
        "the package state change could not be committed safely",
        "run `pkg doctor` before retrying the change",
    )
}

fn gc_failed() -> CommandError {
    CommandError::new(
        ExitCode::StateCorrupt,
        "generation cleanup could not be completed safely",
        "run `pkg doctor`, then retry the command",
    )
}

fn gc_error(error: GcError) -> CommandError {
    match error {
        GcError::RootRemoval | GcError::Nix => CommandError::new(
            ExitCode::EngineUnavailable,
            "the managed package service could not complete cleanup",
            "run `pkg doctor`, then retry the command",
        ),
        GcError::LeaseRequired => CommandError::new(
            ExitCode::StateLocked,
            "generation cleanup lost its exclusive state lease",
            "wait for the active operation, then retry the command",
        ),
        _ => gc_failed(),
    }
}

fn confirmation_required() -> CommandError {
    CommandError::new(
        ExitCode::AcquireNeedsApproval,
        "the destructive operation requires confirmation",
        "run interactively or pass `--yes` after reviewing a dry run",
    )
}

fn index_unavailable() -> CommandError {
    CommandError::new(
        ExitCode::EngineUnavailable,
        "authenticated package metadata is not available",
        "run `pkg doctor` to inspect managed broker readiness",
    )
}

fn mutation_unavailable() -> CommandError {
    CommandError::new(
        ExitCode::EngineUnavailable,
        "the complete package mutation transaction is not available",
        "run `pkg doctor` to inspect managed broker readiness",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixStream;
    use std::thread;

    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::broker::BrokerLifecycleClient;
    use crate::cli::Cli;
    use crate::commands::execute::{
        CommandEngine, CommandRequest, CoreEngine, OperationPolicy, write_success,
    };
    use crate::ux::OutputMode;
    use pkg_nix::{
        BuildOutput, BuildOutputProvenance, BuildPreview, BuildReport, BuildStatus,
        CliBrokerRequest, CliBrokerResponse, InProcessBroker, InProcessCallerPeer,
        ProductFrameCodec, StorePath,
    };

    const FRAME_HEADER_BYTES: usize = 20;
    const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
    const NAR_HASH: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    fn read_request(stream: &mut UnixStream) -> (u64, CliBrokerRequest) {
        let mut header = [0_u8; FRAME_HEADER_BYTES];
        stream.read_exact(&mut header).unwrap();
        let length = u32::from_be_bytes(header[16..20].try_into().unwrap()) as usize;
        let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + length);
        frame.extend_from_slice(&header);
        frame.resize(FRAME_HEADER_BYTES + length, 0);
        stream.read_exact(&mut frame[FRAME_HEADER_BYTES..]).unwrap();
        ProductFrameCodec::decode_cli_request(&frame).unwrap()
    }

    fn write_response(stream: &mut UnixStream, request_id: u64, response: CliBrokerResponse) {
        let frame = ProductFrameCodec::encode_cli_response(request_id, &response).unwrap();
        stream.write_all(&frame).unwrap();
    }

    fn install_evidence(provenance: &str) -> InstallEvidence {
        let store_path = format!("/nix/store/{STORE_HASH}-hello-1.0");
        let derivation = format!("{store_path}.drv");
        InstallEvidence::from_json_bytes(
            &serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "descriptorHash": format!("sha256-{}", "0".repeat(64)),
                "channelSequence": 42,
                "policyVersion": 7,
                "revision": REVISION,
                "sourceNarHash": NAR_HASH,
                "system": "x86_64-linux",
                "targets": [{
                    "selectorId": "sel_hello",
                    "selector": "hello",
                    "attribute": "hello",
                    "versionPreference": { "kind": "any" },
                    "requestedOutputs": null,
                    "sourceRevision": "channel:current",
                    "rootDerivation": derivation,
                    "rootOutputs": [{ "name": "out", "storePath": store_path }],
                    "outputsToInstall": ["out"],
                    "packageName": "hello",
                    "packageVersion": "1.0",
                    "acquired": [{
                        "outputName": "out",
                        "storePath": store_path,
                        "narHash": NAR_HASH,
                        "signatures": if provenance == "cacheSigned" {
                            vec!["cache.nixos.org-1:AAAA"]
                        } else {
                            Vec::new()
                        },
                        "references": [],
                        "deriver": derivation,
                        "narSize": 20,
                        "closureSize": 42,
                        "provenance": provenance
                    }]
                }]
            }))
            .unwrap(),
        )
        .unwrap()
    }

    fn build_preview() -> BuildPreview {
        BuildPreview::from_json_bytes(
            &serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "platform": { "os": "linux", "arch": "x86_64" },
                "policyVersion": 7,
                "buildPlanDigest": format!("sha256:{}", "1".repeat(64)),
                "targets": [{
                    "selector": "hello",
                    "packageName": "hello",
                    "version": "1.0",
                    "outputsToInstall": ["out"],
                    "localBuildRequired": true
                }],
                "build": { "count": 1, "names": ["hello"], "hasFixedOutput": false },
                "cache": { "knownDownloadBytes": 0, "knownContentBytes": 0 },
                "unknownLocalOutputs": 1,
                "estimates": {
                    "approxBuildMinutes": null,
                    "approxNewDiskBytes": 1073741824,
                    "approxTotalClosureBytes": null
                },
                "readiness": {
                    "sandboxed": true,
                    "buildIsolationReady": true,
                    "nativeBuild": true,
                    "resourceBoundary": {
                        "isolation": "sandbox",
                        "perBuildResourceCap": false,
                        "notice": "Builds run sandboxed. The managed runtime applies no hard per-build memory/CPU/IO cap; daemon time/log ceilings and one machine-global build admission bound the operation."
                    }
                },
                "approvalRequired": true
            }))
            .unwrap(),
        )
        .unwrap()
    }

    fn hello_selectors() -> Vec<PackageSelector> {
        let cli = Cli::try_parse(["pkg", "install", "hello"]).unwrap();
        let crate::cli::Command::Install(args) = cli.parsed_command() else {
            panic!("expected install command");
        };
        install_selectors(args, "00112233445566778899aabbccddeeff").unwrap()
    }

    #[test]
    fn cache_hit_uses_the_closed_acquire_protocol_and_returns_evidence() {
        let (mut server, client) = UnixStream::pair().unwrap();
        let expected = install_evidence("cacheSigned");
        let server_evidence = expected.clone();
        let worker = thread::spawn(move || {
            let broker = InProcessBroker::new().unwrap();
            let caller = broker
                .connect(InProcessCallerPeer::authenticated(501))
                .unwrap();

            let (request_id, request) = read_request(&mut server);
            let CliBrokerRequest::Begin(BrokerOperationKind::Acquire) = request else {
                panic!("expected acquire begin");
            };
            let handle = caller.begin(BrokerOperationKind::Acquire).unwrap();
            write_response(
                &mut server,
                request_id,
                CliBrokerResponse::Started(handle.clone()),
            );

            let (request_id, request) = read_request(&mut server);
            let CliBrokerRequest::AcquireInstall(actual, selectors) = request else {
                panic!("expected cache acquisition");
            };
            assert_eq!(actual, handle);
            assert_eq!(selectors.len(), 1);
            assert_eq!(selectors[0].selector().as_str(), "hello");
            write_response(
                &mut server,
                request_id,
                CliBrokerResponse::InstallDownloadProgress(
                    pkg_nix::InstallDownloadProgress::new(
                        SelectorInput::new("hello").unwrap(),
                        0,
                        42,
                    )
                    .unwrap(),
                ),
            );
            write_response(
                &mut server,
                request_id,
                CliBrokerResponse::InstallDownloadProgress(
                    pkg_nix::InstallDownloadProgress::new(
                        SelectorInput::new("hello").unwrap(),
                        42,
                        42,
                    )
                    .unwrap(),
                ),
            );
            write_response(&mut server, request_id, CliBrokerResponse::InstallAcquired);

            let (request_id, request) = read_request(&mut server);
            let CliBrokerRequest::GetInstallEvidence(actual) = request else {
                panic!("expected private install evidence request");
            };
            assert_eq!(actual, handle);
            write_response(
                &mut server,
                request_id,
                CliBrokerResponse::InstallEvidence(server_evidence),
            );
            let mut eof = [0_u8; 1];
            assert_eq!(server.read(&mut eof).unwrap(), 0);
        });

        let mut broker = BrokerLifecycleClient::from_stream(client);
        let mut events = Vec::new();
        let (handle, public_operation_id, actual, approval) = acquire_install_evidence(
            &mut broker,
            hello_selectors(),
            OperationPolicy::for_test(true, false),
            &mut |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();
        assert!(!handle.as_str().is_empty());
        assert_eq!(actual, expected);
        assert_eq!(approval, "not_required");
        assert_eq!(events.len(), 4);
        let rendered = events
            .iter()
            .map(|event| String::from_utf8(event.to_ndjson_line().unwrap()).unwrap())
            .collect::<String>();
        assert!(rendered.contains(r#""type":"download_started""#));
        assert!(rendered.contains(r#""type":"download_progress""#));
        assert!(rendered.contains(r#""done":42,"total":42"#));
        assert!(
            events
                .iter()
                .all(|event| event.op_id() == public_operation_id)
        );
        drop(broker);
        worker.join().unwrap();
    }

    #[test]
    fn cache_miss_uses_one_digest_bound_build_and_returns_local_evidence() {
        let (mut server, client) = UnixStream::pair().unwrap();
        let preview = build_preview();
        let digest = parse_build_plan_digest(preview.build_plan_digest()).unwrap();
        let expected = install_evidence("localBuild");
        let server_evidence = expected.clone();
        let worker = thread::spawn(move || {
            let broker = InProcessBroker::new().unwrap();
            let caller = broker
                .connect(InProcessCallerPeer::authenticated(501))
                .unwrap();

            let (request_id, request) = read_request(&mut server);
            let CliBrokerRequest::Begin(BrokerOperationKind::Acquire) = request else {
                panic!("expected acquire begin");
            };
            let acquire_handle = caller.begin(BrokerOperationKind::Acquire).unwrap();
            write_response(
                &mut server,
                request_id,
                CliBrokerResponse::Started(acquire_handle.clone()),
            );

            let (request_id, request) = read_request(&mut server);
            let CliBrokerRequest::AcquireInstall(actual, selectors) = request else {
                panic!("expected cache acquisition");
            };
            assert_eq!(actual, acquire_handle);
            assert_eq!(selectors[0].selector().as_str(), "hello");
            write_response(
                &mut server,
                request_id,
                CliBrokerResponse::InstallBuildRequired,
            );

            let (request_id, request) = read_request(&mut server);
            let CliBrokerRequest::Complete(actual) = request else {
                panic!("expected cache operation completion");
            };
            assert_eq!(actual, acquire_handle);
            write_response(&mut server, request_id, CliBrokerResponse::Completed);

            let (request_id, request) = read_request(&mut server);
            let CliBrokerRequest::Begin(BrokerOperationKind::Build) = request else {
                panic!("expected build begin");
            };
            let build_handle = caller.begin(BrokerOperationKind::Build).unwrap();
            write_response(
                &mut server,
                request_id,
                CliBrokerResponse::Started(build_handle.clone()),
            );

            let (request_id, request) = read_request(&mut server);
            let CliBrokerRequest::PrepareBuild(actual, selectors) = request else {
                panic!("expected private build preparation");
            };
            assert_eq!(actual, build_handle);
            assert_eq!(selectors[0].selector().as_str(), "hello");
            write_response(
                &mut server,
                request_id,
                CliBrokerResponse::BuildPrepared(preview),
            );

            let (request_id, request) = read_request(&mut server);
            let CliBrokerRequest::ApproveBuild(actual, approval) = request else {
                panic!("expected exact build approval");
            };
            assert_eq!(actual, build_handle);
            assert_eq!(approval.build_plan_digest(), digest);
            assert_eq!(approval.source(), ApprovalSource::AssumeYes);
            write_response(&mut server, request_id, CliBrokerResponse::BuildApproved);

            let (request_id, request) = read_request(&mut server);
            let CliBrokerRequest::ExecuteBuild(actual, actual_digest) = request else {
                panic!("expected exact build execution");
            };
            assert_eq!(actual, build_handle);
            assert_eq!(actual_digest, digest);
            let report = BuildReport::new(
                BuildStatus::Built,
                vec![BuildOutput::new(
                    StorePath::new(&format!("/nix/store/{STORE_HASH}-hello-1.0")).unwrap(),
                    BuildOutputProvenance::LocalBuild,
                )],
            )
            .unwrap();
            write_response(
                &mut server,
                request_id,
                CliBrokerResponse::BuildExecuted(report),
            );

            let (request_id, request) = read_request(&mut server);
            let CliBrokerRequest::GetInstallEvidence(actual) = request else {
                panic!("expected post-build install evidence");
            };
            assert_eq!(actual, build_handle);
            write_response(
                &mut server,
                request_id,
                CliBrokerResponse::InstallEvidence(server_evidence),
            );
            let mut eof = [0_u8; 1];
            assert_eq!(server.read(&mut eof).unwrap(), 0);
        });

        let mut broker = BrokerLifecycleClient::from_stream(client);
        let mut events = Vec::new();
        let result = acquire_install_evidence(
            &mut broker,
            hello_selectors(),
            OperationPolicy::for_test(true, false),
            &mut |event| {
                events.push(event);
                Ok(())
            },
        );
        let (handle, public_operation_id, actual, approval) = match result {
            Ok(result) => result,
            Err(error) => {
                drop(broker);
                let server_result = worker.join();
                panic!("client failed: {error:?}; server: {server_result:?}");
            }
        };
        assert!(!handle.as_str().is_empty());
        assert_eq!(actual, expected);
        assert_eq!(approval, "yes");
        assert_eq!(
            events,
            vec![
                PublicEvent::phase(&public_operation_id, "acquire", "started").unwrap(),
                PublicEvent::phase(&public_operation_id, "acquire", "completed").unwrap(),
                PublicEvent::phase(&public_operation_id, "build", "started").unwrap(),
                PublicEvent::build_started(&public_operation_id, "hello", "hello", "1.0",).unwrap(),
                PublicEvent::build_progress(&public_operation_id, "hello", 0.0).unwrap(),
                PublicEvent::build_progress(&public_operation_id, "hello", 1.0).unwrap(),
                PublicEvent::phase(&public_operation_id, "build", "completed").unwrap(),
            ]
        );
        assert!(
            events
                .iter()
                .all(|event| event.op_id() == public_operation_id)
        );
        drop(broker);
        worker.join().unwrap();
    }

    #[test]
    fn install_success_output_matches_the_v1_golden() {
        let result = install_result(
            "op_fixture",
            "gen-0001",
            None,
            &install_evidence("cacheSigned"),
        )
        .unwrap();
        assert_eq!(result.summary(), "Installed 1 package(s) as gen-0001.");

        let mut output = Vec::new();
        write_success(&mut output, OutputMode::Json, "install", &result).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            include_str!("../../../../fixtures/cli-v1/install-success.json")
        );
    }

    #[test]
    fn missing_state_is_initialized_as_empty_history() {
        let home = TempDir::new().unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let uid = fs::symlink_metadata(home.path()).unwrap().uid();
        let cli = Cli::try_parse(["pkg", "history"]).unwrap();
        let mut engine = CoreEngine::new(LocalStateOperations::open(
            home.path(),
            &home.path().join("pkg"),
            uid,
        ));
        let result = engine.execute(&CommandRequest::from_cli(&cli)).unwrap();
        assert_eq!(result.fields()["entries"], Value::Array(vec![]));
        assert_eq!(
            fs::symlink_metadata(home.path().join("pkg"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn initialized_empty_state_reports_no_active_generation() {
        let home = TempDir::new().unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let uid = fs::symlink_metadata(home.path()).unwrap().uid();
        let state = home.path().join("pkg");
        for relative in ["", "generations", "journal", "activations", "run"] {
            let path = state.join(relative);
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let identity =
            pkg_store::LeaseIdentity::new("op_initialize", "nonce1", "2026-08-11T00:00:00Z")
                .unwrap();
        let layout = StateLayout::open(home.path(), &state, uid).unwrap();
        drop(StateLease::try_exclusive(&layout, &identity).unwrap());

        let cli = Cli::try_parse(["pkg", "history"]).unwrap();
        let mut engine = CoreEngine::new(LocalStateOperations::open(home.path(), &state, uid));
        let result = engine.execute(&CommandRequest::from_cli(&cli)).unwrap();
        assert_eq!(result.fields()["entries"], Value::Array(vec![]));
    }

    #[test]
    fn mutation_identity_helpers_are_canonical_and_overflow_safe() {
        assert_eq!(next_generation_id("gen-0009").unwrap(), "gen-0010");
        assert_eq!(next_generation_id("gen-9999").unwrap(), "gen-10000");
        assert!(next_generation_id("generation-1").is_err());
        assert_eq!(format_utc(0).as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(
            format_utc(951_782_400).as_deref(),
            Some("2000-02-29T00:00:00Z")
        );
        assert_eq!(
            format_utc(1_787_528_645).as_deref(),
            Some("2026-08-23T23:44:05Z")
        );
    }

    #[test]
    fn install_arguments_become_closed_current_channel_selectors() {
        let cli = Cli::try_parse([
            "pkg",
            "install",
            "ripgrep",
            "fd",
            "--with-outputs",
            "out,man",
        ])
        .unwrap();
        let crate::cli::Command::Install(args) = cli.parsed_command() else {
            panic!("expected install command");
        };
        require_supported_install_options(args).unwrap();
        let selectors = install_selectors(args, "00112233445566778899aabbccddeeff").unwrap();

        assert_eq!(selectors.len(), 2);
        assert_eq!(selectors[0].selector().as_str(), "ripgrep");
        assert_eq!(
            selectors[0]
                .outputs()
                .explicit_outputs()
                .unwrap()
                .iter()
                .map(OutputName::as_str)
                .collect::<Vec<_>>(),
            ["out", "man"]
        );
        assert!(matches!(
            selectors[0].source_revision(),
            SourceRevision::CurrentChannel
        ));
        assert_ne!(selectors[0].id(), selectors[1].id());
    }

    #[test]
    fn install_argument_widening_is_refused_before_broker_access() {
        for argv in [
            vec!["pkg", "install", "ripgrep", "ripgrep"],
            vec!["pkg", "install", "ripgrep", "--channel", "other"],
        ] {
            let cli = Cli::try_parse(argv).unwrap();
            let crate::cli::Command::Install(args) = cli.parsed_command() else {
                panic!("expected install command");
            };
            assert!(
                require_supported_install_options(args).is_err()
                    || install_selectors(args, "00112233445566778899aabbccddeeff").is_err()
            );
        }
    }

    #[test]
    fn install_collision_policy_reaches_the_state_boundary() {
        for (value, expected) in [
            ("abort", StateCollisionPolicy::Abort),
            ("keep-first", StateCollisionPolicy::KeepFirst),
            ("keep-last", StateCollisionPolicy::KeepLast),
        ] {
            let cli =
                Cli::try_parse(["pkg", "install", "ripgrep", "--on-collision", value]).unwrap();
            let crate::cli::Command::Install(args) = cli.parsed_command() else {
                panic!("expected install command");
            };
            require_supported_install_options(args).unwrap();
            assert_eq!(state_collision_policy(args.collision_policy()), expected);
        }
    }

    #[test]
    fn alternate_state_roots_are_read_only_for_broker_backed_mutations() {
        let home = TempDir::new().unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let uid = fs::symlink_metadata(home.path()).unwrap().uid();
        let state = home.path().join("alternate");
        let cli =
            Cli::try_parse(["pkg", "gc", "--yes", "--state", state.to_str().unwrap()]).unwrap();
        let mut engine = CoreEngine::new(LocalStateOperations::open(home.path(), &state, uid));

        let error = engine.execute(&CommandRequest::from_cli(&cli)).unwrap_err();
        assert_eq!(error.exit_code(), ExitCode::Config);
        assert!(!state.join("journal/operations.jsonl").exists());
    }
}
