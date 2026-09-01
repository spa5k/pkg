//! Production command adapter over the invoking user's verified local state.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::broker::{BrokerClientError, BrokerClientErrorCode, BrokerLifecycleClient};
use crate::cli::{
    CollisionPolicy, GcArgs, HistoryArgs, InfoArgs, InstallArgs, ListArgs, PackageArgs, RemoveArgs,
    RepairArgs, RollbackArgs, SearchArgs, UpdateArgs, UpgradeArgs,
};

/// One acquired install: its handle, public id, evidence, and phase name.
type AcquiredInstallEvidence = (OperationHandle, String, InstallEvidence, &'static str);
use crate::commands::execute::{CommandResult, CoreOperations, OperationPolicy};
use crate::commands::query::{
    InstalledCatalogPackage, info_catalog_reports, outdated_catalog_reports, search_catalog_report,
};
use crate::commands::state::{
    LifecycleEdit, edit_pin_state, list_state, read_history, remove_state, rollback_state,
};
use crate::exit::ExitCode;
use crate::path::StateLocation;
use crate::progress::PublicEvent;
use crate::ux::CommandError;
use pkg_core::state::CollisionPolicy as StateCollisionPolicy;
use pkg_core::upgrade::{UpgradeScope, select_upgrade};
use pkg_core::{
    History, OutputName, OutputSelection, PackageSelector, PinAction, SelectorId, SelectorInput,
    SourceRevision, VersionPreference, advance_channel,
};
use pkg_nix::{
    ApprovalSource, BrokerOperationKind, BuildPreview, CacheInstallErrorCode, CacheInstallOutcome,
    CatalogInfoRequest, CatalogSearchRequest, ChannelRefreshMode, ChannelRefreshReport, Digest,
    GenerationId, GenerationRootAttestationErrorCode, InstallEvidence, MaintenanceAdapter,
    MaintenanceError, OperationHandle, OperationStatus, RemoveRootSetRequest,
    RepairGenerationRequest, RepairGenerationStatus, RepairStorePathsReport,
    RepairStorePathsRequest, RootSet, RootSetAttestationRequest, RootSetReport,
};
use pkg_pipeline::{
    CommitError, InstallGenerationError, InstallGenerationMetadata, InstallStateError,
    StateEditKind, StateEditMetadata, assemble_upgrade_evidence_state, discard_unprepared_installs,
    discard_unprepared_state_edits, load_active_snapshot, load_retained_history,
    pending_install_discard_generation, pending_install_generation, pending_state_edit_generation,
    pending_state_transition_source, prepare_install_generation, prepare_rollback,
    prepare_state_edit, recover_generation, recover_transitioned_state_edit,
    resume_prepared_install, resume_prepared_state_edit,
};
use pkg_store::{
    GcError, GcPolicy, LeaseError, LeaseIdentity, PruneOutcome, StateLayout, StateLease, plan_gc,
    plan_generation_prune, prune_generation, recover_prunes,
};
use serde_json::{Map, Value, json};

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
    source: StateLayout,
    broker_state_compatible: bool,
}

impl LocalStateOperations {
    /// Opens the resolved state location beneath its trusted ownership boundary.
    pub fn open(location: &StateLocation, owner_uid: u32) -> Result<Self, CommandError> {
        let source = StateLayout::initialize(
            location.trusted_boundary(),
            location.state_root(),
            owner_uid,
        )
        .map_err(|_| {
            CommandError::new(
                ExitCode::StateCorrupt,
                "the per-user package state is missing or unsafe",
                "run `pkg doctor` before managing packages",
            )
        })?;
        Ok(Self {
            source,
            broker_state_compatible: location.is_production(),
        })
    }

    const fn layout(&self) -> &StateLayout {
        &self.source
    }

    fn active(&self) -> Result<pkg_core::GenerationSnapshot, CommandError> {
        let layout = self.layout();
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
        let layout = self.layout();
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
    fn search(&mut self, args: &SearchArgs) -> Result<CommandResult, CommandError> {
        if args.channel().is_some() {
            return Err(CommandError::new(
                ExitCode::ResolveFailed,
                "the selected channel is not loaded",
                "omit `--channel` or run `pkg update` for the current channel",
            ));
        }
        let request =
            CatalogSearchRequest::new(args.query(), args.limit(), args.exact(), args.license())
                .ok_or_else(catalog_query_invalid)?;
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;
        run_catalog_search(&mut broker, request)
    }

    fn info(&mut self, args: &InfoArgs) -> Result<CommandResult, CommandError> {
        if args.exact() {
            return Err(CommandError::new(
                ExitCode::EngineUnavailable,
                "exact package inspection requires the private package engine",
                "omit `--exact` for verified catalog metadata",
            ));
        }
        if args.channel().is_some() {
            return Err(CommandError::new(
                ExitCode::ResolveFailed,
                "the selected channel is not loaded",
                "omit `--channel` or run `pkg update` for the current channel",
            ));
        }
        let requests = args
            .packages()
            .iter()
            .map(|selector| CatalogInfoRequest::new(selector).ok_or_else(catalog_query_invalid))
            .collect::<Result<Vec<_>, _>>()?;
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;
        run_catalog_info(&mut broker, requests)
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
        confirm_destructive(
            policy.yes(),
            &format!("Remove {} package(s)?", args.packages().len()),
        )?;
        self.commit_state_edit(StateEditKind::Remove, |state| remove_state(state, args))
    }

    fn list(&mut self, args: &ListArgs) -> Result<CommandResult, CommandError> {
        let active = self.active()?;
        list_state(active.state(), args, None)
    }

    fn outdated(&mut self) -> Result<CommandResult, CommandError> {
        let active = self.active()?;
        let installed = installed_catalog_packages(active.state(), None)?;
        if installed.is_empty() {
            return outdated_catalog_reports(
                active.state().manifest().channel_seq(),
                &installed,
                &[],
            );
        }
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;
        run_catalog_outdated(
            &mut broker,
            active.state().manifest().channel_seq(),
            &installed,
        )
    }

    fn update(
        &mut self,
        args: &UpdateArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.require_broker_state()?;
        let mode = if args.check() || policy.dry_run() {
            ChannelRefreshMode::Check
        } else if args.force() {
            ChannelRefreshMode::Force
        } else {
            ChannelRefreshMode::Apply
        };
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;
        let report = refresh_channel_metadata(&mut broker, mode)?;
        if mode == ChannelRefreshMode::Check {
            return channel_refresh_result(report, mode, false);
        }
        let layout = self.layout();
        if layout
            .current_generation()
            .map_err(|_| mutation_failed())?
            .is_none()
        {
            return channel_refresh_result(report, mode, false);
        }
        let current = self.active()?.state().manifest().channel_seq();
        if report.sequence() == current {
            return channel_refresh_result(report, mode, false);
        }
        let result = channel_refresh_result(report, mode, true)?;
        self.commit_state_edit(StateEditKind::Update, |state| {
            let state = advance_channel(state, report.sequence()).map_err(|_| mutation_failed())?;
            Ok(LifecycleEdit::new(state, result))
        })
    }

    fn upgrade(
        &mut self,
        args: &UpgradeArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.upgrade_packages(args, policy)
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
            let layout = self.layout();
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
        args: &RepairArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.require_broker_state()?;
        if args.from_manifest().is_some() || args.from_lock() {
            return Err(CommandError::new(
                ExitCode::Config,
                "the requested state-reconstruction repair mode is not available",
                "omit --from-manifest and --from-lock to verify or repair a generation",
            ));
        }
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;
        self.repair_with_broker(&mut broker, args, policy)
    }
}

fn run_catalog_search(
    broker: &mut BrokerLifecycleClient,
    request: CatalogSearchRequest,
) -> Result<CommandResult, CommandError> {
    let handle = broker
        .begin(BrokerOperationKind::Resolve)
        .map_err(broker_error)?;
    let result = (|| {
        let report = broker
            .search_catalog(handle.clone(), request)
            .map_err(catalog_broker_error)?;
        broker.complete(handle.clone()).map_err(broker_error)?;
        search_catalog_report(&report)
    })();
    if result.is_err() {
        let _ = broker.cancel(handle);
    }
    result
}

fn run_catalog_info(
    broker: &mut BrokerLifecycleClient,
    requests: Vec<CatalogInfoRequest>,
) -> Result<CommandResult, CommandError> {
    let handle = broker
        .begin(BrokerOperationKind::Resolve)
        .map_err(broker_error)?;
    let result = (|| {
        let reports = broker
            .info_catalog(handle.clone(), requests)
            .map_err(catalog_broker_error)?;
        let result = info_catalog_reports(&reports)?;
        broker.complete(handle.clone()).map_err(broker_error)?;
        Ok(result)
    })();
    if result.is_err() {
        let _ = broker.cancel(handle);
    }
    result
}

fn run_catalog_outdated(
    broker: &mut BrokerLifecycleClient,
    installed_sequence: pkg_core::ChannelSequence,
    installed: &[InstalledCatalogPackage],
) -> Result<CommandResult, CommandError> {
    if installed.is_empty() {
        return outdated_catalog_reports(installed_sequence, &[], &[]);
    }
    let requests = installed
        .iter()
        .map(|package| CatalogInfoRequest::new(package.package()).ok_or_else(catalog_query_invalid))
        .collect::<Result<Vec<_>, _>>()?;
    let handle = broker
        .begin(BrokerOperationKind::Resolve)
        .map_err(broker_error)?;
    let result = (|| {
        let reports = broker
            .info_catalog(handle.clone(), requests)
            .map_err(catalog_broker_error)?;
        broker.complete(handle.clone()).map_err(broker_error)?;
        outdated_catalog_reports(installed_sequence, installed, &reports)
    })();
    if result.is_err() {
        let _ = broker.cancel(handle);
    }
    result
}

fn installed_catalog_packages(
    state: &pkg_core::lifecycle::LifecycleState,
    selected: Option<&BTreeSet<SelectorId>>,
) -> Result<Vec<InstalledCatalogPackage>, CommandError> {
    state
        .manifest()
        .entries()
        .iter()
        .filter(|desired| selected.is_none_or(|ids| ids.contains(desired.id())))
        .map(|desired| {
            let locked = state
                .locked()
                .entries()
                .get(desired.id())
                .ok_or_else(invalid_active_state)?;
            let realization = locked.realization();
            Ok(InstalledCatalogPackage::new(
                desired.attribute().clone(),
                realization.pname().to_owned(),
                realization.version().clone(),
                realization.nixpkgs_revision().clone(),
                desired.is_pinned(),
            ))
        })
        .collect()
}

fn invalid_active_state() -> CommandError {
    CommandError::new(
        ExitCode::StateCorrupt,
        "the active package generation is inconsistent",
        "run `pkg doctor` before managing packages",
    )
}

impl LocalStateOperations {
    fn upgrade_packages(
        &self,
        args: &UpgradeArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.require_broker_state()?;
        require_supported_upgrade_options(args)?;
        let layout = self.layout().clone();
        let source = self.active()?;
        let mut selection = select_upgrade(
            source.state().clone(),
            upgrade_scope(source.state(), args)?,
            args.bump_pinned(),
        )
        .map_err(upgrade_failed)?;
        let skipped_pinned = selection
            .skipped_pinned()
            .iter()
            .map(SelectorId::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let selected_ids = selection
            .selectors()
            .iter()
            .map(|selector| selector.id().clone())
            .collect::<BTreeSet<_>>();
        if selected_ids.is_empty() {
            return upgrade_noop_result(&skipped_pinned);
        }
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;
        let has_local_build = selected_ids.iter().any(|id| {
            source
                .state()
                .locked()
                .entries()
                .get(id)
                .is_some_and(|entry| entry.provenance() == "build:local")
        });
        if has_local_build && !policy.dry_run() && !args.bump_pinned() {
            let installed = installed_catalog_packages(source.state(), Some(&selected_ids))?;
            let currency = run_catalog_outdated(
                &mut broker,
                source.state().manifest().channel_seq(),
                &installed,
            )?;
            let outdated = outdated_attributes(&currency)?;
            if outdated.is_empty() {
                return upgrade_noop_result(&skipped_pinned);
            }
            let ids = selection
                .selectors()
                .iter()
                .filter(|selector| {
                    selector
                        .attribute()
                        .is_some_and(|attribute| outdated.contains(attribute.as_str()))
                })
                .map(|selector| selector.id().clone())
                .collect::<Vec<_>>();
            if ids.len() != outdated.len() {
                return Err(invalid_active_state());
            }
            selection = select_upgrade(source.state().clone(), UpgradeScope::Named(ids), false)
                .map_err(upgrade_failed)?;
        }
        let selector_names = selection
            .selectors()
            .iter()
            .map(|selector| {
                (
                    selector.id().clone(),
                    selector.selector().as_str().to_owned(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let selectors = broker_upgrade_selectors(selection.selectors());
        if policy.dry_run() {
            return preview_upgrade(&mut broker, selectors, &skipped_pinned);
        }
        self.recover_pending_install(&layout, &mut broker)?;
        let mut ignore_progress = |_| Ok(());
        let (handle, public_operation_id, evidence, build_approval) = acquire_install_evidence(
            &mut broker,
            selectors,
            policy,
            !args.no_build(),
            &mut ignore_progress,
        )?;
        let mut local_committed = false;
        let result = (|| {
            let plan = selection
                .bind_channel(evidence.channel_sequence(), evidence.revision().clone())
                .map_err(upgrade_failed)?;
            let created_at = utc_now()?;
            let upgraded = assemble_upgrade_evidence_state(plan, &evidence, &created_at)
                .map_err(|_| upgrade_failed(pkg_core::upgrade::UpgradeError::InvalidState))?;
            if !upgraded.changed() {
                let _ = broker.complete(handle.clone());
                return upgrade_noop_result(&skipped_pinned);
            }
            let upgraded_names = upgraded
                .upgraded()
                .iter()
                .map(|id| {
                    selector_names.get(id).cloned().ok_or_else(|| {
                        upgrade_failed(pkg_core::upgrade::UpgradeError::InvalidState)
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let next = upgraded.into_state();
            let nonce = secure_nonce()?;
            let identity = LeaseIdentity::new(&public_operation_id, &nonce, &created_at)
                .map_err(state_lease_error)?;
            let lease = StateLease::try_exclusive(&layout, &identity).map_err(state_lease_error)?;
            let history = load_retained_history(&layout, &lease).map_err(state_read_error)?;
            let newest = history
                .snapshots()
                .first()
                .ok_or_else(no_active_generation)?;
            let generation_id = next_generation_id(newest.generation().id())?;
            let prepared = prepare_state_edit(
                layout.clone(),
                lease,
                &source,
                &next,
                StateEditMetadata::new(
                    &generation_id,
                    &created_at,
                    &public_operation_id,
                    StateEditKind::Upgrade,
                )
                .with_collision_policy(state_collision_policy(args.collision_policy()))
                .with_build_approval(build_approval),
            )
            .map_err(|_| mutation_failed())?;
            let added_paths = install_output_paths(&evidence);
            let report = prepared
                .root_intent_from_source(
                    GenerationId::new(source.generation().id())
                        .map_err(|_| install_commit_failed())?,
                    &added_paths,
                )
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
            upgrade_result(
                &public_operation_id,
                &generation_id,
                &upgraded_names,
                &skipped_pinned,
                build_approval,
            )
        })();
        if result.is_err() && !local_committed {
            let _ = broker.cancel(handle);
        }
        result
    }

    fn install_packages(
        &self,
        args: &InstallArgs,
        policy: OperationPolicy,
        progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
    ) -> Result<CommandResult, CommandError> {
        self.require_broker_state()?;
        require_supported_install_options(args)?;
        let layout = self.layout().clone();
        let nonce = secure_nonce()?;
        let selectors = install_selectors(args, &nonce)?;
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;

        if policy.dry_run() {
            return preview_install(&mut broker, selectors);
        }
        self.recover_pending_install(&layout, &mut broker)?;

        let (handle, public_operation_id, evidence, build_approval) =
            acquire_install_evidence(&mut broker, selectors, policy, true, progress)?;
        let mut local_committed = false;
        let result = (|| {
            emit_phase(progress, &public_operation_id, "stage", "started")?;
            let created_at = utc_now()?;
            let identity = LeaseIdentity::new(&public_operation_id, &nonce, &created_at)
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
                    &public_operation_id,
                    build_approval,
                ),
            )
            .map_err(|error| map_install_generation_error(&error))?;
            emit_phase(progress, &public_operation_id, "stage", "completed")?;
            emit_phase(progress, &public_operation_id, "activate", "started")?;
            let added_paths = install_output_paths(&evidence);
            let intent = match current.as_ref() {
                Some(source) if !source.generation().activation().output_roots().is_empty() => {
                    prepared.root_intent_from_source(
                        GenerationId::new(source.generation().id())
                            .map_err(|_| install_commit_failed())?,
                        &added_paths,
                    )
                }
                _ => prepared.root_intent(),
            }
            .map_err(|_| install_commit_failed())?;
            let report = intent
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
        let mut reconnect = BrokerLifecycleClient::connect_default;
        self.recover_pending_install_with(layout, broker, &mut reconnect)
    }

    fn recover_pending_install_with(
        &self,
        layout: &StateLayout,
        broker: &mut BrokerLifecycleClient,
        reconnect: &mut dyn FnMut() -> Result<BrokerLifecycleClient, BrokerClientError>,
    ) -> Result<(), CommandError> {
        let probe = StateLease::try_shared(layout).map_err(state_lease_error)?;
        let pending_discard =
            pending_install_discard_generation(layout, &probe).map_err(state_read_error)?;
        drop(probe);
        if let Some(generation) = pending_discard {
            self.recover_pending_prunes(layout, broker)?;
            self.discard_unrooted_install_with(layout, broker, reconnect, &generation)?;
            return Ok(());
        }
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
        // `local_committed` marks the linearization point after which the
        // Activate operation must be completed, never cancelled.
        // `activate_cancelled` marks the AttestationFailed branch, which has
        // already cancelled the Activate handle before the nested GC discard.
        let mut local_committed = false;
        let mut activate_cancelled = false;
        let result = (|| {
            if current.as_ref() == Some(&pending) {
                let report = broker
                    .attest_generation_roots(handle.clone(), pending.clone())
                    .map_err(install_broker_error)?;
                let maintenance = AttestedRootMaintenance { report };
                recover_generation(layout, &lease, &pending, &maintenance)
                    .map_err(|_| install_commit_failed())?;
                local_committed = true;
                return complete_operation(broker, reconnect, handle.clone());
            }
            let prepared = resume_prepared_install(layout.clone(), lease, &pending)
                .map_err(|_| install_commit_failed())?;
            match broker.attest_generation_roots(handle.clone(), pending.clone()) {
                Ok(report) => {
                    prepared
                        .activate_published(Some(&report), &nonce)
                        .map_err(|_| install_commit_failed())?
                        .finish()
                        .map_err(|_| install_commit_failed())?;
                    local_committed = true;
                    complete_operation(broker, reconnect, handle.clone())
                }
                Err(error)
                    if error.generation_root_attestation_code()
                        == Some(GenerationRootAttestationErrorCode::AttestationFailed) =>
                {
                    drop(prepared);
                    if !cancel_operation(broker, reconnect, handle.clone()) {
                        return Err(install_broker_error(error));
                    }
                    activate_cancelled = true;
                    self.discard_unrooted_install_with(layout, broker, reconnect, &pending)
                }
                Err(error) => Err(install_broker_error(error)),
            }
        })();
        if result.is_err() && !local_committed && !activate_cancelled {
            cancel_operation(broker, reconnect, handle);
        }
        result
    }

    fn discard_unrooted_install_with(
        &self,
        layout: &StateLayout,
        broker: &mut BrokerLifecycleClient,
        reconnect: &mut dyn FnMut() -> Result<BrokerLifecycleClient, BrokerClientError>,
        generation: &pkg_nix::GenerationId,
    ) -> Result<(), CommandError> {
        let handle = broker
            .begin(BrokerOperationKind::Gc)
            .map_err(broker_error)?;
        let mut local_committed = false;
        let result = (|| {
            broker.acquire_gc(handle.clone()).map_err(broker_error)?;
            let (lease, _) = self.gc_lease(layout)?;
            let maintenance = BrokerGcMaintenance {
                broker: Mutex::new(&mut *broker),
                handle: handle.clone(),
            };
            recover_generation(layout, &lease, generation, &maintenance)
                .map_err(|_| install_commit_failed())?;
            drop(maintenance);
            local_committed = true;
            complete_operation(broker, reconnect, handle.clone())
        })();
        if result.is_err() && !local_committed {
            cancel_operation(broker, reconnect, handle);
        }
        result
    }

    fn preview_history_delete(&self, args: &HistoryArgs) -> Result<CommandResult, CommandError> {
        let generation = args.delete().ok_or_else(mutation_failed)?;
        let layout = self.layout();
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
        let layout = self.layout().clone();
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
            broker.acquire_gc(handle.clone()).map_err(broker_error)?;
            let (lease, operation_id) = self.gc_lease(&layout)?;
            let active = load_active_snapshot(&layout, &lease)
                .map_err(state_read_error)?
                .ok_or_else(no_active_generation)?;
            let history = load_retained_history(&layout, &lease).map_err(state_read_error)?;
            ensure_generation_deletable(&active, &history, generation)?;
            let candidate =
                plan_generation_prune(&active, history.snapshots(), generation, unix_now()?)
                    .map_err(|_| gc_failed())?;
            confirm_destructive(policy.yes(), &format!("Prune generation {generation}?"))?;
            let maintenance = BrokerGcMaintenance {
                broker: Mutex::new(&mut broker),
                handle: handle.clone(),
            };
            prune_generation(&layout, &lease, &candidate, &maintenance, &operation_id)
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
        let layout = self.layout();
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
        let layout = self.layout().clone();
        let mut broker = BrokerLifecycleClient::connect_default().map_err(broker_error)?;
        let recovered = self.recover_pending_prunes(&layout, &mut broker)?;
        self.recover_pending_state_edit(&layout, &mut broker)?;
        let handle = broker
            .begin(BrokerOperationKind::Gc)
            .map_err(broker_error)?;
        let result = (|| {
            broker.acquire_gc(handle.clone()).map_err(broker_error)?;
            let (lease, operation_id) = self.gc_lease(&layout)?;
            let active = load_active_snapshot(&layout, &lease)
                .map_err(state_read_error)?
                .ok_or_else(no_active_generation)?;
            let history = load_retained_history(&layout, &lease).map_err(state_read_error)?;
            let plan = plan_gc(&active, history.snapshots(), gc_policy(args)?, unix_now()?)
                .map_err(|_| gc_failed())?;
            require_gc_confirmation(policy, &plan)?;
            let maintenance = BrokerGcMaintenance {
                broker: Mutex::new(&mut broker),
                handle: handle.clone(),
            };
            let mut pruned = Vec::new();
            for candidate in plan.candidates() {
                if prune_generation(&layout, &lease, candidate, &maintenance, &operation_id)
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

    fn gc_lease(&self, layout: &StateLayout) -> Result<(StateLease, String), CommandError> {
        let nonce = secure_nonce()?;
        let created_at = utc_now()?;
        let operation_id = state_operation_id(&nonce);
        let identity =
            LeaseIdentity::new(&operation_id, &nonce, &created_at).map_err(state_lease_error)?;
        let lease = StateLease::try_exclusive(layout, &identity).map_err(state_lease_error)?;
        Ok((lease, operation_id))
    }

    fn commit_rollback(&self, args: &RollbackArgs) -> Result<CommandResult, CommandError> {
        self.require_broker_state()?;
        let layout = self.layout().clone();
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
            let operation_id = state_operation_id(&nonce);
            let identity = LeaseIdentity::new(&operation_id, &nonce, &created_at)
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
                &operation_id,
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
        let layout = self.layout().clone();
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
            let operation_id = state_operation_id(&nonce);
            let identity = LeaseIdentity::new(&operation_id, &nonce, &created_at)
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
                &next,
                StateEditMetadata::new(&generation_id, &created_at, &operation_id, kind),
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
            broker.acquire_gc(handle.clone()).map_err(broker_error)?;
            let (lease, _) = self.gc_lease(layout)?;
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

    fn repair_with_broker(
        &self,
        broker: &mut BrokerLifecycleClient,
        args: &RepairArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        let layout = self.layout().clone();
        let verify_only = args.verify_only() || policy.dry_run();
        if !verify_only {
            write_repair_warning()?;
            confirm_destructive(
                policy.yes(),
                "Repair can temporarily make affected commands unavailable. Continue?",
            )?;
        }
        let mut handle: Option<OperationHandle> = None;
        let result = (|| {
            let (lease, _) = self.gc_lease(&layout)?;
            let generation = match args.generation() {
                Some(generation) => pkg_nix::GenerationId::new(generation).map_err(|_| {
                    CommandError::new(
                        ExitCode::Usage,
                        "the repair generation identifier is invalid",
                        "use an identifier shown by `pkg history`",
                    )
                })?,
                None => {
                    let active = load_active_snapshot(&layout, &lease)
                        .map_err(state_read_error)?
                        .ok_or_else(no_active_generation)?;
                    pkg_nix::GenerationId::new(active.generation().id()).map_err(|_| {
                        CommandError::new(
                            ExitCode::StateCorrupt,
                            "the active generation identifier is invalid",
                            "run `pkg doctor` before repairing packages",
                        )
                    })?
                }
            };
            let opened = broker
                .begin(BrokerOperationKind::Repair)
                .map_err(broker_error)?;
            handle = Some(opened.clone());
            if verify_only {
                // The Broker-held GC inhibitor now protects the selected
                // generation and its roots. Release the exclusive state lease
                // before the long read-only verification.
                drop(lease);
            }
            let mut report = broker
                .repair_generation(
                    opened,
                    RepairGenerationRequest::new(generation.clone(), verify_only),
                )
                .map_err(repair_broker_error)?;
            let mut approved_preview = None;
            if report.status() == RepairGenerationStatus::NeedsApproval {
                let preview = report.build_preview().ok_or_else(|| {
                    CommandError::new(
                        ExitCode::EngineUnavailable,
                        "the repair service returned no build preview",
                        "retry after running `pkg doctor`",
                    )
                })?;
                render_build_preview(preview)?;
                approved_preview = Some(
                    preview
                        .to_json_value()
                        .map_err(|_| install_commit_failed())?,
                );
                confirm_destructive(policy.yes(), "Rebuild the damaged packages locally?")?;
                let source = if policy.yes() {
                    ApprovalSource::AssumeYes
                } else {
                    ApprovalSource::Interactive
                };
                let digest = parse_build_plan_digest(preview.build_plan_digest())?;
                let opened = broker
                    .begin(BrokerOperationKind::Repair)
                    .map_err(broker_error)?;
                handle = Some(opened.clone());
                report = broker
                    .repair_generation(
                        opened,
                        RepairGenerationRequest::with_approval(
                            generation.clone(),
                            pkg_nix::BuildApprovalRequest::new(digest, source),
                        ),
                    )
                    .map_err(repair_broker_error)?;
            }
            match report.status() {
                RepairGenerationStatus::Clean => repair_result(
                    "The generation is clean.",
                    &generation,
                    "clean",
                    0,
                    verify_only,
                    approved_preview,
                ),
                RepairGenerationStatus::RepairedFromCache => repair_result(
                    "The generation was repaired from the signed cache.",
                    &generation,
                    "repaired-from-cache",
                    0,
                    false,
                    approved_preview,
                ),
                RepairGenerationStatus::RepairedByBuild => repair_result(
                    "The generation was repaired by an approved local build.",
                    &generation,
                    "repaired-by-build",
                    0,
                    false,
                    approved_preview,
                ),
                RepairGenerationStatus::DamageDetected => Err(CommandError::new(
                    ExitCode::VerifyFail,
                    format!(
                        "verification found {} damaged store path(s)",
                        report.damaged_paths()
                    ),
                    "run `pkg repair` without --verify-only to restore signed cache paths",
                )),
                RepairGenerationStatus::NeedsApproval => Err(CommandError::new(
                    ExitCode::AcquireNeedsApproval,
                    format!(
                        "{} damaged store path(s) require an approved local rebuild",
                        report.damaged_paths()
                    ),
                    "retry and approve the newly displayed repair build plan",
                )),
            }
        })();
        if result.is_err()
            && let Some(handle) = handle
        {
            let _ = broker.cancel(handle);
        }
        result
    }
}

/// Completes one committed Broker operation and reconciles an uncertain reply.
///
/// A lost completion acknowledgement never returns success while it is
/// uncertain. The caller polls the exact handle on a fresh same-uid connection.
/// A confirmed completed report is success. A confirmed live handle is
/// cancelled before the original error is returned.
fn complete_operation(
    broker: &mut BrokerLifecycleClient,
    reconnect: &mut dyn FnMut() -> Result<BrokerLifecycleClient, BrokerClientError>,
    handle: OperationHandle,
) -> Result<(), CommandError> {
    match broker.complete(handle.clone()) {
        Ok(()) => Ok(()),
        Err(error) => reconcile_completion(reconnect, handle, error),
    }
}

/// Reconciles one operation after a lost completion reply on a fresh connection.
fn reconcile_completion(
    reconnect: &mut dyn FnMut() -> Result<BrokerLifecycleClient, BrokerClientError>,
    handle: OperationHandle,
    first_error: BrokerClientError,
) -> Result<(), CommandError> {
    match reconnect() {
        Ok(mut fresh) => match fresh.poll(handle.clone()) {
            Ok(OperationStatus::Completed) => Ok(()),
            Ok(OperationStatus::Running) => {
                cancel_operation(&mut fresh, reconnect, handle);
                Err(broker_error(first_error))
            }
            Ok(OperationStatus::Cancelled) => Err(broker_error(first_error)),
            Err(_) => {
                cancel_operation(&mut fresh, reconnect, handle);
                Err(broker_error(first_error))
            }
        },
        Err(_) => Err(broker_error(first_error)),
    }
}

/// Cancels one still-live Broker operation, falling back to a fresh same-uid
/// connection when the current connection is poisoned or otherwise unusable.
///
/// Returns `true` only after a cancellation acknowledgement or an exact
/// `Cancelled` status was observed.
/// Cleanup never overrides the caller's first functional error.
fn cancel_operation(
    broker: &mut BrokerLifecycleClient,
    reconnect: &mut dyn FnMut() -> Result<BrokerLifecycleClient, BrokerClientError>,
    handle: OperationHandle,
) -> bool {
    if broker.cancel(handle.clone()).is_ok() {
        return true;
    }
    let Ok(mut fresh) = reconnect() else {
        return false;
    };
    match fresh.poll(handle.clone()) {
        Ok(OperationStatus::Cancelled) => {
            *broker = fresh;
            true
        }
        Ok(OperationStatus::Running) => match fresh.cancel(handle.clone()) {
            Ok(()) => {
                *broker = fresh;
                true
            }
            Err(_) => {
                let Ok(mut final_client) = reconnect() else {
                    return false;
                };
                if final_client.poll(handle) == Ok(OperationStatus::Cancelled) {
                    *broker = final_client;
                    true
                } else {
                    false
                }
            }
        },
        Ok(OperationStatus::Completed) | Err(_) => false,
    }
}

fn outdated_attributes(result: &CommandResult) -> Result<BTreeSet<String>, CommandError> {
    result
        .fields()
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(invalid_active_state)?
        .iter()
        .map(|entry| {
            entry
                .get("package")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(invalid_active_state)
        })
        .collect()
}

fn require_gc_confirmation(
    policy: OperationPolicy,
    plan: &pkg_store::GcPlan,
) -> Result<(), CommandError> {
    let generations = plan
        .candidates()
        .iter()
        .map(pkg_store::PruneCandidate::generation_id)
        .collect::<Vec<_>>()
        .join(", ");
    confirm_destructive(
        policy.yes(),
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
    allow_build: bool,
    progress: &mut dyn FnMut(PublicEvent) -> Result<(), CommandError>,
) -> Result<AcquiredInstallEvidence, CommandError> {
    let acquire_handle = broker
        .begin(BrokerOperationKind::Acquire)
        .map_err(broker_error)?;
    let public_operation_id = format!("op_{}", secure_nonce()?);
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
            let cache_code = error.cache_install_code();
            let fallback = install_broker_error(error);
            if matches!(
                cache_code,
                Some(
                    CacheInstallErrorCode::InvalidIntent | CacheInstallErrorCode::AcquisitionFailed
                )
            ) && let Some(diagnostic) = diagnose_install_selector_error(broker, &selectors)
            {
                return Err(diagnostic);
            }
            return Err(fallback);
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
    if !allow_build {
        return Err(CommandError::new(
            ExitCode::AcquireNoBinary,
            "one or more packages require a local build",
            "remove `--no-build` to review and approve the sandboxed build",
        ));
    }

    let build_handle = broker
        .begin(BrokerOperationKind::Build)
        .map_err(broker_error)?;
    if let Err(error) = emit_phase(progress, &public_operation_id, "build", "started") {
        let _ = broker.cancel(build_handle);
        return Err(error);
    }
    let result = (|| {
        let preview = match broker.prepare_build(build_handle.clone(), selectors) {
            Ok(preview) => preview,
            Err(error) => {
                if let Some(code) = error.build_preparation_code() {
                    emit_phase(
                        progress,
                        &public_operation_id,
                        "build_prepare",
                        code.as_str(),
                    )?;
                }
                return Err(install_broker_error(error));
            }
        };
        if !policy.yes() {
            render_build_preview(&preview)?;
        }
        confirm_destructive(policy.yes(), "Build the missing packages locally?")?;
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

fn diagnose_install_selector_error(
    broker: &mut BrokerLifecycleClient,
    selectors: &[PackageSelector],
) -> Option<CommandError> {
    let requests = selectors
        .iter()
        .map(|selector| CatalogInfoRequest::new(selector.selector().as_str()))
        .collect::<Option<Vec<_>>>()?;
    run_catalog_info(broker, requests)
        .err()
        .filter(|error| error.exit_code() == ExitCode::ResolveFailed)
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
            let value = public_build_preview(&preview)?;
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

fn preview_upgrade(
    broker: &mut BrokerLifecycleClient,
    selectors: Vec<PackageSelector>,
    skipped_pinned: &[String],
) -> Result<CommandResult, CommandError> {
    let handle = broker
        .begin(BrokerOperationKind::Build)
        .map_err(broker_error)?;
    let result = broker
        .prepare_build(handle.clone(), selectors)
        .map_err(install_broker_error)
        .and_then(|preview| {
            let value = public_build_preview(&preview)?;
            CommandResult::new(
                "Upgrade preview is ready. No package was downloaded or activated.",
                Map::from_iter([
                    ("dryRun".into(), json!(true)),
                    ("preflight".into(), value),
                    ("skippedPinned".into(), json!(skipped_pinned)),
                ]),
                Vec::new(),
            )
            .map_err(|_| install_commit_failed())
        });
    let _ = broker.cancel(handle);
    result
}

fn public_build_preview(preview: &BuildPreview) -> Result<Value, CommandError> {
    let mut value = preview
        .to_json_value()
        .map_err(|_| install_commit_failed())?;
    value
        .as_object_mut()
        .and_then(|fields| fields.remove("schemaVersion"))
        .ok_or_else(install_commit_failed)?;
    Ok(value)
}

fn upgrade_scope(
    state: &pkg_core::lifecycle::LifecycleState,
    args: &UpgradeArgs,
) -> Result<UpgradeScope, CommandError> {
    if args.all() {
        return Ok(UpgradeScope::All);
    }
    let ids = args
        .packages()
        .iter()
        .map(|name| {
            let matches = state
                .manifest()
                .entries()
                .iter()
                .filter(|entry| entry.id().as_str() == name || entry.selector().as_str() == name)
                .map(|entry| entry.id().clone())
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [id] => Ok(id.clone()),
                [] => Err(CommandError::new(
                    ExitCode::ResolveFailed,
                    "package is not installed",
                    "run `pkg list` and use an installed selector",
                )),
                _ => Err(CommandError::new(
                    ExitCode::ResolveFailed,
                    "installed selector is ambiguous",
                    "use the stable selector id from machine output",
                )),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(UpgradeScope::Named(ids))
}

fn broker_upgrade_selectors(selectors: &[PackageSelector]) -> Vec<PackageSelector> {
    selectors
        .iter()
        .map(|selector| {
            PackageSelector::new(
                selector.id().clone(),
                selector.selector().clone(),
                selector.version_preference().clone(),
                selector.outputs().clone(),
                SourceRevision::CurrentChannel,
            )
        })
        .collect()
}

fn require_supported_upgrade_options(args: &UpgradeArgs) -> Result<(), CommandError> {
    if args.channel().is_some()
        || !args.outputs().is_empty()
        || args.keep_going()
        || args.include_removed_upstream()
    {
        return Err(CommandError::new(
            ExitCode::Config,
            "the requested upgrade mode is not available",
            "use the current channel without --with-outputs, --keep-going, or --include-removed-upstream",
        ));
    }
    Ok(())
}

fn upgrade_noop_result(skipped_pinned: &[String]) -> Result<CommandResult, CommandError> {
    CommandResult::new(
        "No eligible package changed.",
        Map::from_iter([
            ("generation".into(), serde_json::Value::Null),
            ("upgraded".into(), json!([])),
            ("skippedPinned".into(), json!(skipped_pinned)),
            ("buildApproval".into(), json!("not_required")),
        ]),
        Vec::new(),
    )
    .map_err(|_| mutation_failed())
}

fn upgrade_result(
    operation_id: &str,
    generation_id: &str,
    upgraded: &[String],
    skipped_pinned: &[String],
    build_approval: &str,
) -> Result<CommandResult, CommandError> {
    CommandResult::new(
        format!("Upgraded {} package(s) as {generation_id}.", upgraded.len()),
        Map::from_iter([
            ("operationId".into(), json!(operation_id)),
            ("generation".into(), json!(generation_id)),
            ("upgraded".into(), json!(upgraded)),
            ("skippedPinned".into(), json!(skipped_pinned)),
            ("buildApproval".into(), json!(build_approval)),
        ]),
        Vec::new(),
    )
    .map_err(|_| mutation_failed())
}

fn upgrade_failed(error: pkg_core::upgrade::UpgradeError) -> CommandError {
    let (message, hint) = match error {
        pkg_core::upgrade::UpgradeError::NotInstalled => (
            "package is not installed",
            "run `pkg list` and use an installed selector",
        ),
        pkg_core::upgrade::UpgradeError::SequenceRollback => (
            "the authenticated channel is older than active package state",
            "run `pkg update`; report the issue if the channel remains older",
        ),
        _ => (
            "the package upgrade could not be applied safely",
            "run `pkg doctor`; retry after the reported issue is resolved",
        ),
    };
    CommandError::new(ExitCode::ResolveFailed, message, hint)
}

fn install_output_paths(evidence: &InstallEvidence) -> Vec<pkg_core::identity::StorePath> {
    evidence
        .targets()
        .iter()
        .flat_map(pkg_nix::InstallTargetEvidence::acquired)
        .map(|output| output.path_info().store_path().clone())
        .collect()
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
        BrokerClientErrorCode::BuildPreparationRefused => ExitCode::EngineUnavailable,
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

fn map_install_generation_error(error: &InstallGenerationError) -> CommandError {
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
        InstallGenerationError::InvalidEvidence(InstallStateError::AlreadyInstalled) => {
            CommandError::new(
                ExitCode::PreflightFail,
                "one or more requested packages are already installed",
                "run `pkg upgrade`, or remove the package before you install it again",
            )
        }
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

/// Requests one terminal confirmation unless the caller supplied `--yes`.
///
/// # Errors
///
/// Returns a stable public refusal when confirmation is unavailable or denied.
pub fn confirm_destructive(yes: bool, prompt: &str) -> Result<(), CommandError> {
    if yes {
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
        .map(pkg_store::PruneCandidate::generation_id)
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

fn secure_nonce() -> Result<String, CommandError> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| mutation_failed())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn state_operation_id(nonce: &str) -> String {
    format!("op_{nonce}")
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

fn repair_broker_error(error: BrokerClientError) -> CommandError {
    let (exit, message, hint) = match error.code() {
        BrokerClientErrorCode::RepairInvalidScope => (
            ExitCode::ResolveFailed,
            "the selected rooted generation cannot be repaired",
            "use a generation shown by `pkg history`, then retry",
        ),
        BrokerClientErrorCode::RepairVerifyFailed | BrokerClientErrorCode::RepairStillDamaged => (
            ExitCode::VerifyFail,
            "generation integrity verification failed",
            "run `pkg doctor`, then retry the repair",
        ),
        BrokerClientErrorCode::RepairAdmissionFailed => (
            ExitCode::StateLocked,
            "another managed operation prevents repair",
            "wait for the active operation, then retry",
        ),
        BrokerClientErrorCode::RepairHelperFailed => (
            ExitCode::Permission,
            "the privileged repair helper refused the operation",
            "run `pkg doctor` to inspect helper readiness",
        ),
        BrokerClientErrorCode::RepairJournalFailed => (
            ExitCode::StateCorrupt,
            "the durable repair journal is unavailable or invalid",
            "run `pkg doctor` before retrying repair",
        ),
        BrokerClientErrorCode::RepairFreshApprovalRequired => (
            ExitCode::AcquireNeedsApproval,
            "the local repair build requires a fresh approval",
            "review the new repair build preview before approving it",
        ),
        BrokerClientErrorCode::RepairAuthorityUnavailable => (
            ExitCode::EngineUnavailable,
            "the production repair authority is unavailable",
            "run `pkg doctor` to inspect managed service readiness",
        ),
        _ => return broker_error(error),
    };
    CommandError::new(exit, message, hint)
}

fn write_repair_warning() -> Result<(), CommandError> {
    let mut stderr = io::stderr();
    writeln!(
        stderr,
        "Warning: repair is non-atomic. Affected commands can be temporarily unavailable."
    )
    .and_then(|()| stderr.flush())
    .map_err(|_| {
        CommandError::new(
            ExitCode::Config,
            "the repair warning could not be displayed",
            "retry from a terminal with a writable standard error stream",
        )
    })
}

fn repair_result(
    summary: &str,
    generation: &pkg_nix::GenerationId,
    status: &str,
    damaged_paths: u32,
    verify_only: bool,
    build_preview: Option<serde_json::Value>,
) -> Result<CommandResult, CommandError> {
    let mut fields = Map::from_iter([
        ("generation".into(), json!(generation.as_str())),
        ("status".into(), json!(status)),
        ("damagedPathCount".into(), json!(damaged_paths)),
        ("verifyOnly".into(), json!(verify_only)),
        ("nonAtomic".into(), json!(!verify_only)),
    ]);
    if let Some(preview) = build_preview {
        fields.insert("buildPreview".into(), preview);
    }
    CommandResult::new(summary, fields, Vec::new()).map_err(|_| mutation_failed())
}

fn catalog_broker_error(error: BrokerClientError) -> CommandError {
    if error.code() == BrokerClientErrorCode::CatalogQueryRefused {
        index_unavailable()
    } else {
        broker_error(error)
    }
}

fn catalog_query_invalid() -> CommandError {
    CommandError::new(
        ExitCode::ResolveFailed,
        "package query was invalid",
        "use a bounded query and a valid display license identifier",
    )
}

fn channel_refresh_error(error: BrokerClientError) -> CommandError {
    let (exit_code, message, hint) = channel_refresh_error_fields(error.code());
    CommandError::new(exit_code, message, hint)
}

const fn channel_refresh_error_fields(
    code: BrokerClientErrorCode,
) -> (ExitCode, &'static str, &'static str) {
    match code {
        BrokerClientErrorCode::ChannelRefreshNetwork => (
            ExitCode::AcquireNetwork,
            "signed channel metadata could not be downloaded",
            "check network access, then retry `pkg update`",
        ),
        BrokerClientErrorCode::ChannelRefreshVerification => (
            ExitCode::VerifyFail,
            "signed channel metadata was refused",
            "check system time, then retry `pkg update`",
        ),
        BrokerClientErrorCode::ChannelRefreshBusy => (
            ExitCode::StateLocked,
            "another channel refresh is active",
            "wait for the active refresh, then retry `pkg update`",
        ),
        BrokerClientErrorCode::ChannelRefreshServiceUnavailable => (
            ExitCode::EngineUnavailable,
            "the managed package service refused the transaction",
            "run `pkg doctor` to inspect managed broker readiness",
        ),
        _ => (
            ExitCode::EngineUnavailable,
            "the managed package service refused the transaction",
            "run `pkg doctor` to inspect managed broker readiness",
        ),
    }
}

fn refresh_channel_metadata(
    broker: &mut BrokerLifecycleClient,
    mode: ChannelRefreshMode,
) -> Result<ChannelRefreshReport, CommandError> {
    let handle = broker
        .begin(BrokerOperationKind::Refresh)
        .map_err(broker_error)?;
    let result = (|| {
        let report = broker
            .refresh_channel(handle.clone(), mode)
            .map_err(channel_refresh_error)?;
        broker.complete(handle.clone()).map_err(broker_error)?;
        Ok(report)
    })();
    if result.is_err() {
        let _ = broker.cancel(handle);
    }
    result
}

fn channel_refresh_result(
    report: ChannelRefreshReport,
    mode: ChannelRefreshMode,
    state_updated: bool,
) -> Result<CommandResult, CommandError> {
    let message = if mode == ChannelRefreshMode::Check {
        if report.updated() {
            format!(
                "New channel metadata is available at sequence {}.",
                report.sequence()
            )
        } else {
            format!(
                "Channel metadata is current at sequence {}.",
                report.sequence()
            )
        }
    } else if report.updated() {
        format!(
            "Channel metadata updated to sequence {}.",
            report.sequence()
        )
    } else {
        format!(
            "Channel metadata is current at sequence {}.",
            report.sequence()
        )
    };
    CommandResult::new(
        message,
        Map::from_iter([
            ("updated".into(), json!(report.updated())),
            (
                "checkedOnly".into(),
                json!(mode == ChannelRefreshMode::Check),
            ),
            ("stateUpdated".into(), json!(state_updated)),
            (
                "channelSequence".into(),
                json!(report.sequence().get().get()),
            ),
        ]),
        Vec::new(),
    )
    .map_err(|_| mutation_failed())
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

#[cfg(test)]
mod tests;
