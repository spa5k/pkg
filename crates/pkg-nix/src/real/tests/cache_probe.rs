//! Batched download planning must preserve independent, authenticated closures.

use super::*;

fn path(name: &str) -> Result<StorePath, Box<dyn std::error::Error>> {
    Ok(StorePath::new(&format!(
        "/nix/store/22222222222222222222222222222222-{name}"
    ))?)
}

fn entry(references: &[&StorePath], remote: bool) -> serde_json::Value {
    let mut value = serde_json::json!({
        "narHash": "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "narSize": 13,
        "references": references.iter().map(|path| {
            path.as_str().trim_start_matches("/nix/store/")
        }).collect::<Vec<_>>(),
        "signatures": [],
        "storeDir": "/nix/store",
        "ultimate": false,
        "version": 2,
    });
    if remote {
        value["compression"] = "xz".into();
        value["downloadHash"] = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into();
        value["downloadSize"] = 7.into();
        value["url"] = "nar/example.nar.xz".into();
        value["signatures"] = serde_json::json!([
            "cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="
        ]);
    }
    value
}

fn envelope(entries: &[(&StorePath, serde_json::Value)]) -> Vec<u8> {
    let info = entries
        .iter()
        .map(|(path, value)| {
            (
                path.as_str().trim_start_matches("/nix/store/").to_owned(),
                value.clone(),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::json!({"info": info, "storeDir": "/nix/store", "version": 2})
        .to_string()
        .into_bytes()
}

#[test]
fn download_probe_batches_mixed_roots_without_cross_contaminating_closures()
-> Result<(), Box<dyn std::error::Error>> {
    let first = path("first")?;
    let second = path("second")?;
    let local = path("local")?;
    let missing = path("missing")?;
    let shared = path("shared")?;
    let local_dep = path("local-dep")?;
    let missing_dep = path("missing-dep")?;
    let first_entry = entry(&[&first, &shared, &local_dep], true);
    let second_entry = entry(&[&shared, &missing_dep], true);
    let executor = Scripted::new(vec![
        success(Vec::new()),
        success(envelope(&[
            (&first, serde_json::Value::Null),
            (&local, entry(&[], false)),
            (&second, serde_json::Value::Null),
            (&missing, serde_json::Value::Null),
        ])),
        success(Vec::new()),
        success(format!("{}\n", missing.as_str())),
        success(envelope(&[
            (&first, first_entry),
            (&second, second_entry),
            (&shared, entry(&[], true)),
            (&local_dep, entry(&[], true)),
            (&missing_dep, serde_json::Value::Null),
        ])),
        success(envelope(&[
            (&shared, serde_json::Value::Null),
            (&local_dep, entry(&[], false)),
        ])),
        success(Vec::new()),
    ]);
    let calls = Arc::clone(&executor.calls);
    let programs = Arc::clone(&executor.programs);
    let adapter = RealNixAdapter::scripted(executor);
    let closures = adapter.inspect_download_closures(&[
        first.clone(),
        local.clone(),
        second.clone(),
        missing.clone(),
    ])?;
    assert_eq!(
        closures,
        vec![
            CacheDownloadClosure::new(
                first.clone(),
                vec![
                    CachePathObservation::hit(first.clone(), 7, 13),
                    CachePathObservation::hit(shared.clone(), 7, 13),
                    CachePathObservation::local(local_dep.clone(), 13),
                ]
            )?,
            CacheDownloadClosure::new(local.clone(), vec![CachePathObservation::local(local, 13)])?,
            CacheDownloadClosure::new(
                second.clone(),
                vec![
                    CachePathObservation::hit(second.clone(), 7, 13),
                    CachePathObservation::hit(shared.clone(), 7, 13),
                    CachePathObservation::miss(missing_dep),
                ]
            )?,
            CacheDownloadClosure::new(missing.clone(), vec![CachePathObservation::miss(missing)])?,
        ]
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 7);
    assert_eq!(
        programs.lock().map_err(|_| "poisoned program log")?[3],
        NixProgram::LegacyStore
    );
    for argument in ["--check-validity", "--print-invalid", "--store", CACHE_URL] {
        assert!(calls[3].contains(&OsString::from(argument)));
    }
    assert!(calls[4].contains(&OsString::from("--recursive")));
    assert!(calls[4].contains(&OsString::from(first.as_str())));
    assert!(calls[4].contains(&OsString::from(second.as_str())));
    let verified = calls[6]
        .iter()
        .filter_map(|arg| arg.to_str())
        .filter(|arg| arg.starts_with("/nix/store/"))
        .collect::<Vec<_>>();
    assert_eq!(
        verified,
        vec![first.as_str(), second.as_str(), shared.as_str()]
    );
    assert!(!verified.contains(&local_dep.as_str()));
    Ok(())
}

#[test]
fn download_probe_bounds_process_count_for_many_leaf_dependencies()
-> Result<(), Box<dyn std::error::Error>> {
    let roots = (0..65)
        .map(|index| path(&format!("crate-{index:02}")))
        .collect::<Result<Vec<_>, _>>()?;
    let mut outcomes = vec![success(Vec::new())];
    for chunk in roots.chunks(PATH_INFO_BATCH_SIZE) {
        outcomes.push(success(envelope(
            &chunk
                .iter()
                .map(|path| (path, serde_json::Value::Null))
                .collect::<Vec<_>>(),
        )));
    }
    outcomes.push(success(Vec::new()));
    for _ in roots.chunks(PATH_INFO_BATCH_SIZE) {
        outcomes.push(success(Vec::new()));
    }
    for chunk in roots.chunks(PATH_INFO_BATCH_SIZE) {
        outcomes.push(success(envelope(
            &chunk
                .iter()
                .map(|path| (path, entry(&[], true)))
                .collect::<Vec<_>>(),
        )));
        outcomes.push(success(Vec::new()));
    }
    let executor = Scripted::new(outcomes);
    let calls = Arc::clone(&executor.calls);
    let closures = RealNixAdapter::scripted(executor).inspect_download_closures(&roots)?;
    assert_eq!(closures.len(), 65);
    for (closure, root) in closures.iter().zip(&roots) {
        assert_eq!(
            closure.paths(),
            &[CachePathObservation::hit(root.clone(), 7, 13)]
        );
    }
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    // Four commands per group (local, validity, recursive, verify), plus two pings.
    assert_eq!(calls.len(), 14);
    for call in calls.iter() {
        assert!(
            call.iter()
                .filter_map(|arg| arg.to_str())
                .filter(|arg| arg.starts_with("/nix/store/"))
                .count()
                <= PATH_INFO_BATCH_SIZE
        );
    }
    Ok(())
}

#[test]
fn download_probe_rejects_failed_or_malformed_validity_responses()
-> Result<(), Box<dyn std::error::Error>> {
    let root = path("root")?;
    let unknown = path("unknown")?;
    for outcome in [
        failure(1),
        success(format!("{}\n", unknown.as_str())),
        success(format!("{0}\n{0}\n", root.as_str())),
        success(b"not-a-store-path\n".as_slice()),
        success(b"\n".as_slice()),
        success(vec![0xff]),
    ] {
        let executor = Scripted::new(vec![
            success(Vec::new()),
            success(envelope(&[(&root, serde_json::Value::Null)])),
            success(Vec::new()),
            outcome,
        ]);
        let calls = Arc::clone(&executor.calls);
        assert_eq!(
            RealNixAdapter::scripted(executor)
                .inspect_download_closures(std::slice::from_ref(&root))
                .unwrap_err()
                .code(),
            BuildCacheErrorCode::ProbeFailed
        );
        assert_eq!(calls.lock().map_err(|_| "poisoned call log")?.len(), 4);
    }
    Ok(())
}

#[test]
fn download_probe_falls_back_after_failed_local_batch() -> Result<(), Box<dyn std::error::Error>> {
    let local = path("local")?;
    let missing = path("missing")?;
    let remote = path("remote")?;
    let executor = Scripted::new(vec![
        success(Vec::new()),
        failure(1),
        success(format!("{}\n{}\n", missing.as_str(), remote.as_str())),
        success(envelope(&[(&local, entry(&[], false))])),
        success(Vec::new()),
        success(format!("{}\n", missing.as_str())),
        success(envelope(&[(&remote, entry(&[], true))])),
        success(Vec::new()),
    ]);
    let closures = RealNixAdapter::scripted(executor).inspect_download_closures(&[
        local.clone(),
        missing.clone(),
        remote.clone(),
    ])?;
    assert_eq!(
        closures,
        vec![
            CacheDownloadClosure::new(local.clone(), vec![CachePathObservation::local(local, 13)])?,
            CacheDownloadClosure::new(missing.clone(), vec![CachePathObservation::miss(missing)])?,
            CacheDownloadClosure::new(
                remote.clone(),
                vec![CachePathObservation::hit(remote, 7, 13)]
            )?,
        ]
    );
    Ok(())
}

#[test]
fn entirely_missing_local_batches_never_fall_back_to_per_path_queries()
-> Result<(), Box<dyn std::error::Error>> {
    let paths = (0..65)
        .map(|index| path(&format!("missing-{index}")))
        .collect::<Result<Vec<_>, _>>()?;
    let invalid = |chunk: &[StorePath]| {
        chunk
            .iter()
            .map(|path| format!("{}\n", path.as_str()))
            .collect::<String>()
    };
    let mut outcomes = vec![success(Vec::new())];
    for chunk in paths.chunks(PATH_INFO_BATCH_SIZE) {
        outcomes.push(failure(1));
        outcomes.push(success(invalid(chunk)));
    }
    outcomes.push(success(Vec::new()));
    for chunk in paths.chunks(PATH_INFO_BATCH_SIZE) {
        outcomes.push(success(invalid(chunk)));
    }
    let executor = Scripted::new(outcomes);
    let calls = Arc::clone(&executor.calls);
    let programs = Arc::clone(&executor.programs);
    let closures = RealNixAdapter::scripted(executor).inspect_download_closures(&paths)?;
    assert_eq!(closures.len(), 65);
    assert!(
        closures
            .iter()
            .all(|closure| closure.paths()[0].download_bytes().is_none())
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 11);
    let programs = programs.lock().map_err(|_| "poisoned program log")?;
    for index in [2, 4, 6] {
        assert_eq!(programs[index], NixProgram::LegacyStore);
        assert!(!calls[index].iter().any(|arg| arg == "--store"));
    }
    Ok(())
}

#[test]
fn download_probe_rejects_incomplete_recursive_graphs_and_untrusted_hits()
-> Result<(), Box<dyn std::error::Error>> {
    let root = path("root")?;
    let dep = path("dep")?;
    let mut unsigned = entry(&[], true);
    unsigned["signatures"] = serde_json::json!([]);
    let cases = [
        (envelope(&[(&root, entry(&[&dep], true))]), false),
        (envelope(&[(&root, serde_json::Value::Null)]), false),
        (envelope(&[(&root, unsigned)]), false),
        (envelope(&[(&root, entry(&[], true))]), true),
    ];
    for (recursive, fail_signature_check) in cases {
        let mut outcomes = vec![
            success(Vec::new()),
            success(envelope(&[(&root, serde_json::Value::Null)])),
            success(Vec::new()),
            success(Vec::new()),
            success(recursive),
        ];
        if fail_signature_check {
            outcomes.push(failure(1));
        }
        let executor = Scripted::new(outcomes);
        let calls = Arc::clone(&executor.calls);
        assert_eq!(
            RealNixAdapter::scripted(executor)
                .inspect_download_closures(std::slice::from_ref(&root))
                .unwrap_err()
                .code(),
            BuildCacheErrorCode::ProbeFailed
        );
        let calls = calls.lock().map_err(|_| "poisoned call log")?;
        assert_eq!(calls.len(), if fail_signature_check { 6 } else { 5 });
    }
    Ok(())
}
