//! Production broker authority for path-free generation repair.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use pkg_nix::{
    AuthenticatedCaller, BuildApprovalReceipt, OperationHandle, RealNixAdapter,
    RepairGenerationErrorCode, RepairGenerationReport, RepairGenerationRequest,
    RepairGenerationStatus, RootRepairPlanProof, RootRepairPlanRequest, RootSetAttestationRequest,
    StorePath, verify_closure,
};
use pkg_pipeline::AuthenticatedBuildAuthority;

use crate::{
    BrokerCallerApprovalJournal, BrokerRepairJournals, RepairApprovalGate, RepairApprovalScope,
    RepairAuthorityDispatch, RepairCoordinatorError, RepairCoordinatorErrorCode, RepairJournal,
    RepairRequest, RepairResult, RootHelperClient, recover_repair, repair_generation,
};

/// Broker-private production owner of generation repair inputs and execution.
pub struct ProductionRepairAuthority {
    adapter: Arc<RealNixAdapter>,
    roots: Arc<RootHelperClient>,
    build_authority: Arc<AuthenticatedBuildAuthority>,
    journals: BrokerRepairJournals,
}

impl ProductionRepairAuthority {
    /// Binds repair to the broker's authenticated Nix, root, channel, and journal authorities.
    #[must_use]
    pub const fn new(
        adapter: Arc<RealNixAdapter>,
        roots: Arc<RootHelperClient>,
        build_authority: Arc<AuthenticatedBuildAuthority>,
        journals: BrokerRepairJournals,
    ) -> Self {
        Self {
            adapter,
            roots,
            build_authority,
            journals,
        }
    }
}

impl RepairAuthorityDispatch for ProductionRepairAuthority {
    fn repair(
        &self,
        caller: &AuthenticatedCaller,
        handle: &OperationHandle,
        request: &RepairGenerationRequest,
        approval_journal: Option<&BrokerCallerApprovalJournal>,
    ) -> Result<RepairGenerationReport, RepairGenerationErrorCode> {
        let owner_uid = caller
            .authorize_repair(handle)
            .map_err(|_| RepairGenerationErrorCode::AdmissionFailed)?;
        let root_set = self
            .roots
            .load_repair_root_set(&RootSetAttestationRequest::new(
                owner_uid,
                request.generation().clone(),
            ))
            .map_err(|_| RepairGenerationErrorCode::HelperFailed)?;
        if root_set.owner_uid() != owner_uid || root_set.generation() != request.generation() {
            return Err(RepairGenerationErrorCode::InvalidScope);
        }
        let roots = root_set
            .entries()
            .iter()
            .map(|entry| entry.target().clone())
            .collect::<Vec<_>>();
        let closure = self
            .adapter
            .closure_for_roots(&roots)
            .map_err(|_| RepairGenerationErrorCode::InvalidScope)?;
        let policy_version = self
            .build_authority
            .policy_version()
            .map_err(|_| RepairGenerationErrorCode::AuthorityUnavailable)?;
        let approved_build = request
            .approval()
            .map(|approval| {
                let journal =
                    approval_journal.ok_or(RepairGenerationErrorCode::AuthorityUnavailable)?;
                let damage = verify_closure(self.adapter.as_ref(), closure.iter().cloned())
                    .map_err(|_| RepairGenerationErrorCode::VerifyFailed)?;
                if damage.is_clean() {
                    return Err(RepairGenerationErrorCode::FreshApprovalRequired);
                }
                let plan = self.repair_plan(damage.damaged(), policy_version)?;
                if plan.digest() != approval.build_plan_digest() {
                    return Err(RepairGenerationErrorCode::FreshApprovalRequired);
                }
                let timestamp = approval_timestamp()?;
                self.build_authority
                    .under_current_policy(policy_version, || {
                        caller.approve_repair_subject(
                            handle,
                            approval.build_plan_digest(),
                            policy_version,
                            approval.source(),
                            &timestamp,
                            journal,
                        )
                    })
                    .map_err(|_| RepairGenerationErrorCode::FreshApprovalRequired)?
                    .map_err(|_| RepairGenerationErrorCode::FreshApprovalRequired)
            })
            .transpose()?;
        let repair_request = RepairRequest::new(
            owner_uid,
            request.generation().clone(),
            closure.clone(),
            policy_version,
            request.verify_only(),
            approved_build,
        )
        .map_err(map_repair_error)?;
        let mut journal = self
            .journals
            .for_generation(owner_uid, request.generation().clone())
            .map_err(map_repair_error)?;
        let recovery = recover_repair(journal.entries()).map_err(map_repair_error)?;
        if recovery.iter().any(|action| {
            let path = match action {
                crate::RepairRecoveryAction::RetryCacheOnly(path)
                | crate::RepairRecoveryAction::NeedsFreshApproval(path) => path,
            };
            !closure.contains(path)
        }) {
            return Err(RepairGenerationErrorCode::InvalidScope);
        }
        let gate = ProductionRepairApproval {
            caller,
            handle,
            adapter: self.adapter.as_ref(),
            build_authority: self.build_authority.as_ref(),
        };
        let result = repair_generation(
            &repair_request,
            self.adapter.as_ref(),
            caller,
            handle,
            self.roots.as_ref(),
            &gate,
            &mut journal,
        )
        .map_err(map_repair_error)?;
        self.report_result(result, &closure)
    }
}

impl ProductionRepairAuthority {
    fn repair_plan(
        &self,
        damaged: &[StorePath],
        policy_version: pkg_core::PolicyVersion,
    ) -> Result<RootRepairPlanProof, RepairGenerationErrorCode> {
        let (current_policy, facts) = self
            .build_authority
            .repair_build_context()
            .map_err(|_| RepairGenerationErrorCode::AuthorityUnavailable)?;
        if current_policy != policy_version {
            return Err(RepairGenerationErrorCode::FreshApprovalRequired);
        }
        let request = RootRepairPlanRequest::new(
            damaged.to_vec(),
            policy_version,
            facts.system(),
            facts.readiness().clone(),
            facts.host_cores(),
        )
        .ok_or(RepairGenerationErrorCode::InvalidScope)?;
        self.adapter
            .repair_plan_proof(&request)
            .map_err(|_| RepairGenerationErrorCode::StillDamaged)
    }

    fn report_result(
        &self,
        result: RepairResult,
        closure: &[StorePath],
    ) -> Result<RepairGenerationReport, RepairGenerationErrorCode> {
        if result == RepairResult::NeedsApproval {
            let report = verify_closure(self.adapter.as_ref(), closure.iter().cloned())
                .map_err(|_| RepairGenerationErrorCode::VerifyFailed)?;
            let count = u32::try_from(report.damaged().len())
                .map_err(|_| RepairGenerationErrorCode::VerifyFailed)?;
            let policy_version = self
                .build_authority
                .policy_version()
                .map_err(|_| RepairGenerationErrorCode::AuthorityUnavailable)?;
            let preview = self
                .repair_plan(report.damaged(), policy_version)?
                .preview()
                .clone();
            return RepairGenerationReport::needs_approval(count, preview)
                .map_err(|_| RepairGenerationErrorCode::VerifyFailed);
        }
        report_terminal_result(result, self.adapter.as_ref(), closure)
    }
}

struct ProductionRepairApproval<'a> {
    caller: &'a AuthenticatedCaller,
    handle: &'a OperationHandle,
    adapter: &'a RealNixAdapter,
    build_authority: &'a AuthenticatedBuildAuthority,
}

impl RepairApprovalGate for ProductionRepairApproval<'_> {
    fn consume(
        &self,
        receipt: &BuildApprovalReceipt,
        scope: &RepairApprovalScope,
    ) -> Result<(), RepairCoordinatorError> {
        let (current_policy, facts) =
            self.build_authority.repair_build_context().map_err(|_| {
                RepairCoordinatorError::new(RepairCoordinatorErrorCode::FreshApprovalRequired)
            })?;
        if current_policy != scope.policy_version() {
            return Err(RepairCoordinatorError::new(
                RepairCoordinatorErrorCode::FreshApprovalRequired,
            ));
        }
        let request = RootRepairPlanRequest::new(
            scope.paths().to_vec(),
            scope.policy_version(),
            facts.system(),
            facts.readiness().clone(),
            facts.host_cores(),
        )
        .ok_or_else(|| {
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::FreshApprovalRequired)
        })?;
        let proof = self.adapter.repair_plan_proof(&request).map_err(|_| {
            RepairCoordinatorError::new(RepairCoordinatorErrorCode::FreshApprovalRequired)
        })?;
        let digest = proof.digest();
        if digest != scope.build_plan_digest() {
            return Err(RepairCoordinatorError::new(
                RepairCoordinatorErrorCode::FreshApprovalRequired,
            ));
        }
        self.build_authority
            .under_current_policy(scope.policy_version(), || {
                self.caller.consume_repair_subject(
                    self.handle,
                    receipt,
                    digest,
                    scope.policy_version(),
                )
            })
            .map_err(|_| {
                RepairCoordinatorError::new(RepairCoordinatorErrorCode::FreshApprovalRequired)
            })?
            .map_err(|_| {
                RepairCoordinatorError::new(RepairCoordinatorErrorCode::FreshApprovalRequired)
            })
    }
}

fn report_terminal_result(
    result: RepairResult,
    adapter: &RealNixAdapter,
    closure: &[StorePath],
) -> Result<RepairGenerationReport, RepairGenerationErrorCode> {
    let (status, damaged_paths) = match result {
        RepairResult::Clean => (RepairGenerationStatus::Clean, 0),
        RepairResult::RepairedFromCache => (RepairGenerationStatus::RepairedFromCache, 0),
        RepairResult::RepairedByBuild => (RepairGenerationStatus::RepairedByBuild, 0),
        RepairResult::DamageDetected => (
            RepairGenerationStatus::DamageDetected,
            damage_count(adapter, closure)?,
        ),
        RepairResult::NeedsApproval => return Err(RepairGenerationErrorCode::InvalidScope),
    };
    RepairGenerationReport::new(status, damaged_paths)
        .map_err(|_| RepairGenerationErrorCode::VerifyFailed)
}

fn approval_timestamp() -> Result<String, RepairGenerationErrorCode> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RepairGenerationErrorCode::AuthorityUnavailable)?;
    Ok(format!("unix-ms:{}", elapsed.as_millis()))
}

fn damage_count(
    adapter: &RealNixAdapter,
    closure: &[StorePath],
) -> Result<u32, RepairGenerationErrorCode> {
    let report = verify_closure(adapter, closure.iter().cloned())
        .map_err(|_| RepairGenerationErrorCode::VerifyFailed)?;
    u32::try_from(report.damaged().len()).map_err(|_| RepairGenerationErrorCode::VerifyFailed)
}

const fn map_repair_error(error: RepairCoordinatorError) -> RepairGenerationErrorCode {
    match error.code() {
        RepairCoordinatorErrorCode::ValidationFailure => RepairGenerationErrorCode::InvalidScope,
        RepairCoordinatorErrorCode::VerifyFailure => RepairGenerationErrorCode::VerifyFailed,
        RepairCoordinatorErrorCode::AdmissionFailure => RepairGenerationErrorCode::AdmissionFailed,
        RepairCoordinatorErrorCode::HelperFailure => RepairGenerationErrorCode::HelperFailed,
        RepairCoordinatorErrorCode::JournalFailure => RepairGenerationErrorCode::JournalFailed,
        RepairCoordinatorErrorCode::StillDamaged => RepairGenerationErrorCode::StillDamaged,
        RepairCoordinatorErrorCode::FreshApprovalRequired => {
            RepairGenerationErrorCode::FreshApprovalRequired
        }
    }
}
