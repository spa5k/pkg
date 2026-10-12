#![expect(missing_docs, reason = "integration test crates have no public API")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests may use unwrap, expect, and panic to fail on broken fixtures"
)]

use std::process::Command;

mod support;
use support::{formula_index_fixture, search_fixture_index};

fn publish_search_tap(state: &std::path::Path) {
    use pkg_cli::tap::{Provenance, Registry, SourceStore};
    let _lock = pkg_cli::tap::GlobalLock::acquire(state).unwrap();
    let mut registry = Registry::empty();
    registry.approve("acme/tools", "https://github.com/acme/homebrew-tools", 1);
    registry.save(&pkg_cli::tap::registry_path(state)).unwrap();
    let store = SourceStore::lock(state, "acme/tools").unwrap();
    let staged = store.new_staging().unwrap();
    std::fs::create_dir(staged.path().join("flake")).unwrap();
    let index = pkg_cli::nix::decode_catalog_index(&search_fixture_index(
        "acme/tools",
        &[
            ("aws-console", "AWS Console", "Browse cloud resources"),
            ("ripgrpe", "Ripgrpe", "Example application"),
        ],
    ))
    .unwrap();
    let provenance = Provenance {
        source: String::from("acme/tools"),
        origin: String::from("https://github.com/acme/homebrew-tools"),
        revision: "a".repeat(40),
        metadata_sha256: "c".repeat(64),
        eligible: 2,
        excluded: 0,
        published_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };
    assert!(
        store
            .publish(staged, &provenance, &index)
            .unwrap()
            .finish()
            .is_empty()
    );
}

struct SearchContext {
    bin: tempfile::TempDir,
    home: tempfile::TempDir,
    nix: std::path::PathBuf,
    log: std::path::PathBuf,
    cache: std::path::PathBuf,
    formulae: std::path::PathBuf,
    original_runtime: String,
}

impl SearchContext {
    fn ready() -> Self {
        let bin = tempfile::tempdir().expect("tempdir");
        let home = tempfile::tempdir().expect("tempdir");
        let nix = bin.path().join("nix");
        let formulae = formula_index_fixture(bin.path());
        let log = bin.path().join("calls.log");
        let index_file = bin.path().join("index.json");
        let index = search_fixture_index(
            "homebrew/cask",
            &[
                ("aws-vault-binary", "AWS Vault", "Stores AWS credentials"),
                ("corretto@11", "AWS Corretto JDK", "Java runtime"),
            ],
        );
        std::fs::write(&index_file, serde_json::to_string(&index).unwrap()).unwrap();
        std::fs::write(&nix, format!(r#"#!/usr/bin/python3
import json,re,sys
args=sys.argv[1:];text=" ".join(args)
if "--version" in args: print("nix (Nix) 2.35.2")
elif "config show system" in text: print("x86_64-linux")
elif "flake metadata" in text:
 owner,repo,rev=("NixOS","nixpkgs","a"*40) if "nixpkgs" in text else ("spa5k","pkg","b"*40)
 print(json.dumps({{"url":"github:"+owner+"/"+repo,"locked":{{"type":"github","owner":owner,"repo":repo,"rev":rev}}}}))
elif "eval" in args and '#formulaIndex' in text: print(open({formulae_literal}).read())
elif "eval" in args and ("--apply" in args or "--expr" in args):
 with open({log},"a") as log: log.write("batch\n")
 expression=args[args.index("--apply")+1] if "--apply" in args else args[args.index("--expr")+1]
 paths=json.loads(json.loads(re.search(r'paths = builtins.fromJSON (.+?);',expression).group(1)))
 metadata={{"ripgrep":("ripgrep","15.2.0","grep-like searcher"),"gnugrep":("grep","3.11","search tool"),"zzz-empty":("zzz-empty","1.0","")}}
 metadata.update({{"aws-library":("aws","1.0","AWS library"),"aws-c-http":("aws-c-http","0.11.0","C99 implementation of HTTP"),"aws-sdk-cpp":("aws-sdk-cpp","1.0","C++ interface for Amazon Web Services"),"awscli2":("awscli2","2.0","Universal command line interface for AWS"),"claws-mail":("claws-mail","1.0","Email client"),"awscli2-unwrapped":("awscli2","2.0","Universal command line interface for AWS"),"python312Packages.awscli":("awscli","1.0","AWS command line interface")}})
 metadata["probe"]=("probe","1","probing tool")
 metadata.update({{"tool-"+str(i):("tool-"+str(i),"1.0","grep helper") for i in range(25)}})
 rows=[]
 for path in paths:
  if len(path)==2: rows.append({{"path":path,"kind":"children","children":[key for key in metadata if "." not in key]+["python312Packages"] if path[0]=="legacyPackages" else ["probe"]}})
  elif path[-1]=="python312Packages": rows.append({{"path":path,"kind":"children","children":["awscli"]}})
  elif ".".join(path[2:]) not in metadata: rows.append({{"path":path,"kind":"skip"}})
  else:
   name,version,description=metadata[".".join(path[2:])]
   if path[0]=="legacyPackages" and path[-1]=="probe": description="unrelated"
   rows.append({{"path":path,"kind":"package","meta":{{"pname":name,"version":version,"description":description}}}})
 print(json.dumps(rows))
elif "eval" in args:
 with open({log},"a") as log: log.write("index\n")
 print(open({index_file_literal}).read())
else: sys.exit(9)
"#, log = serde_json::to_string(log.to_str().unwrap()).unwrap(), index_file_literal = serde_json::to_string(index_file.to_str().unwrap()).unwrap(), formulae_literal = serde_json::to_string(formulae.to_str().unwrap()).unwrap())).expect("write fake nix");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&nix).expect("metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&nix, permissions).expect("chmod");
        }
        let cache = home.path().join("cache");
        let mut context = Self {
            bin,
            home,
            nix,
            log,
            cache,
            formulae,
            original_runtime: String::new(),
        };
        let (code, _, stderr) = context.run(&["setup", "--no-background"]);
        assert_eq!(code, 0, "{stderr}");
        publish_search_tap(&context.home.path().join("state"));
        context.original_runtime = std::fs::read_to_string(&context.nix).unwrap();
        std::fs::write(
            &context.nix,
            "#!/bin/sh\necho unexpected-runtime-call >&2\nexit 99\n",
        )
        .unwrap();
        context
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_pkg"))
            .args(args)
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.bin.path().display()),
            )
            .env("HOME", self.home.path())
            .env("XDG_STATE_HOME", self.home.path().join("state"))
            .env("XDG_CACHE_HOME", &self.cache)
            .output()
            .expect("spawn pkg");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(&self.log).expect("log written")
    }

    fn pkg_cache(&self) -> std::path::PathBuf {
        self.cache.join("pkg")
    }
}

#[test]
fn prepared_catalogs_reuse_snapshot_without_runtime_calls() {
    let context = SearchContext::ready();
    std::fs::write(
        &context.nix,
        "#!/bin/sh\necho unexpected-runtime-call >&2\nexit 99\n",
    )
    .unwrap();
    let (code, stdout, stderr) = context.run(&["search", "ripgrep"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("nixpkgs:ripgrep"), "{stdout}");
    let calls = context.calls();
    assert_eq!(
        calls.lines().count(),
        6,
        "five batches + one index eval: {calls}"
    );
    assert!(calls.contains("batch"), "{calls}");
    let snapshots = std::fs::read_dir(context.pkg_cache())
        .expect("cache dir")
        .flatten()
        .filter(|entry| {
            entry.file_name().to_string_lossy().starts_with("catalog-")
                && !entry.file_name().to_string_lossy().ends_with(".pkg-lock")
        })
        .count();
    assert_eq!(snapshots, 4, "three catalogs and one checked marker");
    let (code, stdout, stderr) = context.run(&["search", "grep|searcher", "--regex"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("nixpkgs:ripgrep"), "{stdout}");
    assert!(stdout.contains("nixpkgs:gnugrep"), "{stdout}");
    assert!(!stdout.contains("zzz-empty"), "{stdout}");
    assert_eq!(context.calls().lines().count(), 6, "snapshot reused");
}

#[test]
fn native_regex_matches_attribute_and_empty_description() {
    let context = SearchContext::ready();
    let (code, stdout, stderr) =
        context.run(&["search", "^legacyPackages\\.x86_64-linux\\.zzz", "--regex"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("nixpkgs:zzz-empty"), "{stdout}");
    assert!(!stdout.contains("nixpkgs:ripgrep"), "{stdout}");
    let (code, stdout, stderr) = context.run(&["search", "^$", "--regex"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("nixpkgs:zzz-empty"), "{stdout}");
    assert!(!stdout.contains("nixpkgs:ripgrep"), "{stdout}");
    assert!(!stdout.contains("nixpkgs:gnugrep"), "{stdout}");
    assert_eq!(context.calls().lines().count(), 6, "no new runtime calls");
}

#[test]
fn invalid_regex_fails_before_runtime_calls() {
    let context = SearchContext::ready();
    let (code, _, stderr) = context.run(&["search", "(", "--regex"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("Invalid search pattern"), "{stderr}");
    assert_eq!(context.calls().lines().count(), 6, "no runtime call");
}

#[test]
fn search_output_has_order_limits_and_json_totals() {
    let context = SearchContext::ready();
    let (code, stdout, stderr) =
        context.run(&["search", "grep", "--limit", "2", "--source", "nixpkgs"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.find("nixpkgs:gnugrep").unwrap() < stdout.find("nixpkgs:ripgrep").unwrap(),
        "{stdout}"
    );
    assert!(stdout.contains("Showing 2 of 27 matches"), "{stdout}");
    let (code, stdout, stderr) = context.run(&["search", "grep", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["result"]["total"], 27);
    assert_eq!(value["result"]["results"].as_array().unwrap().len(), 27);
    assert_eq!(value["result"]["sources"][0]["status"], "cached");
}

#[test]
fn literal_search_treats_regex_characters_as_text() {
    let context = SearchContext::ready();
    let (code, stdout, stderr) = context.run(&["search", "grep|searcher"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("No matches"), "literal pipe: {stdout}");
    let (code, stdout, stderr) = context.run(&["search", "["]);
    assert_eq!(code, 0, "literal bracket: {stderr}");
    assert!(stdout.contains("No matches"), "{stdout}");
}

#[test]
fn failed_update_retains_complete_snapshot() {
    let context = SearchContext::ready();
    let (code, _, stderr) = context.run(&["update", "--catalogs-only"]);
    assert_eq!(code, 1, "refresh must fail: {stderr}");
    let (code, stdout, stderr) = context.run(&["search", "ripgrep"]);
    assert_eq!(code, 0, "old complete catalog survives: {stderr}");
    assert!(stdout.contains("nixpkgs:ripgrep"), "{stdout}");
}

#[test]
fn search_limit_preserves_total_and_rejects_conflicting_options() {
    let context = SearchContext::ready();
    let (code, stdout, stderr) = context.run(&["search", "grep", "--json", "--limit", "2"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["result"]["total"], 27);
    assert_eq!(value["result"]["results"].as_array().unwrap().len(), 2);
    let (code, _, _) = context.run(&["search", "grep", "--all", "--limit", "2"]);
    assert_eq!(code, 2);
}

#[test]
fn normal_aws_search_ranks_user_facing_packages() {
    let context = SearchContext::ready();
    let (code, stdout, stderr) = context.run(&["search", "aws", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let ids = value["result"]["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        ids.iter().copied().find(|id| id.starts_with("nixpkgs:")),
        Some("nixpkgs:awscli2")
    );
    assert!(ids.contains(&"brew:homebrew/core/awscli"));
    assert!(ids.contains(&"cask:homebrew/cask/aws-vault-binary"));
    assert!(ids.contains(&"cask:acme/tools/aws-console"));
    assert!(!ids.iter().any(|id| id.contains("aws-disabled")
        || id.contains("aws-deprecated")
        || id.contains("aws-framework")));
    assert!(!ids.iter().any(|id| id.contains("aws-library")
        || id.contains("aws-c-http")
        || id.contains("aws-sdk-cpp")
        || id.contains("claws")
        || id.contains("python")
        || id.contains('@')
        || id.contains("unwrapped")));
    assert_eq!(value["result"]["approximate"], false);
}

#[test]
fn search_limit_keeps_source_groups() {
    let context = SearchContext::ready();
    let (code, stdout, stderr) = context.run(&["search", "aws", "--limit", "4"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("Nixpkgs (")
            && stdout.contains("Homebrew Formulae (")
            && stdout.contains("Homebrew Casks (")
            && stdout.contains("Cask tap: acme/tools ("),
        "{stdout}"
    );
    for id in [
        "nixpkgs:awscli2",
        "brew:homebrew/core/awscli",
        "cask:homebrew/cask/aws-vault-binary",
        "cask:acme/tools/aws-console",
    ] {
        assert!(stdout.contains(id), "{stdout}");
    }
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.starts_with("nixpkgs:")
                || line.starts_with("brew:")
                || line.starts_with("cask:"))
            .count(),
        4,
        "{stdout}"
    );
}

#[test]
fn formula_aliases_have_canonical_install_commands() {
    let context = SearchContext::ready();
    for query in ["brew:awscli@2", "brew:homebrew/core/awscli"] {
        let (code, stdout, stderr) = context.run(&["search", query, "--json", "--limit", "1"]);
        assert_eq!(code, 0, "{stderr}");
        let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            value["result"]["results"][0]["id"],
            "brew:homebrew/core/awscli"
        );
        assert_eq!(
            value["result"]["results"][0]["install_command"],
            serde_json::json!(["pkg", "install", "brew:homebrew/core/awscli"])
        );
    }
}

#[test]
fn source_filters_and_exact_formula_queries_preserve_visibility() {
    let context = SearchContext::ready();
    for source in ["brew", "homebrew/core"] {
        let (code, stdout, stderr) = context.run(&[
            "search", "aws", "--source", source, "--json", "--limit", "2",
        ]);
        assert_eq!(code, 0, "{stderr}");
        let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(value["result"]["total"], 26);
        assert_eq!(value["result"]["results"].as_array().unwrap().len(), 2);
        assert_eq!(value["result"]["sources"].as_array().unwrap().len(), 1);
    }
    let (code, stdout, stderr) = context.run(&["search", "aws-disabled", "--source", "brew"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("disabled"), "{stdout}");
    for query in ["aws sdk", "aws-framework"] {
        let (code, stdout, stderr) = context.run(&["search", query, "--source", "brew"]);
        assert_eq!(code, 0, "{stderr}");
        assert!(
            stdout.contains("brew:homebrew/core/aws-framework"),
            "{stdout}"
        );
    }
    let (code, stdout, stderr) = context.run(&["search", "aws", "--source", "homebrew", "--all"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("brew:homebrew/core/awscli")
            && stdout.contains("cask:homebrew/cask/aws-vault-binary"),
        "{stdout}"
    );
    assert!(!stdout.contains("nixpkgs:"), "{stdout}");
    assert!(context.formulae.exists());
}

#[test]
fn invalid_formula_refresh_retains_previous_snapshot() {
    let context = SearchContext::ready();
    let formula_snapshot = std::fs::read_dir(context.pkg_cache())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .is_some_and(|value| value["kind"] == "formula-index")
        })
        .unwrap();
    let complete = std::fs::read(&formula_snapshot).unwrap();
    std::fs::write(&context.formulae, "invalid JSON").unwrap();
    std::fs::write(
        &context.nix,
        context.original_runtime.replace("\"b\"*40", "\"c\"*40"),
    )
    .unwrap();
    let (code, _, stderr) = context.run(&["update", "--catalogs-only"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("formula catalog index"), "{stderr}");
    assert_eq!(std::fs::read(&formula_snapshot).unwrap(), complete);
    std::fs::write(&context.nix, "#!/bin/sh\nexit 99\n").unwrap();
}

#[test]
fn broad_search_includes_low_priority_matches() {
    let context = SearchContext::ready();
    let (code, stdout, stderr) = context.run(&["search", "aws", "--broad", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    for extra in [
        "claws-mail",
        "aws-library",
        "aws-c-http",
        "aws-sdk-cpp",
        "python312Packages.awscli",
        "corretto@11",
        "awscli2-unwrapped",
    ] {
        assert!(stdout.contains(extra), "{extra}: {stdout}");
    }
}

#[test]
fn explicit_source_filters_select_one_source() {
    let context = SearchContext::ready();
    for source in ["homebrew/cask", "acme/tools", "nixpkgs"] {
        let (code, stdout, stderr) = context.run(&["search", "aws", "--source", source, "--json"]);
        assert_eq!(code, 0, "{stderr}");
        let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(value["result"]["sources"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["result"]["results"].as_array().unwrap().len(),
            1,
            "{stdout}"
        );
    }
}

#[test]
fn duplicate_native_names_keep_distinct_attributes() {
    let context = SearchContext::ready();
    for (query, expected) in [("probe", 2), ("probing", 1)] {
        let (code, stdout, stderr) =
            context.run(&["search", query, "--source", "nixpkgs", "--json"]);
        assert_eq!(code, 0, "{stderr}");
        let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let rows = value["result"]["results"].as_array().unwrap();
        assert_eq!(rows.len(), expected, "{stdout}");
        assert!(
            rows.iter()
                .any(|row| row["id"] == "nixpkgs:packages.x86_64-linux.probe"),
            "{stdout}"
        );
        if expected == 2 {
            assert!(
                rows.iter()
                    .any(|row| row["id"] == "nixpkgs:legacyPackages.x86_64-linux.probe"),
                "{stdout}"
            );
        }
    }
}

#[test]
fn query_prefixes_combine_with_source_filters() {
    let context = SearchContext::ready();
    for (query, source) in [("cask:aws", "acme/tools"), ("cask:acme/tools/aws", "cask")] {
        let (code, stdout, stderr) = context.run(&["search", query, "--source", source, "--json"]);
        assert_eq!(code, 0, "{stderr}");
        let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(value["result"]["sources"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["result"]["results"][0]["id"],
            "cask:acme/tools/aws-console"
        );
    }
    let (code, stdout, stderr) = context.run(&["search", "cask:aws"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("cask:homebrew/cask/aws-vault-binary"));
    assert!(stdout.contains("cask:acme/tools/aws-console"));
    assert!(!stdout.contains("nixpkgs:"));
    let (code, stdout, stderr) = context.run(&["search", "cask:acme/tools/aws"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("cask:acme/tools/aws-console"));
    assert!(!stdout.contains("homebrew/cask"));
    let (code, stdout, stderr) = context.run(&["search", "nixpkgs:aws"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("nixpkgs:awscli2"));
    assert!(!stdout.contains("cask:"));
    let (code, _, _) = context.run(&["search", "nixpkgs:aws", "--source", "cask"]);
    assert_eq!(code, 2);
}

#[test]
fn multiword_search_uses_description_terms() {
    let context = SearchContext::ready();
    let (code, stdout, stderr) = context.run(&["search", "aws credentials"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("cask:homebrew/cask/aws-vault-binary"));
    assert!(!stdout.contains("awscli2"));
    let (code, stdout, stderr) = context.run(&["search", "aws sdk", "--source", "nixpkgs"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("nixpkgs:aws-sdk-cpp"));
    assert!(!stdout.contains("awscli2"));
}

#[test]
fn fuzzy_fallback_yields_to_later_literal_match() {
    let context = SearchContext::ready();
    let (code, stdout, stderr) =
        context.run(&["search", "ripgrpe", "--source", "nixpkgs", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["result"]["approximate"], true);
    assert_eq!(value["result"]["results"][0]["id"], "nixpkgs:ripgrep");
    let (code, stdout, stderr) =
        context.run(&["search", "ripgrpe", "--source", "nixpkgs", "--no-fuzzy"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("No matches"));
    let (code, stdout, stderr) = context.run(&["search", "ripgrpe", "--json", "--limit", "1"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["result"]["approximate"], false);
    assert_eq!(value["result"]["total"], 1);
    assert_eq!(
        value["result"]["results"][0]["id"],
        "cask:acme/tools/ripgrpe"
    );
}

#[test]
fn empty_query_is_usage_error() {
    let context = SearchContext::ready();
    let (code, _, _) = context.run(&["search", " "]);
    assert_eq!(code, 2);
}

#[test]
fn broken_registry_is_isolated_from_selected_sources() {
    let context = SearchContext::ready();
    let registry = context.home.path().join("state/pkg/taps/registry.json");
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    let healthy_registry = std::fs::read_to_string(&registry).unwrap();
    std::fs::write(&registry, "broken registry").unwrap();
    let (code, stdout, stderr) = context.run(&["search", "aws", "--source", "brew"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("brew:homebrew/core/awscli"));
    let (code, stdout, stderr) = context.run(&["search", "ripgrep", "--source", "nixpkgs"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.is_empty(),
        "unrelated registry is not read: {stderr}"
    );
    assert!(stdout.contains("nixpkgs:ripgrep"));
    let (code, stdout, stderr) = context.run(&["search", "ripgrep"]);
    assert_eq!(code, 0, "healthy rows remain: {stderr}");
    assert!(stdout.contains("nixpkgs:ripgrep"));
    assert!(stderr.contains("Results are incomplete"), "{stderr}");
    std::fs::write(registry, healthy_registry).unwrap();
}

#[test]
fn stale_snapshots_remain_visible_and_marked() {
    let context = SearchContext::ready();
    for path in std::fs::read_dir(context.pkg_cache())
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
    {
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
            && path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("catalog-")
        {
            let mut value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            value["saved_unix"] = serde_json::json!(1);
            std::fs::write(path, serde_json::to_string(&value).unwrap()).unwrap();
        }
    }
    let (code, stdout, stderr) = context.run(&["search", "ripgrep", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["result"]["sources"][0]["status"], "stale");
    assert_eq!(value["result"]["results"][0]["stale"], true);
}

#[test]
fn bare_install_miss_uses_targeted_metadata_with_related_packages() {
    let context = SearchContext::ready();
    std::fs::write(&context.nix, &context.original_runtime).unwrap();
    let before = context.calls().lines().count();
    let (code, stdout, stderr) = context.run(&["install", "aws"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stdout.is_empty());
    assert!(stderr.contains("needs an exact package ID"), "{stderr}");
    assert!(
        stderr.contains("Related packages:") && stderr.contains("nixpkgs:awscli2"),
        "{stderr}"
    );
    assert!(!stderr.contains("Installing:"));
    assert_eq!(
        context.calls().lines().count() - before,
        2,
        "one exact metadata batch and one cask index; no full scan"
    );
}

#[test]
fn cold_bare_install_miss_uses_targeted_metadata() {
    let context = SearchContext::ready();
    std::fs::write(&context.nix, &context.original_runtime).unwrap();
    for path in std::fs::read_dir(context.pkg_cache())
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
    {
        if path
            .extension()
            .is_some_and(|extension| extension == "sqlite")
        {
            std::fs::remove_file(path).unwrap();
        }
    }
    let before = context.calls().lines().count();
    let (code, _, stderr) = context.run(&["install", "aws"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("no supported exact match"), "{stderr}");
    assert_eq!(
        context.calls().lines().count() - before,
        2,
        "cold miss uses targeted metadata evaluation"
    );
}
