//! Production command adapter over the invoking user's verified local state.

use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::broker::BrokerLifecycleClient;
use crate::cli::{
    GcArgs, HistoryArgs, InfoArgs, InstallArgs, ListArgs, PackageArgs, RemoveArgs, RepairArgs,
    RollbackArgs, SearchArgs, UpdateArgs, UpgradeArgs,
};
use crate::commands::execute::{CommandResult, CoreOperations, OperationPolicy};
use crate::commands::state::{
    LifecycleEdit, edit_pin_state, list_state, read_history, remove_state, rollback_state,
};
use crate::exit::ExitCode;
use crate::ux::CommandError;
use pkg_core::{History, PinAction};
use pkg_nix::{
    BrokerOperationKind, MaintenanceAdapter, MaintenanceError, OperationHandle,
    RemoveRootSetRequest, RepairStorePathsReport, RepairStorePathsRequest, RootSet, RootSetReport,
};
use pkg_pipeline::{
    CommitError, StateEditKind, StateEditMetadata, discard_unprepared_state_edits,
    load_active_snapshot, load_retained_history, pending_state_edit_generation,
    pending_state_transition_source, prepare_rollback, prepare_state_edit,
    recover_transitioned_state_edit, resume_prepared_state_edit,
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

impl MaintenanceAdapter for BrokerGcMaintenance<'_> {
    fn publish_root_set(&self, _root_set: &RootSet) -> Result<RootSetReport, MaintenanceError> {
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
/// Read-only state, state-only generation edits, rollback, and GC are live.
/// Install/upgrade/repair and authenticated-index commands remain explicit
/// closed refusals until their transaction coordinators are connected; no
/// command can fall through to raw Nix access.
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
        _args: &InstallArgs,
        _policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        Err(mutation_unavailable())
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
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::cli::Cli;
    use crate::commands::execute::{CommandEngine, CommandRequest, CoreEngine};

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
