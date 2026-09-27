//! Real child-process failures at the source-lock/evaluation boundary.

use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn source_process(directory: &Path, metadata_succeeds: bool, engine_available: bool) {
    let metadata = serde_json::json!({
        "locked": {
            "type": "github", "owner": "example", "repo": "tools",
            "rev": REVISION, "narHash": NAR_HASH,
        },
        "locks": {"version": 7, "root": "root", "nodes": {"root": {}}},
    });
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
         case \" $* \" in\n\
         *' flake metadata '*) printf '%s\\n' '{}'; exit {} ;;\n\
         *' derivation show '*) exit 1 ;;\n\
         *' store ping '*) exit {} ;;\n\
         *) exit 99 ;;\nesac\n",
        directory.join("calls").display(),
        metadata,
        u8::from(!metadata_succeeds),
        u8::from(!engine_available),
    );
    for name in ["nix", "nix-store"] {
        let path = directory.join(name);
        fs::write(&path, &script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn public_source_process_failure_keeps_lock_and_evaluation_categories() {
    for (metadata_succeeds, engine_available, expected) in [
        (false, true, ResolveErrorCode::SourceUnavailable),
        (true, true, ResolveErrorCode::EvaluationFailed),
        (false, false, ResolveErrorCode::EngineUnavailable),
        (true, false, ResolveErrorCode::EngineUnavailable),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let directory = fs::canonicalize(directory.path()).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(directory.join("tmp")).unwrap();
        fs::set_permissions(directory.join("tmp"), fs::Permissions::from_mode(0o700)).unwrap();
        source_process(&directory, metadata_succeeds, engine_available);
        let adapter = pkg_nix::RealNixAdapter::new(&directory.join("nix"), &directory).unwrap();
        let reference = pkg_core::PublicFlakeRef::new("github:example/tools#missing").unwrap();
        let error = resolve_flake(
            &selector(reference.as_str(), VersionPreference::Any),
            &reference,
            System::Aarch64Darwin,
            &adapter,
        )
        .unwrap_err();
        assert_eq!(
            error.code(),
            expected,
            "metadata={metadata_succeeds}, engine={engine_available}"
        );
        let calls = fs::read_to_string(directory.join("calls")).unwrap();
        assert!(calls.contains("flake metadata --json --no-update-lock-file"));
        assert_eq!(calls.contains("derivation show"), metadata_succeeds);
        assert!(calls.contains("--reference-lock-file") || !metadata_succeeds);
        assert!(calls.contains("store ping"));
        assert!(!calls.contains(" build "));
    }
}
