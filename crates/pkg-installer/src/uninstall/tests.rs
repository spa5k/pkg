//! Tests for the `uninstall` module.

use super::*;

#[derive(Default)]
struct FakeBackend {
    calls: Vec<String>,
    fail: Option<UninstallAction>,
    fail_preflight: Option<&'static str>,
}

impl UninstallBackend for FakeBackend {
    fn preflight_privilege(&mut self) -> Result<(), UninstallError> {
        self.calls.push("privilege".into());
        if self.fail_preflight == Some("privilege") {
            Err(UninstallError::backend_failure())
        } else {
            Ok(())
        }
    }

    fn verify_ownership(&mut self, _manifest: &UninstallManifest) -> Result<(), UninstallError> {
        self.calls.push("ownership".into());
        if self.fail_preflight == Some("ownership") {
            Err(UninstallError::backend_failure())
        } else {
            Ok(())
        }
    }

    fn preflight_unmanaged_nix(&mut self) -> Result<(), UninstallError> {
        self.calls.push("foreign-scan".into());
        if self.fail_preflight == Some("foreign") {
            Err(UninstallError::backend_failure())
        } else {
            Ok(())
        }
    }

    fn execute(&mut self, action: UninstallAction) -> Result<(), UninstallError> {
        self.calls.push(format!("{action:?}"));
        if self.fail == Some(action) {
            Err(UninstallError::backend_failure())
        } else {
            Ok(())
        }
    }
}

fn manifest(
    system: System,
    state: RecordedAssetState,
) -> Result<UninstallManifest, UninstallError> {
    let assets = platform_assets(system)
        .into_iter()
        .map(|asset| {
            let record = RecordedAsset::new(
                asset.id,
                if system == System::Aarch64Darwin && asset.id == "nix-root" {
                    RecordedAssetState::PreExisting
                } else {
                    state
                },
            )?;
            Ok(
                if matches!(
                    system,
                    System::X8664Linux | System::Aarch64Linux | System::Aarch64Darwin
                ) && asset.kind == UninstallAssetKind::File
                    && asset.id != "uninstall-manifest"
                {
                    record.with_content_digest(Digest::from_bytes([9; 32]))
                } else {
                    record
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    UninstallManifest::new(system, Digest::from_bytes([7; 32]), assets)
}

fn linux_determinate_manifest() -> Result<UninstallManifest, UninstallError> {
    let assets = platform_assets(System::Aarch64Linux)
        .into_iter()
        .map(|asset| {
            let record = RecordedAsset::new(
                asset.id,
                if asset.id == "nix-root" {
                    RecordedAssetState::PreExisting
                } else {
                    RecordedAssetState::Created
                },
            )?;
            Ok(
                if asset.kind == UninstallAssetKind::File && asset.id != "uninstall-manifest" {
                    record.with_content_digest(Digest::from_bytes([9; 32]))
                } else {
                    record
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    UninstallManifest::new(System::Aarch64Linux, Digest::from_bytes([7; 32]), assets)
}

fn error_code<T>(result: Result<T, UninstallError>) -> Option<UninstallErrorCode> {
    result.err().map(UninstallError::code)
}

#[test]
fn manifest_requires_exact_complete_compiled_ids() -> Result<(), UninstallError> {
    let valid = manifest(System::Aarch64Linux, RecordedAssetState::Created)?;
    assert_eq!(
        valid.assets().len(),
        crate::assets::linux_product_install_assets().count()
    );

    let mut missing = valid.assets().to_vec();
    missing.pop();
    assert_eq!(
        error_code(UninstallManifest::new(
            System::Aarch64Linux,
            Digest::from_bytes([7; 32]),
            missing
        )),
        Some(UninstallErrorCode::InvalidManifest)
    );

    let mut duplicate = valid.assets().to_vec();
    duplicate[0] = duplicate[1].clone();
    assert_eq!(
        error_code(UninstallManifest::new(
            System::Aarch64Linux,
            Digest::from_bytes([7; 32]),
            duplicate
        )),
        Some(UninstallErrorCode::InvalidManifest)
    );
    assert!(RecordedAsset::new("../../etc/passwd", RecordedAssetState::Created).is_err());
    Ok(())
}

#[test]
fn uninstall_manifest_disk_form_is_strict_canonical_and_complete() -> Result<(), UninstallError> {
    let manifest = manifest(System::Aarch64Linux, RecordedAssetState::Created)?;
    let encoded = encode_uninstall_manifest(&manifest)?;
    assert_eq!(decode_uninstall_manifest(&encoded)?, manifest);
    assert!(encoded.ends_with(b"\n"));
    assert!(encoded.starts_with(b"{\"schemaVersion\":2,\"product\":\"pkg\","));

    let mut extended: serde_json::Value = serde_json::from_slice(&encoded)
        .map_err(|_| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    extended["extension"] = serde_json::json!(true);
    let mut extended = serde_json::to_vec(&extended)
        .map_err(|_| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    extended.push(b'\n');
    assert_eq!(
        error_code(decode_uninstall_manifest(&extended)),
        Some(UninstallErrorCode::InvalidManifest)
    );
    assert_eq!(
        error_code(decode_uninstall_manifest(encoded.trim_ascii_end())),
        Some(UninstallErrorCode::InvalidManifest)
    );

    let encoded = encode_uninstall_manifest(&manifest)?;
    let mut wire: serde_json::Value = serde_json::from_slice(&encoded)
        .map_err(|_| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    let records = wire["assets"]
        .as_array_mut()
        .ok_or_else(|| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    let file = records
        .iter_mut()
        .find(|record| record.get("contentDigest").is_some())
        .ok_or_else(|| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    file.as_object_mut()
        .ok_or_else(|| UninstallError::new(UninstallErrorCode::InvalidManifest))?
        .remove("contentDigest");
    let mut malformed = serde_json::to_vec(&wire)
        .map_err(|_| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    malformed.push(b'\n');
    assert_eq!(
        error_code(decode_uninstall_manifest(&malformed)),
        Some(UninstallErrorCode::InvalidManifest)
    );
    Ok(())
}

#[test]
fn manifest_round_trip_preserves_exact_file_content_identity() -> Result<(), UninstallError> {
    let mut assets = platform_assets(System::Aarch64Linux)
        .into_iter()
        .map(|asset| {
            let record = RecordedAsset::new(asset.id, RecordedAssetState::Created)?;
            Ok(
                if asset.kind == UninstallAssetKind::File && asset.id != "uninstall-manifest" {
                    record.with_content_digest(Digest::from_bytes([9; 32]))
                } else {
                    record
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let expected = Digest::from_bytes([11; 32]);
    assets[0] = assets[0].clone().with_content_digest(expected);
    let manifest =
        UninstallManifest::new(System::Aarch64Linux, Digest::from_bytes([7; 32]), assets)?;

    let decoded = decode_uninstall_manifest(&encode_uninstall_manifest(&manifest)?)?;

    assert_eq!(decoded.assets()[0].content_digest(), Some(expected));
    assert_eq!(decoded, manifest);
    Ok(())
}

#[test]
fn v2_linux_receipt_rejects_missing_and_non_file_digests() -> Result<(), UninstallError> {
    let valid = manifest(System::Aarch64Linux, RecordedAssetState::Created)?;
    let file = valid
        .assets()
        .iter()
        .position(|record| record.id() != "uninstall-manifest" && record.content_digest().is_some())
        .ok_or_else(|| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    let mut missing = valid.assets().to_vec();
    missing[file].content_digest = None;
    assert_eq!(
        error_code(UninstallManifest::new(
            System::Aarch64Linux,
            Digest::from_bytes([7; 32]),
            missing,
        )),
        Some(UninstallErrorCode::InvalidManifest)
    );

    let non_file = valid
        .assets()
        .iter()
        .position(|record| record.content_digest().is_none())
        .ok_or_else(|| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    let mut extra = valid.assets().to_vec();
    extra[non_file].content_digest = Some(Digest::from_bytes([12; 32]));
    assert_eq!(
        error_code(UninstallManifest::new(
            System::Aarch64Linux,
            Digest::from_bytes([7; 32]),
            extra,
        )),
        Some(UninstallErrorCode::InvalidManifest)
    );

    let receipt = valid
        .assets()
        .iter()
        .position(|record| record.id() == "uninstall-manifest")
        .ok_or_else(|| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
    let mut preexisting_receipt = valid.assets().to_vec();
    preexisting_receipt[receipt].state = RecordedAssetState::PreExisting;
    assert_eq!(
        error_code(UninstallManifest::new(
            System::Aarch64Linux,
            Digest::from_bytes([7; 32]),
            preexisting_receipt,
        )),
        Some(UninstallErrorCode::InvalidManifest)
    );
    Ok(())
}

#[test]
fn dry_run_is_deterministic_closed_and_non_mutating() -> Result<(), UninstallError> {
    for system in [
        System::X8664Linux,
        System::Aarch64Linux,
        System::Aarch64Darwin,
    ] {
        let manifest = manifest(system, RecordedAssetState::Created)?;
        let first = plan_uninstall(&manifest)?;
        let second = plan_uninstall(&manifest)?;
        assert_eq!(first, second);
        assert_eq!(
            first.actions().first(),
            Some(&UninstallAction::StopServices)
        );
        assert_eq!(
            first.actions().last(),
            Some(if system == System::Aarch64Darwin {
                &UninstallAction::ExecDeterminateUninstall
            } else {
                &UninstallAction::VerifyNoPrivilegedResidue
            })
        );
        if first.actions().iter().any(|action| {
            matches!(
                action,
                UninstallAction::RemoveAsset {
                    id: "uninstall-manifest",
                    ..
                }
            )
        }) {
            let receipt = first
                .actions()
                .iter()
                .position(|action| {
                    matches!(
                        action,
                        UninstallAction::RemoveAsset {
                            id: "uninstall-manifest",
                            ..
                        }
                    )
                })
                .ok_or_else(|| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
            for id in ["uninstall-root", "product-root"] {
                let parent = first
                        .actions()
                        .iter()
                        .position(|action| matches!(action, UninstallAction::RemoveAsset { id: actual, .. } if *actual == id))
                        .ok_or_else(|| UninstallError::new(UninstallErrorCode::InvalidManifest))?;
                assert!(receipt < parent);
            }
        }
        if system == System::Aarch64Darwin {
            assert!(first.actions().contains(&UninstallAction::RemoveUserRoots));
            assert!(!first.actions().iter().any(|action| matches!(
                action,
                UninstallAction::RemoveAsset { target: "/nix", .. }
            )));
        } else {
            assert!(
                first
                    .actions()
                    .contains(&UninstallAction::RemoveManagedStoreIfExclusive)
            );
            assert!(first.actions().iter().any(|action| matches!(
                action,
                UninstallAction::RemoveAsset { target: "/nix", .. }
            )));
        }
    }
    Ok(())
}

#[test]
fn preexisting_base_nix_is_never_a_product_removal_target() -> Result<(), UninstallError> {
    let manifest = manifest(System::Aarch64Darwin, RecordedAssetState::Created)?;
    let plan = plan_uninstall(&manifest)?;
    assert!(
        !plan
            .actions()
            .iter()
            .any(|action| matches!(action, UninstallAction::RemoveAsset { target: "/nix", .. }))
    );
    assert!(!plan.actions().contains(&UninstallAction::CollectGarbage));
    assert!(
        !plan
            .actions()
            .contains(&UninstallAction::RemoveManagedStoreIfExclusive)
    );
    assert_eq!(
        plan.actions().last(),
        Some(&UninstallAction::ExecDeterminateUninstall)
    );
    Ok(())
}

#[test]
fn linux_vendor_uninstall_is_the_terminal_action() -> Result<(), UninstallError> {
    let manifest = linux_determinate_manifest()?;
    let plan = plan_uninstall(&manifest)?;
    let roots = plan
        .actions()
        .iter()
        .position(|action| *action == UninstallAction::RemoveUserRoots)
        .ok_or_else(UninstallError::backend_failure)?;
    let verification = plan
        .actions()
        .iter()
        .position(|action| *action == UninstallAction::VerifyNoPrivilegedResidue)
        .ok_or_else(UninstallError::backend_failure)?;
    let asset = |id| {
        plan.actions()
                .iter()
                .position(|action| matches!(action, UninstallAction::RemoveAsset { id: actual, .. } if *actual == id))
                .ok_or_else(UninstallError::backend_failure)
    };
    let users = asset("nix-gcroots-users")?;
    let product = asset("nix-gcroots")?;
    assert!(roots < users && users < product && product < verification);
    assert_eq!(
        plan.actions().last(),
        Some(&UninstallAction::ExecDeterminateUninstall)
    );
    Ok(())
}

#[test]
fn macos_removes_receipt_and_directories_before_broker_account() -> Result<(), UninstallError> {
    let plan = plan_uninstall(&manifest(
        System::Aarch64Darwin,
        RecordedAssetState::Created,
    )?)?;
    let position = |id| {
        plan.actions()
                .iter()
                .position(|action| matches!(action, UninstallAction::RemoveAsset { id: actual, .. } if *actual == id))
                .ok_or_else(UninstallError::backend_failure)
    };
    let broker = position("broker-user")?;
    assert!(broker < position("uninstall-manifest")?);
    assert!(position("uninstall-manifest")? < position("uninstall-root")?);
    assert!(position("uninstall-root")? < position("product-root")?);
    assert!(broker < position("broker-group")?);
    assert!(!plan.actions().iter().any(|action| matches!(
        action,
        UninstallAction::RemoveAsset { id, .. } if id.starts_with("build-")
    )));
    assert_eq!(
        plan.actions().last(),
        Some(&UninstallAction::ExecDeterminateUninstall)
    );
    Ok(())
}

#[test]
fn every_preflight_refusal_happens_before_mutation() -> Result<(), UninstallError> {
    let manifest = manifest(System::X8664Linux, RecordedAssetState::Created)?;
    let plan = plan_uninstall(&manifest)?;
    for (stage, code) in [
        ("privilege", UninstallErrorCode::PrivilegeRequired),
        ("ownership", UninstallErrorCode::OwnershipRefused),
        ("foreign", UninstallErrorCode::UnmanagedNix),
    ] {
        let mut backend = FakeBackend {
            fail_preflight: Some(stage),
            ..FakeBackend::default()
        };
        assert_eq!(
            error_code(execute_uninstall(&manifest, &plan, &mut backend)),
            Some(code)
        );
        assert!(
            backend.calls.iter().all(|call| {
                matches!(call.as_str(), "privilege" | "ownership" | "foreign-scan")
            })
        );
    }
    Ok(())
}

#[test]
fn service_stop_is_a_cleanup_barrier() -> Result<(), UninstallError> {
    let manifest = manifest(System::Aarch64Darwin, RecordedAssetState::Created)?;
    let plan = plan_uninstall(&manifest)?;
    let mut backend = FakeBackend {
        fail: Some(UninstallAction::StopServices),
        ..FakeBackend::default()
    };
    assert_eq!(
        error_code(execute_uninstall(&manifest, &plan, &mut backend)),
        Some(UninstallErrorCode::ServiceStopFailed)
    );
    assert_eq!(backend.calls.len(), 4);
    Ok(())
}

#[test]
fn cleanup_failures_do_not_skip_residue_verification() -> Result<(), UninstallError> {
    let manifest = manifest(System::Aarch64Linux, RecordedAssetState::Created)?;
    let plan = plan_uninstall(&manifest)?;
    let failed = plan
        .actions()
        .iter()
        .copied()
        .find(|action| matches!(action, UninstallAction::RemoveAsset { .. }))
        .ok_or_else(UninstallError::backend_failure)?;
    let mut backend = FakeBackend {
        fail: Some(failed),
        ..FakeBackend::default()
    };
    assert_eq!(
        error_code(execute_uninstall(&manifest, &plan, &mut backend)),
        Some(UninstallErrorCode::CleanupIncomplete)
    );
    assert_eq!(
        backend.calls.last().map(String::as_str),
        Some("VerifyNoPrivilegedResidue")
    );
    assert!(
        !backend
            .calls
            .iter()
            .any(|call| { call.starts_with("RemoveAsset { id: \"uninstall-manifest\"") })
    );
    Ok(())
}

#[test]
fn product_cleanup_failure_never_dispatches_terminal_vendor() -> Result<(), UninstallError> {
    for manifest in [
        linux_determinate_manifest()?,
        manifest(System::Aarch64Darwin, RecordedAssetState::Created)?,
    ] {
        let plan = plan_uninstall(&manifest)?;
        let failed = plan
            .actions()
            .iter()
            .copied()
            .find(|action| matches!(action, UninstallAction::RemoveAsset { .. }))
            .ok_or_else(UninstallError::backend_failure)?;
        let mut backend = FakeBackend {
            fail: Some(failed),
            ..FakeBackend::default()
        };

        assert_eq!(
            error_code(execute_uninstall(&manifest, &plan, &mut backend)),
            Some(UninstallErrorCode::CleanupIncomplete)
        );
        assert!(backend.calls.contains(&"VerifyNoPrivilegedResidue".into()));
        assert!(
            !backend
                .calls
                .contains(&format!("{:?}", UninstallAction::ExecDeterminateUninstall))
        );
    }
    Ok(())
}

#[test]
fn residue_failure_never_dispatches_terminal_vendor() -> Result<(), UninstallError> {
    for manifest in [
        linux_determinate_manifest()?,
        manifest(System::Aarch64Darwin, RecordedAssetState::Created)?,
    ] {
        let plan = plan_uninstall(&manifest)?;
        let mut backend = FakeBackend {
            fail: Some(UninstallAction::VerifyNoPrivilegedResidue),
            ..FakeBackend::default()
        };

        assert_eq!(
            error_code(execute_uninstall(&manifest, &plan, &mut backend)),
            Some(UninstallErrorCode::ResidueRemaining)
        );
        assert!(backend.calls.contains(&"VerifyNoPrivilegedResidue".into()));
        assert!(
            !backend
                .calls
                .contains(&format!("{:?}", UninstallAction::ExecDeterminateUninstall))
        );
    }
    Ok(())
}

#[test]
fn residue_failure_has_priority_and_success_is_total() -> Result<(), UninstallError> {
    let manifest = manifest(System::Aarch64Darwin, RecordedAssetState::Created)?;
    let plan = plan_uninstall(&manifest)?;
    let mut residue = FakeBackend {
        fail: Some(UninstallAction::VerifyNoPrivilegedResidue),
        ..FakeBackend::default()
    };
    assert_eq!(
        error_code(execute_uninstall(&manifest, &plan, &mut residue)),
        Some(UninstallErrorCode::ResidueRemaining)
    );

    let mut success = FakeBackend::default();
    let report = execute_uninstall(&manifest, &plan, &mut success)?;
    assert_eq!(report.completed_actions(), plan.actions().len());
    Ok(())
}
