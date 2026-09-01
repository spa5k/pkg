//! End-to-end CLI tests against the fake Nix and broker harness.

use std::sync::Arc;

use pkg_cli::cli::{
    Cli, GcArgs, HistoryArgs, InfoArgs, InstallArgs, ListArgs, PackageArgs, RemoveArgs, RepairArgs,
    RollbackArgs, SearchArgs, UpdateArgs, UpgradeArgs,
};
use pkg_cli::commands::execute::{
    CommandEngine, CommandRequest, CommandResult, CoreEngine, CoreOperations, OperationPolicy,
};
use pkg_cli::exit::ExitCode;
use pkg_cli::ux::CommandError;
use pkg_nix::{
    AcceptedFormats, FormatVersion, GcReport, GcStatus, NixAdapter, NixVersion, VersionInfo,
};
use pkg_testkit::FakeNix;
use serde_json::{Map, json};

struct FakeOperations {
    nix: Arc<FakeNix>,
    calls: Vec<&'static str>,
}

impl FakeOperations {
    fn ok(&mut self, command: &'static str) -> Result<CommandResult, CommandError> {
        self.calls.push(command);
        CommandResult::new(
            format!("{command} completed"),
            Map::from_iter([("handled".into(), json!(command))]),
            vec![],
        )
        .map_err(|_| CommandError::new(ExitCode::Config, "invalid fake result", "report a bug"))
    }
}

impl CoreOperations for FakeOperations {
    fn search(&mut self, _: &SearchArgs) -> Result<CommandResult, CommandError> {
        self.ok("search")
    }
    fn info(&mut self, _: &InfoArgs) -> Result<CommandResult, CommandError> {
        self.ok("info")
    }
    fn install(
        &mut self,
        _: &InstallArgs,
        policy: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        assert!(policy.yes());
        self.nix.version().map_err(|_| {
            CommandError::new(
                ExitCode::EngineUnavailable,
                "engine unavailable",
                "run `pkg doctor`",
            )
        })?;
        self.ok("install")
    }
    fn remove(
        &mut self,
        _: &RemoveArgs,
        _: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.ok("remove")
    }
    fn list(&mut self, _: &ListArgs) -> Result<CommandResult, CommandError> {
        self.ok("list")
    }
    fn outdated(&mut self) -> Result<CommandResult, CommandError> {
        self.ok("outdated")
    }
    fn update(
        &mut self,
        _: &UpdateArgs,
        _: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.ok("update")
    }
    fn upgrade(
        &mut self,
        _: &UpgradeArgs,
        _: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.ok("upgrade")
    }
    fn pin(&mut self, _: &PackageArgs, _: OperationPolicy) -> Result<CommandResult, CommandError> {
        self.ok("pin")
    }
    fn unpin(
        &mut self,
        _: &PackageArgs,
        _: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.ok("unpin")
    }
    fn history(
        &mut self,
        _: &HistoryArgs,
        _: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.ok("history")
    }
    fn rollback(
        &mut self,
        _: &RollbackArgs,
        _: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.ok("rollback")
    }
    fn gc(&mut self, _: &GcArgs, policy: OperationPolicy) -> Result<CommandResult, CommandError> {
        assert!(policy.dry_run());
        let report = self.nix.gc().map_err(|_| {
            CommandError::new(
                ExitCode::EngineUnavailable,
                "collector unavailable",
                "run `pkg doctor`",
            )
        })?;
        self.calls.push("gc");
        CommandResult::new(
            "gc completed",
            Map::from_iter([("freedBytes".into(), json!(report.freed_bytes()))]),
            vec![],
        )
        .map_err(|_| CommandError::new(ExitCode::Config, "invalid fake result", "report a bug"))
    }
    fn repair(
        &mut self,
        _: &RepairArgs,
        _: OperationPolicy,
    ) -> Result<CommandResult, CommandError> {
        self.nix.version().map_err(|_| {
            CommandError::new(
                ExitCode::EngineUnavailable,
                "engine unavailable",
                "run `pkg doctor`",
            )
        })?;
        self.ok("repair")
    }
}

fn version() -> VersionInfo {
    VersionInfo::new(
        NixVersion::new("2.34.8").unwrap(),
        AcceptedFormats::new(FormatVersion::new(1).unwrap()),
    )
}

#[test]
fn every_command_routes_through_typed_fake_core_and_engine_calls_are_exact() {
    let nix = Arc::new(FakeNix::new());
    nix.expect_version(Ok(version()))
        .expect_gc(Ok(GcReport::new(GcStatus::Collected, vec![], 4096).unwrap()))
        .expect_version(Ok(version()));
    let operations = FakeOperations {
        nix: nix.clone(),
        calls: Vec::new(),
    };
    let mut engine = CoreEngine::new(operations);
    let commands = [
        vec!["pkg", "search", "ripgrep"],
        vec!["pkg", "info", "ripgrep"],
        vec!["pkg", "install", "ripgrep", "--yes"],
        vec!["pkg", "remove", "ripgrep"],
        vec!["pkg", "list"],
        vec!["pkg", "outdated"],
        vec!["pkg", "update"],
        vec!["pkg", "upgrade", "ripgrep"],
        vec!["pkg", "pin", "ripgrep"],
        vec!["pkg", "unpin", "ripgrep"],
        vec!["pkg", "history"],
        vec!["pkg", "rollback"],
        vec!["pkg", "gc", "--dry-run"],
        vec!["pkg", "repair"],
    ];
    for argv in commands {
        let cli = Cli::try_parse(argv).unwrap();
        let result = engine.execute(&CommandRequest::from_cli(&cli)).unwrap();
        let bytes = serde_json::to_vec(result.fields()).unwrap();
        assert!(!bytes.windows(11).any(|window| window == b"/nix/store/"));
    }
    let operations = engine.into_operations();
    assert_eq!(
        operations.calls,
        [
            "search", "info", "install", "remove", "list", "outdated", "update", "upgrade", "pin",
            "unpin", "history", "rollback", "gc", "repair"
        ]
    );
    nix.assert_exhausted().unwrap();
}
