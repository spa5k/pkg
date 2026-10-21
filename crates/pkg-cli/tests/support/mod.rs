pub fn formula_index_fixture(bin: &std::path::Path) -> std::path::PathBuf {
    let payload = bin.join("formula-index.json");
    let names: Vec<String> = std::iter::once(String::from("awscli"))
        .chain((0..25).map(|i| format!("aws-helper-{i}")))
        .chain([
            String::from("aws-disabled"),
            String::from("aws-deprecated"),
            String::from("aws-framework"),
        ])
        .collect();
    let entries: Vec<_> = names
        .iter()
        .map(|name| (name.as_str(), name.as_str(), "AWS command line tool"))
        .collect();
    let mut index = search_fixture_index("homebrew/core", &entries);
    index["schema"] = serde_json::json!("pkg-formula-catalog/1");
    index["aliases"] = serde_json::json!({"awscli@2": "awscli"});
    index["executables"] = serde_json::json!({"aws-framework": []});
    index["deprecated"] = serde_json::json!(["aws-deprecated"]);
    for name in &names {
        index["systems"]["x86_64-linux"]["entries"][format!("homebrew/core/{name}")]["kind"] =
            serde_json::json!("nixpkgs-equivalent");
    }
    for (name, reason) in [
        ("aws-disabled", "disabled"),
        ("aws-deprecated", "deprecated"),
    ] {
        let entry =
            &mut index["systems"]["x86_64-linux"]["entries"][format!("homebrew/core/{name}")];
        entry["status"] = serde_json::json!("excluded");
        entry["kind"] = serde_json::Value::Null;
        entry["reason"] = serde_json::json!(reason);
    }
    index["systems"]["x86_64-linux"]["entries"]["homebrew/core/aws-framework"]["description"] =
        serde_json::json!("AWS SDK framework");
    std::fs::write(&payload, serde_json::to_string(&index).unwrap()).unwrap();
    payload
}

pub fn search_fixture_index(source: &str, entries: &[(&str, &str, &str)]) -> serde_json::Value {
    let origin = format!(
        "https://github.com/{}/homebrew-{}",
        source.split_once('/').unwrap().0,
        source.split_once('/').unwrap().1
    );
    let entries = entries.iter().map(|(token, name, description)| (format!("{source}/{token}"), serde_json::json!({
        "source": source, "token": token, "name": name, "description": description,
        "version": "1.0", "homepage": null, "status": "eligible", "kind": "binary", "reason": null, "detail": null,
    }))).collect::<serde_json::Map<_, _>>();
    serde_json::json!({
        "schema": "pkg-cask-catalog/4", "generator": {"name": "cask-catalog", "version": "0.1.0"},
        "inputs": {(source): {"url": origin, "revision": "a".repeat(40), "sha256": "c".repeat(64), "license": "Homebrew data license",
            "raw": {"archiveUrl": format!("{origin}/archive/{}.tar.gz", "a".repeat(40)), "archiveSha256": "b".repeat(64), "captureSha256": "c".repeat(64), "envelopeSchema": "pkg-tap-raw-export/1"}}},
        "targets": ["x86_64-linux"], "macosBaseline": "15.7.7", "systems": {"x86_64-linux": {"entries": entries}},
    })
}

/// Write an offline `curl` into `bin`. Without `list`, every answer is
/// HTTP 404: the NixOS channels publish no list, and pkg evaluates the
/// source. With `list`, the channel names revision `c…c` and serves that
/// compressed package list. Each requested URL goes to `curl.log`.
pub fn fake_curl(bin: &std::path::Path, list: Option<&std::path::Path>) {
    let curl = bin.join("curl");
    let log = bin.join("curl.log");
    let answers = match list {
        Some(list) => format!(
            "  */git-revision) printf '%s\\n' {revision} > \"$out\"; printf '200 https://releases.test/nixpkgs/r1/git-revision';;\n  https://releases.test/nixpkgs/r1/packages.json.br) cp '{}' \"$out\"; printf '200 %s' \"$url\";;\n",
            list.display(),
            revision = "c".repeat(40)
        ),
        None => String::new(),
    };
    std::fs::write(
        &curl,
        format!(
            "#!/bin/sh\nout=; url=\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    -o) out=$2; shift;;\n    -w|--max-time|--proto|--proto-redir) shift;;\n    -*) ;;\n    *) url=$1;;\n  esac\n  shift\ndone\necho \"$url\" >> '{}'\ncase \"$url\" in\n{answers}  *) : > \"$out\"; printf '404 %s' \"$url\";;\nesac\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&curl, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}
