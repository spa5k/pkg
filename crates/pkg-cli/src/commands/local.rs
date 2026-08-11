//! Production command adapter over the invoking user's verified local state.

use std::path::Path;

use crate::cli::{
    GcArgs, HistoryArgs, InfoArgs, InstallArgs, ListArgs, PackageArgs, RemoveArgs, RepairArgs,
    RollbackArgs, SearchArgs, UpdateArgs, UpgradeArgs,
};
use crate::commands::execute::{CommandResult, CoreOperations, OperationPolicy};
use crate::commands::state::{edit_pin_state, list_state, read_history, remove_state};
use crate::exit::ExitCode;
use crate::ux::CommandError;
use pkg_core::{History, PinAction};
use pkg_pipeline::{CommitError, load_active_snapshot, load_retained_history};
use pkg_store::{LeaseError, StateLayout, StateLease};

/// Shipped command operations backed by one ownership-validated user state.
///
/// Read-only lifecycle commands are live. Mutating and authenticated-index
/// commands remain explicit closed refusals until their complete transaction
/// coordinators are connected; they never fall through to raw Nix access.
#[derive(Debug)]
pub struct LocalStateOperations {
    source: Result<StateLayout, CommandError>,
}

impl LocalStateOperations {
    /// Opens a state root beneath the caller's trusted home boundary.
    #[must_use]
    pub fn open(trusted_home: &Path, state_root: &Path, owner_uid: u32) -> Self {
        let source = StateLayout::open(trusted_home, state_root, owner_uid).map_err(|_| {
            CommandError::new(
                ExitCode::StateCorrupt,
                "the per-user package state is missing or unsafe",
                "run `pkg doctor` before managing packages",
            )
        });
        Self { source }
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
        let edit = remove_state(self.active()?.state().clone(), args)?;
        if policy.dry_run() {
            return Ok(edit.into_parts().1);
        }
        Err(mutation_unavailable())
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
        _policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        read_history(&self.history_view()?, args)
    }

    fn rollback(
        &mut self,
        _args: &RollbackArgs,
        _policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        Err(mutation_unavailable())
    }

    fn gc(
        &mut self,
        _args: &GcArgs,
        _policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        Err(mutation_unavailable())
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
    fn edit_pin(
        &self,
        args: &PackageArgs,
        policy: OperationPolicy,
        action: PinAction,
    ) -> Result<CommandResult, CommandError> {
        let edit = edit_pin_state(self.active()?.state().clone(), args, action)?;
        if policy.dry_run() {
            return Ok(edit.into_parts().1);
        }
        Err(mutation_unavailable())
    }
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
    use crate::cli::{Cli, Command};
    use crate::commands::execute::{CommandEngine, CommandRequest, CoreEngine};

    #[test]
    fn missing_or_uninitialized_state_fails_closed() {
        let home = TempDir::new().unwrap();
        fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let uid = fs::symlink_metadata(home.path()).unwrap().uid();
        let mut operations = LocalStateOperations::open(home.path(), &home.path().join("pkg"), uid);
        let cli = Cli::try_parse(["pkg", "list"]).unwrap();
        let Command::List(args) = cli.parsed_command() else {
            unreachable!()
        };
        assert_eq!(
            operations.list(args).unwrap_err().exit_code(),
            ExitCode::StateCorrupt
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
}
