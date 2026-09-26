//! Public, redacted view of the authenticated uninstall plan.

use std::io::{self, Write};

use pkg_installer::{UninstallAction, UninstallAssetKind, UninstallPlan};
use serde_json::{Map, Value, json};

use super::execute::CommandResult;
use crate::{exit::ExitCode, presentation::Style, ux::CommandError};

const SHELL_GUIDANCE: &str = "Remove old unguarded pkg shellenv lines from your Bash or zsh startup file. Guarded setup lines can stay. Open a new shell after uninstall.";

/// Builds a bounded public result without account-home or store-object paths.
///
/// # Errors
/// Returns a command error if a plan cannot satisfy the public output contract.
pub fn result(plan: Option<&UninstallPlan>, dry_run: bool) -> Result<CommandResult, CommandError> {
    let actions = plan.map_or(&[][..], UninstallPlan::actions);
    let (summary, status) = if actions.is_empty() {
        ("pkg is not installed.", "absent")
    } else if dry_run {
        (
            "Uninstall preview is ready. No changes were made.",
            "planned",
        )
    } else {
        ("pkg is uninstalled.", "removed")
    };
    let details = actions.iter().map(action_detail).collect::<Vec<_>>();
    CommandResult::new(
        summary,
        Map::from_iter([
            ("actions".into(), json!(actions.len())),
            ("status".into(), json!(status)),
            ("plannedActions".into(), json!(details)),
            (
                "retainedItems".into(),
                json!([
                    "Personal shell startup files and unrelated user files",
                    "The root-owned install coordination lock (contains no lifecycle state)"
                ]),
            ),
            ("shellGuidance".into(), json!(SHELL_GUIDANCE)),
        ]),
        Vec::new(),
    )
    .map_err(|_| {
        CommandError::new(
            ExitCode::EngineUnavailable,
            "pkg could not report the uninstall plan",
            "run `pkg doctor`",
        )
    })
}

fn action_detail(action: &UninstallAction) -> Value {
    let (operation, target) = match action {
        UninstallAction::StopServices => ("Stop services", "pkg services"),
        UninstallAction::RemoveUserRoots => ("Remove package roots", "Registered pkg users"),
        UninstallAction::CollectGarbage => ("Collect unused packages", "Managed package store"),
        UninstallAction::RemoveAsset { kind, target, .. } => {
            let operation = match kind {
                UninstallAssetKind::File => "Remove file",
                UninstallAssetKind::Directory => "Remove directory",
                UninstallAssetKind::User => "Remove account",
                UninstallAssetKind::Group => "Remove group",
            };
            // Runtime-private paths remain behind the public CLI boundary.
            let target = if target.contains("/nix/") {
                "Managed Nix files"
            } else {
                target
            };
            (operation, target)
        }
        UninstallAction::RemoveRegisteredUserState => (
            "Remove user state",
            "Package selections, generations, logs, and caches for registered pkg users",
        ),
        UninstallAction::RemoveManagedStoreIfExclusive => (
            "Remove owned store",
            "Managed Nix store or volume, after ownership checks",
        ),
        UninstallAction::RemoveManagedRuntimePreservingStore => {
            ("Remove runtime", "Preserve the pre-existing store")
        }
        UninstallAction::ExecDeterminateUninstall => (
            "Run vendor uninstall",
            "Determinate removes Base Nix and its managed store; its result is final",
        ),
        UninstallAction::VerifyNoPrivilegedResidue => (
            "Verify cleanup",
            "Product services, accounts, and privileged files",
        ),
    };
    json!({"action": operation, "target": target})
}

/// Displays the same redacted plan before approval or the vendor process handoff.
///
/// # Errors
/// Returns an I/O error if the plan cannot be written.
pub fn write_confirmation(result: &CommandResult, writer: impl Write) -> io::Result<()> {
    super::human::write_confirmation(writer, "system uninstall", result)
}

pub(super) fn write_details(
    mut writer: impl Write,
    fields: &Map<String, Value>,
    style: Style,
) -> io::Result<()> {
    if let Some(actions) = fields.get("plannedActions").and_then(Value::as_array) {
        for action in actions {
            if let (Some(label), Some(target)) =
                (action["action"].as_str(), action["target"].as_str())
            {
                style.text(&mut writer, &format!("{label}: "), target)?;
            }
        }
    }
    if let Some(items) = fields.get("retainedItems").and_then(Value::as_array) {
        for item in items.iter().filter_map(Value::as_str) {
            style.text(&mut writer, "Retained: ", item)?;
        }
    }
    if let Some(guidance) = fields.get("shellGuidance").and_then(Value::as_str) {
        style.text(&mut writer, "Shell setup: ", guidance)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_redacted_but_product_paths_remain_reviewable() {
        for (target, expected) in [
            ("/nix/store/private", "Managed Nix files"),
            ("/usr/local/bin/pkg", "/usr/local/bin/pkg"),
        ] {
            let detail = action_detail(&UninstallAction::RemoveAsset {
                id: "fixture",
                kind: UninstallAssetKind::File,
                target,
            });
            assert_eq!(detail["target"], expected);
            CommandResult::new(
                "Preview",
                Map::from_iter([("plannedActions".into(), json!([detail]))]),
                vec![],
            )
            .unwrap();
        }
    }

    #[test]
    fn absent_install_keeps_shell_cleanup_guidance() {
        let result = result(None, true).unwrap();
        assert_eq!(result.fields()["actions"], 0);
        assert_eq!(result.fields()["status"], "absent");
        let mut output = Vec::new();
        write_confirmation(&result, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Personal shell startup files"));
        assert!(text.contains("Remove old unguarded pkg shellenv lines"));
    }
}
