//! Compiled CLI checks with temporary homes and controlled child output.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests may use unwrap, expect, and panic to fail on broken fixtures"
)]

use std::process::Command;

#[test]
fn help_lists_d4_commands_and_no_removed_ones() {
    let (code, stdout, _) = pkg(&["--help"], &[]);
    assert_eq!(code, 0);
    for kept in [
        "search",
        "info",
        "install",
        "list",
        "remove",
        "update",
        "upgrade",
        "history",
        "rollback",
        "prune",
        "doctor",
        "shellenv",
        "completion",
        "apps",
    ] {
        assert!(stdout.contains(kept), "help must list {kept}");
    }
    for removed in [
        "pin", "unpin", "outdated", "repair", "gc", "verify", "system",
    ] {
        assert!(
            !stdout.contains(&format!(" {removed} ")),
            "help must not list {removed}"
        );
    }
    assert!(stdout.contains("[alias: add]"), "{stdout}");
    assert!(stdout.contains("[alias: uninstall]"), "{stdout}");
    for shell in ["bash", "zsh", "fish"] {
        let (code, completion, stderr) = pkg(&["completion", shell], &[]);
        assert_eq!(code, 0, "{stderr}");
        assert!(
            completion.contains("add") && completion.contains("uninstall"),
            "{shell}: {completion}"
        );
    }
}

#[test]
fn invalid_command_combinations_fail_before_loading_config() {
    let home = tempfile::tempdir().expect("temporary home");
    let config = home.path().join("config/pkg");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("config.toml"), "invalid = [").unwrap();
    for args in [
        &["pin", "ripgrep"][..],
        &["unpin", "ripgrep"],
        &["outdated"],
        &["repair"],
        &["gc"],
        &["verify"],
        &["system", "uninstall"],
        &["--dry-run", "install", "nixpkgs:ripgrep"],
        &["install", "--keep-first", "nixpkgs:ripgrep"],
        &["install"],
        &["add"],
        &["remove"],
        &["uninstall"],
        &["upgrade"],
        &["upgrade", "--all", "ripgrep"],
        &["upgrade", "--all", "--entry"],
    ] {
        let (code, stdout, stderr) = run_pkg_under_home(home.path(), args, "/nonexistent");
        assert_eq!(code, 2, "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        assert!(stderr.contains("Usage:"), "{args:?}: {stderr}");
        assert!(!stderr.contains("invalid config"), "{args:?}: {stderr}");
    }
    assert!(!home.path().join("state").exists());
    assert!(!home.path().join("cache").exists());
}

#[test]
fn shellenv_is_idempotent_and_uses_the_dedicated_profile() {
    // Spaces, quotes, dollars, and backticks in the XDG path must stay
    // literal, and evaluating the output twice must not duplicate the entry.
    let state = "/tmp/pkg we'ird \"dq $HOME `tick`";
    let (code, stdout, _) = pkg(&["shellenv"], &[("XDG_STATE_HOME", state)]);
    assert_eq!(code, 0);
    assert_eq!(stdout.matches("export PATH").count(), 1, "one export line");
    let bin = "/tmp/pkg we'ird \"dq $HOME `tick`/nix/profiles/pkg/bin";
    let script = format!("{stdout}{stdout}printf '%s\\n' \"$PATH\"");
    // An unrelated entry that merely ends with the bin path must not satisfy
    // the guard: the exact entry is still added, exactly once. The starting
    // PATH keeps /usr/bin:/bin so the shell itself stays spawnable.
    let decoy = format!("/usr/other{bin}:/usr/bin:/bin");
    for shell in ["bash", "zsh"] {
        for start_path in ["/usr/bin:/bin", decoy.as_str()] {
            let output = match Command::new(shell)
                .arg("-c")
                .arg(&script)
                .env("PATH", start_path)
                .output()
            {
                Ok(output) => output,
                Err(_) if shell == "zsh" => break,
                Err(error) => panic!("`{shell}` must run the shellenv script: {error}"),
            };
            assert!(output.status.success(), "`{shell}` must accept the script");
            let path = String::from_utf8_lossy(&output.stdout).into_owned();
            let hits = path.split(':').filter(|entry| *entry == bin).count();
            assert_eq!(
                hits, 1,
                "`{shell}` must hold the exact entry once (start PATH {start_path}): {path}"
            );
        }
    }
}

#[test]
fn relative_xdg_bases_are_rejected() {
    for variable in ["XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME"] {
        let (code, stdout, stderr) = pkg(&["shellenv"], &[(variable, "relative/base")]);
        assert_eq!(code, 1, "{variable}: {stderr}");
        assert!(stdout.is_empty(), "{variable}: {stdout}");
        assert!(
            stderr.contains(variable) && stderr.contains("absolute"),
            "{stderr}"
        );
    }
}

#[test]
fn prune_requires_an_explicit_positive_age() {
    for (age, diagnostic) in [
        ("0d", "positive"),
        ("30", "end in `d`"),
        ("-1d", "number of days"),
        ("", "end in `d`"),
        ("18446744073709551616d", "number of days"),
    ] {
        let (code, stdout, stderr) = pkg(&["prune", &format!("--older-than={age}")], &[]);
        assert_eq!(code, 1, "{age:?}: {stderr}");
        assert!(stdout.is_empty(), "{age:?}: {stdout}");
        assert!(stderr.contains(diagnostic), "{age:?}: {stderr}");
        assert!(
            !stderr.contains("Nix"),
            "validation before runtime: {stderr}"
        );
    }
    let (code, stdout, stderr) = pkg(&["prune"], &[]);
    assert_eq!(code, 2, "{stderr}");
    assert_eq!(stdout, "");
}

#[test]
fn invalid_config_is_reported_without_replacing_the_file() {
    let home = tempfile::tempdir().expect("temporary home");
    let config = home.path().join("config/pkg");
    std::fs::create_dir_all(&config).unwrap();
    let file = config.join("config.toml");
    for text in [
        "invalid = [",
        "sources = 3",
        "soruces = {}",
        "[sources]\nnixpkg = 'github:NixOS/nixpkgs'",
        "[display]\ncolour = 'never'",
        "[runtime]\nnix_path = '/usr/bin/nix'",
    ] {
        std::fs::write(&file, text).unwrap();
        let (code, stdout, stderr) = run_pkg_under_home(home.path(), &["doctor"], "/nonexistent");
        assert_eq!(code, 1, "{text}: {stderr}");
        assert!(stdout.is_empty(), "{text}: {stdout}");
        assert!(stderr.contains("invalid config"), "{text}: {stderr}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    }
    assert!(!home.path().join("state").exists());
    assert!(!home.path().join("cache").exists());
}

#[test]
fn profile_queries_preserve_native_state_without_creating_a_lock() {
    let home = tempfile::tempdir().expect("temporary home");
    let manifest = serde_json::json!({
        "version": 3,
        "elements": { "native-hello": {
            "active": true,
            "attrPath": "legacyPackages.x86_64-linux.hello",
            "originalUrl": "github:NixOS/nixpkgs/nixpkgs-unstable",
            "url": "github:NixOS/nixpkgs/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "storePaths": []
        }}
    })
    .to_string();
    let (nix, _runtime) = fake_nix(&format!(
        "case \"$*\" in\n\
         *--version*) echo 'nix (Nix) 2.35.2';;\n\
         *'profile list'*) echo '{manifest}';;\n\
         *'profile history'*) echo 'Version 3 (2026-10-18)';;\n\
         *) echo \"unexpected nix call: $*\" >&2; exit 9;;\n\
         esac"
    ));
    let path = format!(
        "{}:/usr/bin:/bin",
        nix.parent().expect("runtime directory").display()
    );

    let (code, stdout, stderr) = run_pkg_under_home(home.path(), &["list", "--json"], &path);
    assert_eq!(code, 0, "{stderr}");
    let fresh: serde_json::Value = serde_json::from_str(&stdout).expect("list JSON");
    assert_eq!(fresh["result"], serde_json::json!([]));
    assert!(!home.path().join("state").exists());

    let profiles = home.path().join("state/nix/profiles");
    let generation = profiles.join("pkg-3-link");
    std::fs::create_dir_all(&generation).expect("generation directory");
    std::fs::write(generation.join("manifest.json"), &manifest).expect("native manifest");
    let profile = profiles.join("pkg");
    std::os::unix::fs::symlink("pkg-3-link", &profile).expect("active generation");

    let (code, stdout, stderr) = run_pkg_under_home(home.path(), &["list", "--json"], &path);
    assert_eq!(code, 0, "{stderr}");
    let installed: serde_json::Value = serde_json::from_str(&stdout).expect("list JSON");
    assert_eq!(
        installed["result"]
            .as_array()
            .expect("installed entries")
            .len(),
        1
    );
    assert_eq!(installed["result"][0]["entry_id"], "native-hello");
    assert_eq!(installed["result"][0]["package_id"], "nixpkgs:hello");

    let (code, stdout, stderr) = run_pkg_under_home(home.path(), &["history"], &path);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("3  2026-10-18  current"), "{stdout}");
    assert_eq!(
        std::fs::read_link(&profile).expect("profile link"),
        std::path::Path::new("pkg-3-link")
    );
    assert_eq!(
        std::fs::read_to_string(generation.join("manifest.json")).expect("native manifest"),
        manifest
    );
    assert_eq!(
        std::fs::read_dir(&profiles)
            .expect("profile directory")
            .count(),
        2
    );
    for untouched in ["cache", "config", "state/pkg", "Applications"] {
        assert!(!home.path().join(untouched).exists(), "{untouched}");
    }
}

#[test]
fn malformed_ids_are_refused_before_the_runtime_is_needed() {
    // Validation must not depend on a present Nix runtime.
    for args in [
        vec!["info", "nixpkgs:"],
        vec!["info", "nixpkgs:a..b"],
        vec!["info", "cask:"],
        vec!["info", "github:owner"],
        vec!["info", "github:owner/repo"],
        vec!["info", "path:/tmp/foo#default"],
        vec!["info", "https://example.com/repo#default"],
        vec!["info", "flake#attr"],
        vec!["install", "nixpkgs:"],
    ] {
        let (code, _, stderr) = pkg(&args, &[("PATH", "/nonexistent")]);
        let joined = args.join(" ");
        assert_eq!(code, 1, "`{joined}` must be refused");
        assert!(
            stderr.contains("pkg:") && !stderr.contains("not found\n"),
            "`{joined}` should fail with a validation message: {stderr}"
        );
    }
}

#[test]
fn invalid_search_pattern_is_a_usage_error_before_runtime_discovery() {
    let (code, stdout, stderr) = pkg(&["search", "[", "--regex"], &[("PATH", "/nonexistent")]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        stderr.contains("Invalid search pattern"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("Nix executable"), "stderr: {stderr}");
}

#[test]
fn cold_search_gives_setup_guidance_without_a_runtime() {
    let (code, stdout, stderr) = pkg(&["search", "ripgrep"], &[("PATH", "/nonexistent")]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("search data is not ready"), "{stderr}");
    assert!(stderr.contains("pkg setup"), "{stderr}");
    assert!(
        !stderr.contains("Nix executable"),
        "no runtime discovery: {stderr}"
    );
}

/// Whether `path` is a regular file with an executable bit, mirroring the
/// discovery candidate rule.
#[cfg(unix)]
fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_file(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Whether this machine holds a usable Nix at the stable system profile
/// path. Machines with such a Nix exercise discovery with the real
/// runtime; machines without it (the CI runners) exercise the per-user
/// profile tiers with the fakes below. Tests branch on this machine state
/// instead of letting the host Nix silently mask a fake candidate.
fn system_profile_nix_present() -> bool {
    is_executable_file(std::path::Path::new(
        "/nix/var/nix/profiles/default/bin/nix",
    ))
}

/// The version line of the real system profile Nix, probed once. Discovery
/// on a machine that holds one must find and probe exactly this runtime
/// when PATH holds no Nix.
fn system_profile_nix_version() -> String {
    let output = Command::new("/nix/var/nix/profiles/default/bin/nix")
        .arg("--version")
        .output()
        .expect("probe the real system profile Nix");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Write one executable fake `nix` and return its path with the directory
/// that keeps it alive for the test.
fn fake_nix(body: &str) -> (std::path::PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nix");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake nix");
    make_executable(&path);
    (path, dir)
}

/// One fake `nix` body that answers the three read-only probes doctor
/// runs, marking the version line with `marker`; any other call fails
/// loudly so an unexpected native call fails the test.
fn doctor_fake_body(marker: &str) -> String {
    format!(
        "case \"$*\" in
          *--version*) echo 'nix (Nix) {marker}';;
          *'store info'*) echo 'store reachable';;
          *'profile list'*) echo '{{\"version\":3,\"elements\":{{}}}}';;
          *'config show'*) echo 'false';;
          *) echo 'unexpected nix call: '$* >&2; exit 9;;
        esac"
    )
}

/// Install one executable fake `nix` at an exact path (parents created)
/// and return its text path.
fn install_fake_nix_at(path: &std::path::Path, body: &str) -> String {
    let (nix, _keep) = fake_nix(body);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::copy(&nix, path).expect("copy fake nix");
    make_executable(path);
    path.display().to_string()
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(not(unix))]
fn make_executable(_path: &std::path::Path) {}

/// Run the real client with one isolated fresh home: fresh XDG bases and
/// environment entries the caller chooses. Subprocess isolation keeps the
/// test away from the runner's real files; the standard profile paths
/// under the fresh home hold nothing unless the test creates them.
fn pkg(args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let home = tempfile::tempdir().expect("tempdir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
    command
        .args(args)
        .env_remove("NO_COLOR")
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_CONFIG_HOME", home.path().join("config"));
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("spawn pkg");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Run the real client under one prepared isolated home with one `PATH`.
fn run_pkg_under_home(home: &std::path::Path, args: &[&str], path: &str) -> (i32, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
    command
        .args(args)
        .env_remove("NO_COLOR")
        .env("HOME", home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("PATH", path);
    let output = command.output().expect("spawn pkg");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn missing_nix_gives_vendor_guidance_not_a_silent_result() {
    // Doctor is read-only and must fail (nonzero) with guidance when Nix
    // is absent, without starting any installer (spec: external-nix-runtime).
    // The home is isolated, so the per-user standard path holds nothing.
    // The system profile path is machine state, so both of its states are
    // asserted: with a real Nix there, discovery must find it without PATH
    // (the alpha.4 fix); with nothing there, the not-found guidance must
    // appear with the searched locations.
    if system_profile_nix_present() {
        let (code, stdout, stderr) = pkg(&["doctor", "--verbose"], &[("PATH", "/usr/bin:/bin")]);
        let _ = code; // daemon health is machine state; discovery is not.
        let expected = system_profile_nix_version();
        assert!(
            stdout.contains(&expected),
            "the system profile Nix must be found and probed without PATH: {stdout}{stderr}"
        );
    } else {
        let (code, stdout, stderr) = pkg(&["doctor", "--verbose"], &[("PATH", "/nonexistent")]);
        assert_eq!(code, 1);
        let text = format!("{stdout}{stderr}");
        assert!(text.contains("Determinate"), "guidance: {text}");
        assert!(text.contains("standard Nix locations"), "locations: {text}");
    }
}

/// The per-user profile locations are searched after PATH: with no usable
/// PATH candidate, doctor becomes healthy without manual PATH setup. Both
/// tiers are exercised with fakes under isolated homes. On a machine with
/// a real system profile Nix that runtime wins instead, and the branches
/// assert which runtime each machine selects. The host Nix never masks
/// a fake candidate silently.
#[test]
fn doctor_finds_a_user_profile_nix_when_path_has_none() {
    for (tier, marker) in [
        ("state/nix/profile/bin/nix", "2.35.2-modern"),
        (".nix-profile/bin/nix", "2.35.2-legacy"),
    ] {
        // run_pkg_under_home points XDG_STATE_HOME at <home>/state, so the
        // first tier is the modern per-user state profile and the second
        // the legacy one; each iteration starts from an empty home.
        let home = tempfile::tempdir().expect("tempdir");
        let fake = install_fake_nix_at(&home.path().join(tier), &doctor_fake_body(marker));
        let (code, stdout, stderr) =
            run_pkg_under_home(home.path(), &["doctor", "--verbose"], "/usr/bin:/bin");
        if system_profile_nix_present() {
            assert!(
                stdout.contains(&system_profile_nix_version()),
                "the real system profile runtime wins over tier {tier}: {stdout}"
            );
        } else {
            assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
            assert!(
                stdout.contains(&fake),
                "tier {tier} must be reported: {stdout}"
            );
            assert!(
                stdout.contains(marker),
                "the tier {tier} fake runtime is the one probed: {stdout}"
            );
        }
    }
}

/// Non-executable candidates are skipped: a plain file named `nix` on PATH
/// does not stop discovery, and the standard profile candidate is used.
#[test]
fn non_executable_candidates_are_skipped() {
    let home = tempfile::tempdir().expect("tempdir");
    let fake = install_fake_nix_at(
        &home.path().join(".nix-profile/bin/nix"),
        &doctor_fake_body("2.35.2-userprofile"),
    );
    let plain = tempfile::tempdir().expect("tempdir");
    let plain_nix = plain.path().join("nix");
    std::fs::write(&plain_nix, "not executable").expect("write plain file");
    let (code, stdout, stderr) = run_pkg_under_home(
        home.path(),
        &["doctor", "--verbose"],
        &format!("{}:/usr/bin:/bin", plain.path().display()),
    );
    assert!(
        !stdout.contains(plain.path().display().to_string().as_str()),
        "the plain file must never be the runtime: {stdout}"
    );
    if system_profile_nix_present() {
        assert!(
            stdout.contains(&system_profile_nix_version()),
            "the real system profile runtime wins: {stdout}"
        );
    } else {
        assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
        assert!(stdout.contains(&fake), "{stdout}");
    }
}

/// A configured `runtime.nix` selects its runtime and, when unusable,
/// fails alone instead of falling back to discoverable candidates.
#[test]
fn configured_runtime_selects_and_fails_alone() {
    // Usable configured value: its unique version marker reaches the row.
    let (nix, _keep) = fake_nix(
        "case \"$*\" in
          *--version*) echo 'nix (Nix) 9.9.9-configured';;
          *'store info'*) echo 'store reachable';;
          *'profile list'*) echo '{\"version\":3,\"elements\":{}}';;
          *) exit 0;;
        esac",
    );
    let home = tempfile::tempdir().expect("tempdir");
    install_fake_nix_at(&home.path().join(".nix-profile/bin/nix"), "exit 1");
    let config = home.path().join("config/pkg");
    std::fs::create_dir_all(&config).expect("mkdir");
    std::fs::write(
        config.join("config.toml"),
        format!("[runtime]\nnix = '{}'\n", nix.display()),
    )
    .expect("write config");
    let (code, stdout, stderr) = run_pkg_under_home(
        home.path(),
        &["doctor", "--verbose"],
        &format!("{}:/usr/bin:/bin", nix.parent().expect("parent").display()),
    );
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("9.9.9-configured"), "{stdout}");

    // Unusable configured value: the error names it and refuses fallback,
    // even though a usable standard profile Nix exists in the same home.
    let home = tempfile::tempdir().expect("tempdir");
    let fake = install_fake_nix_at(&home.path().join(".nix-profile/bin/nix"), "exit 0");
    let config = home.path().join("config/pkg");
    std::fs::create_dir_all(&config).expect("mkdir");
    let configured = home.path().join("missing/nix");
    std::fs::write(
        config.join("config.toml"),
        format!("[runtime]\nnix = '{}'\n", configured.display()),
    )
    .expect("write config");
    let (code, stdout, stderr) =
        run_pkg_under_home(home.path(), &["doctor", "--verbose"], "/usr/bin:/bin");
    assert_eq!(code, 1);
    let text = format!("{stdout}{stderr}");
    assert!(
        text.contains(&configured.display().to_string()),
        "the configured value is named: {text}"
    );
    assert!(text.contains("does not fall back"), "{text}");
    assert!(
        !stdout.contains(&fake),
        "no fallback runtime may be reported: {stdout}"
    );
}

#[test]
fn doctor_forwards_verbose_and_no_color_to_native_children() {
    // Regression: doctor built its runtime without the session's output
    // options, so `pkg doctor --verbose` traced nothing and `--no-color`
    // never reached native children as NO_COLOR. The fake runtime reports
    // NO_COLOR through its store-info text, which doctor prints verbatim,
    // and answers `profile list` with a valid empty native manifest (the
    // read-only apps status asks for it even when the profile is absent).
    let bin = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let nix = bin.path().join("nix");
    std::fs::write(
        &nix,
        "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'nix (Nix) 2.35.2';;\n  *\"store info\"*) echo \"daemon reachable no-color=${NO_COLOR:-unset}\";;\n  *\"profile list\"*) echo '{\"version\":3,\"elements\":{}}';;\n  *) exit 0;;\nesac\n",
    )
    .expect("write fake nix");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&nix).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&nix, permissions).expect("chmod");
    }
    // Local runner with a fully isolated environment: fresh HOME and XDG
    // bases, and NO_COLOR removed so the "unset" control case stays true
    // even when the test runner itself inherits NO_COLOR=1. The global
    // pkg() helper keeps its semantics.
    let run = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
        command
            .args(args)
            .env_remove("NO_COLOR")
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
            .env("HOME", home.path())
            .env("XDG_STATE_HOME", home.path().join("state"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .env("XDG_CONFIG_HOME", home.path().join("config"));
        let output = command.output().expect("spawn pkg");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };

    // `--verbose` traces every native probe doctor runs.
    let (code, stdout, stderr) = run(&["doctor", "--verbose"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("pkg: nix "), "verbose trace: {stderr}");
    // Without the flag no trace appears.
    let (code, stdout, stderr) = run(&["doctor"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        !stderr.contains("pkg: nix "),
        "no trace by default: {stderr}"
    );
    // `--no-color` forwards NO_COLOR=1 to the native children.
    let (code, stdout, stderr) = run(&["doctor", "--verbose", "--no-color"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("no-color=1"),
        "NO_COLOR forwarded: stdout: {stdout}\nstderr: {stderr}"
    );
    // Without the flag the child sees no NO_COLOR.
    let (code, stdout, stderr) = run(&["doctor", "--verbose"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("no-color=unset"),
        "no ambient NO_COLOR: stdout: {stdout}\nstderr: {stderr}"
    );
}

/// Percent-encode one path the way a `path:` flake reference is encoded.
fn encode_ref(path: &str) -> String {
    let mut out = String::new();
    for &byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            use std::fmt::Write as _;
            write!(&mut out, "%{byte:02X}").expect("writing to String cannot fail");
        }
    }
    out
}

/// The upgrade refusal rules for removed taps, through a fake Nix runtime:
/// an explicitly named entry from an unregistered tap is refused before
/// any `profile upgrade` runs, and `--all` skips such entries with a note
/// and passes only the explicit remaining IDs without `--all`.
#[test]
fn upgrade_refuses_removed_taps_for_explicit_and_bulk_targets() {
    let bin = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let state = home.path().join("state");
    let args_file = home.path().join("upgrade-args");
    let tap_ref = format!(
        "path:{}/pkg/taps/src/somebody/apps/current",
        encode_ref(state.to_str().expect("utf-8 state path"))
    );
    let manifest = format!(
        r#"{{"version":3,"elements":{{
          "fixed-entry":{{"active":true,"attrPath":"packages.x86_64-linux.fixed",
            "originalUrl":"github:owner/repo/2403040105060708090a0b0c0d0e0f1011121314",
            "url":"github:owner/repo/2403040105060708090a0b0c0d0e0f1011121314",
            "storePaths":["/nix/store/fixed"]}},
          "github-entry":{{"active":true,"attrPath":"packages.x86_64-linux.tool",
            "originalUrl":"github:owner/repo","url":"github:owner/repo/2403040105060708090a0b0c0d0e0f1011121314",
            "storePaths":["/nix/store/x"]}},
          "tap-entry":{{"active":true,"attrPath":"packages.x86_64-linux.somebody-apps-tool",
            "originalUrl":"{tap_ref}","url":"{tap_ref}",
            "storePaths":["/nix/store/y"]}}}}}}"#
    );
    let nix = bin.path().join("nix");
    std::fs::write(
        &nix,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'nix (Nix) 2.35.2';;\n  *'profile list'*) cat <<'JSON'\n{manifest}\nJSON\n;;\n  *'profile upgrade'*) printf '%s\\n' \"$@\" > '{}'; exit 0;;\n  *) echo 'unexpected nix call: '$* >&2; exit 9;;\nesac\n",
            args_file.display()
        ),
    )
    .expect("write fake nix");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&nix).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&nix, permissions).expect("chmod");
    }
    // A registry that holds one unrelated tap but not `somebody/apps`.
    // The dedicated profile must exist, or the entry list reads as empty.
    std::fs::create_dir_all(state.join("nix").join("profiles").join("pkg")).expect("profile");
    let registry = state.join("pkg").join("taps");
    std::fs::create_dir_all(&registry).expect("mkdir");
    std::fs::write(
        registry.join("registry.json"),
        r#"{"schema":"pkg-tap-registry/1","sources":{
          "other/cli":{"origin":"https://github.com/other/homebrew-cli","added_unix":1}}}"#,
    )
    .expect("write registry");

    let run = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
        command
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
            .env("HOME", home.path())
            .env("XDG_STATE_HOME", &state)
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .env("XDG_CONFIG_HOME", home.path().join("config"));
        let output = command.output().expect("spawn pkg");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };

    // An explicitly named entry from the removed tap is refused before the
    // native upgrade: no `profile upgrade` call happens at all.
    let (code, _, stderr) = run(&["upgrade", "tap-entry"]);
    assert_eq!(code, 1, "explicit removed-tap entry must fail");
    assert!(
        stderr.contains("is removed; upgrade refuses to follow it"),
        "stderr: {stderr}"
    );
    assert!(
        !args_file.exists(),
        "no native upgrade may run for a removed tap entry"
    );

    let (code, stdout, stderr) = run(&["upgrade", "fixed-entry", "tap-entry", "--entry"]);
    assert_eq!(
        code, 1,
        "mixed explicit upgrade must refuse the removed tap"
    );
    assert!(
        stderr.contains("is removed; upgrade refuses to follow it"),
        "{stderr}"
    );
    assert!(
        stdout.is_empty(),
        "refusal precedes fixed-reference skip output: {stdout}"
    );
    assert!(
        !args_file.exists(),
        "the full batch must pass before mutation"
    );

    // `--all` skips the removed tap entry with a clear note and passes the
    // remaining entry IDs explicitly without `--all`.
    let (code, stdout, stderr) = run(&["upgrade", "--all"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("tap-entry"), "skip note: {stdout}");
    assert!(stdout.contains("keeping locked outputs"), "{stdout}");
    let recorded = std::fs::read_to_string(&args_file).expect("upgrade args recorded");
    assert!(recorded.contains("github-entry"), "{recorded}");
    assert!(!recorded.contains("tap-entry"), "{recorded}");
    assert!(!recorded.contains("fixed-entry"), "{recorded}");
    assert!(!recorded.contains("--all"), "{recorded}");
}

/// Filter fetch and evaluation messages while preserving warnings, interactive
/// prompts, and the profile result. Replay stderr captured from a Helix install.
#[test]
fn install_filters_native_chatter_and_keeps_warnings_and_prompts() {
    let bin = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let nix = bin.path().join("nix");
    let noisy = concat!(
        "printf \"fetching Git repository 'https://gitlab.com/gabmus/tree-sitter-blueprint'\\n\" >&2\n",
        "printf \"warning: redirecting to https://gitlab.com/gabmus/tree-sitter-blueprint.git/\\n\" >&2\n",
        "printf \"remote: Enumerating objects: 32, done.\\n\" >&2\n",
        "printf \"Receiving objects: 100%% (32/32), 42.45 KiB | 1.70 MiB/s, done.\\n\" >&2\n",
        "printf \"Unpacking objects: 100%% (28/28), done.\\n\" >&2\n",
        "printf \"warning: not writing modified lock file of flake 'github:hraban/mac-app-util/test':\\n\" >&2\n",
        "printf \"• Updated input 'nixpkgs':\\n    'github:NixOS/nixpkgs/80d591e' (2026-07-02)\\n  → 'github:NixOS/nixpkgs/b6c8664' (2026-09-29)\\n\" >&2\n",
        "printf \"warning: ignoring untrusted substituter 'https://helix.cachix.org'\\n\" >&2\n",
        "printf \"evaluating derivation 'github:helix-editor/helix#helix'...\\n\" >&2\n",
        "printf \"do you want to allow configuration setting 'extra-substituters' to be set (y/N)? \" >&2\n",
        "printf \"\\n\"\n"
    );
    std::fs::write(
        &nix,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'nix (Nix) 2.35.2';;\n  *'config show system'*) echo 'x86_64-linux';;\n  *'flake metadata'*) echo '{{\"url\":\"github:helix-editor/helix\",\"locked\":{{\"type\":\"github\",\"owner\":\"helix-editor\",\"repo\":\"helix\",\"rev\":\"ba40e547426b0f9896c8bdc699a4ab11f2b37dbc\"}}}}';;\n  *'profile add'*) {noisy};;\n  *'profile list'*) echo '{{\"version\":3,\"elements\":{{}}}}';;\n  *) echo 'unexpected nix call: '$* >&2; exit 9;;\nesac\n"
        ),
    )
    .expect("write fake nix");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&nix).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&nix, permissions).expect("chmod");
    }
    let run = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
        command
            .args(args)
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
            .env("HOME", home.path())
            .env("XDG_STATE_HOME", home.path().join("state"))
            .env("XDG_CACHE_HOME", home.path().join("cache"));
        let output = command.output().expect("spawn pkg");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };

    let (code, stdout, stderr) = run(&["install", "github:helix-editor/helix#helix"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("Unchanged: profile did not change."),
        "the fake runtime leaves the profile unchanged: {stdout}"
    );
    for hidden in [
        "fetching Git repository",
        "Receiving objects",
        "Unpacking objects",
        "redirecting to",
        "evaluating derivation",
        "unexpected nix call",
        "not writing modified lock file",
        "Updated input",
        "github:NixOS/nixpkgs/b6c8664",
    ] {
        assert!(
            !stderr.contains(hidden),
            "filtered output leaked: {hidden}\nstderr: {stderr}"
        );
    }
    assert!(
        stderr.contains("ignoring untrusted substituter"),
        "warnings must stay visible\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("do you want to allow configuration setting"),
        "trust prompts must stay visible\nstderr: {stderr}"
    );
    let (code, _, stderr) = run(&["--verbose", "install", "github:helix-editor/helix#helix"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("not writing modified lock file"),
        "{stderr}"
    );
    assert!(stderr.contains("Updated input"), "{stderr}");
}

#[test]
fn removal_filters_ignored_errors_but_preserves_failure_causes() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let nix = root.path().join("nix");
    let manifest = root.path().join("manifest.json");
    let removed = root.path().join("removed");
    let fail = root.path().join("fail");
    let profile = root.path().join("state/nix/profiles/pkg");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
    std::fs::write(profile, "fixture").unwrap();
    std::fs::write(
        &manifest,
        serde_json::json!({
            "version": 3,
            "elements": { "native-fff": {
                "active": true,
                "attrPath": "legacyPackages.x86_64-linux.fff",
                "originalUrl": "github:NixOS/nixpkgs/nixpkgs-unstable",
                "url": "github:NixOS/nixpkgs/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "storePaths": []
            }}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        &nix,
        format!(
            r#"#!/bin/sh
case "$*" in
  *--version*) echo 'nix (Nix) 2.35.2';;
  *'profile list'*)
    if [ -f '{removed}' ]; then echo '{{"version":3,"elements":{{}}}}'; else cat '{manifest}'; fi;;
  *'profile remove'*)
    echo 'error (ignored): opening file "/etc/nix/sentry-endpoint": Permission denied' >&2
    if [ -f '{fail}' ]; then echo 'error: permission denied while updating profile' >&2; exit 4; fi
    echo "removing 'github:NixOS/nixpkgs#legacyPackages.x86_64-linux.fff'" >&2
    echo 'warning: fixture warning' >&2
    printf 'do you want to continue (y/N)? ' >&2
    printf '\n' >&2
    touch '{removed}';;
  *) echo "unexpected nix call: $*" >&2; exit 9;;
esac
"#,
            removed = removed.display(),
            manifest = manifest.display(),
            fail = fail.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&nix, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_pkg"))
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", root.path().display()))
            .env("HOME", root.path())
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .output()
            .unwrap();
        (
            output.status.code().unwrap(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let (code, stdout, stderr) = run(&["uninstall", "fff"]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), "Removed: nixpkgs:fff");
    assert!(stderr.contains("Removing: nixpkgs:fff"), "{stderr}");
    assert!(
        stderr.contains("fixture warning") && stderr.contains("do you want to continue"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("sentry-endpoint") && !stderr.contains("removing '"),
        "{stderr}"
    );
    std::fs::remove_file(&removed).unwrap();
    let (code, _, stderr) = run(&["--verbose", "remove", "--entry", "native-fff"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("sentry-endpoint") && stderr.contains("removing '"),
        "{stderr}"
    );
    std::fs::remove_file(&removed).unwrap();
    std::fs::write(&fail, "fail").unwrap();
    let (code, stdout, stderr) = run(&["uninstall", "fff"]);
    assert_eq!(code, 1, "{stderr}");
    assert!(!stdout.contains("Removed:"), "{stdout}");
    assert!(stderr.contains("Installed entries unchanged."), "{stderr}");
    assert!(
        stderr.contains("Cause: permission denied while updating profile"),
        "{stderr}"
    );
    assert!(!stderr.contains("sentry-endpoint"), "{stderr}");
}

/// A failed install keeps its diagnostics readable: the native error and
/// its nix-log pointer survive, and the suppressed fetch chatter never
/// replays inside the failure message, no matter how much of it Nix
/// printed before failing.
#[test]
fn failed_install_reports_signal_lines_only() {
    let bin = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let nix = bin.path().join("nix");
    let noisy_fail = concat!(
        "i=0\n",
        "while [ \"$i\" -lt 60 ]; do\n",
        "  printf \"fetching Git repository 'https://example.com/r%s\\n\" \"$i\" >&2\n",
        "  i=$((i+1))\n",
        "done\n",
        "printf \"unpacking 'github:example/flake' into the Git cache...\\n\" >&2\n",
        "printf \"error: Cannot build '/nix/store/aaa-tool.drv'.\\n\" >&2\n",
        "printf '%s\\n' '> cask-build: cannot preserve an external code signature in the Nix store: python-lib.bypy.frozen:com.apple.cs.CodeEntitlements' >&2\n",
        "printf \"For full logs, run:\\n  nix log /nix/store/aaa-tool.drv\\n\" >&2\n",
        "exit 1\n"
    );
    std::fs::write(
        &nix,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'nix (Nix) 2.35.2';;\n  *'config show system'*) echo 'x86_64-linux';;\n  *'flake metadata'*) echo '{{\"url\":\"github:owner/repo\",\"locked\":{{\"type\":\"github\",\"owner\":\"owner\",\"repo\":\"repo\",\"rev\":\"1111111111111111111111111111111111111111\"}}}}';;\n  *'profile add'*) {noisy_fail};;\n  *'profile list'*) echo '{{\"version\":3,\"elements\":{{}}}}';;\n  *) echo 'unexpected nix call: '$* >&2; exit 9;;\nesac\n"
        ),
    )
    .expect("write fake nix");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&nix).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&nix, permissions).expect("chmod");
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
    command
        .args(["install", "github:owner/repo#tool"])
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CACHE_HOME", home.path().join("cache"));
    let output = command.output().expect("spawn pkg");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("Cause: cannot preserve an external code signature in the Nix store"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("nix log /nix/store/aaa-tool.drv"),
        "stderr: {stderr}"
    );
    assert!(
        !stderr.contains("fetching Git repository"),
        "chatter must not replay: {stderr}"
    );
    assert!(
        !stderr.contains("into the Git cache"),
        "unpacking chatter must stay hidden: {stderr}"
    );
}

/// A package can reach the native profile before macOS launcher setup fails.
/// The final output must call this partial work, never a complete install.
#[cfg(target_os = "macos")]
#[test]
fn install_reports_partial_state_when_app_setup_fails() {
    let home = tempfile::tempdir().expect("tempdir");
    let bundle = home
        .path()
        .join("fixture/Stats/Applications/Stats.app/Contents");
    std::fs::create_dir_all(&bundle).expect("create app bundle");
    let profile = home.path().join("state/nix/profiles/pkg");
    std::fs::create_dir_all(profile.parent().expect("profile parent"))
        .expect("create profile parent");
    let marker = home.path().join("installed-marker");
    let manifest = serde_json::json!({
        "version": 3,
        "elements": {
            "stats-entry": {
                "active": true,
                "attrPath": "packages.aarch64-darwin.Stats",
                "originalUrl": "github:owner/repo",
                "url": "github:owner/repo/1111111111111111111111111111111111111111",
                "storePaths": [home.path().join("fixture/Stats").display().to_string()]
            }
        }
    });
    let flake = serde_json::json!({
        "url": "github:owner/repo",
        "locked": {
            "type": "github", "owner": "owner", "repo": "repo",
            "rev": "1111111111111111111111111111111111111111"
        }
    });
    let (nix, _keep) = fake_nix(&format!(
        "case \"$*\" in\n\
         *--version*) echo 'nix (Nix) 2.35.2';;\n\
         *'config show system'*) echo 'aarch64-darwin';;\n\
         *'flake metadata'*) printf '%s\\n' '{flake}';;\n\
         *'profile list'*'pkg-app-tools'*) echo '{{\"version\":3,\"elements\":{{}}}}';;\n\
         *'profile list'*) if [ -f '{marker}' ]; then printf '%s\\n' '{manifest}'; else echo '{{\"version\":3,\"elements\":{{}}}}'; fi;;\n\
         *'profile add'*'hraban/mac-app-util'*) printf '%s\\n' \"error: Cannot build '/nix/store/example-system-alexandria.drv'.\" '> failed to allocate 1048576 bytes' 'For full logs, run:' '  nix log /nix/store/example-system-alexandria.drv' '❌ github:hraban/mac-app-util/example#' >&2; exit 1;;\n\
         *'profile add'*) touch '{marker}' '{profile}';;\n\
         *) echo \"unexpected nix call: $*\" >&2; exit 9;;\n\
         esac",
        marker = marker.display(),
        profile = profile.display()
    ));
    let config = home.path().join("config/pkg");
    std::fs::create_dir_all(&config).expect("create config directory");
    std::fs::write(
        config.join("config.toml"),
        format!("[runtime]\nnix = '{}'\n", nix.display()),
    )
    .expect("write config");
    let (code, stdout, stderr) = run_pkg_under_home(
        home.path(),
        &["install", "github:owner/repo#Stats"],
        &format!("{}:/usr/bin:/bin", nix.parent().expect("parent").display()),
    );
    assert_eq!(code, 1, "stdout: {stdout}\nstderr: {stderr}");
    assert!(marker.exists(), "package profile changed");
    assert!(!stdout.contains("Installed:"), "stdout: {stdout}");
    assert!(
        stderr.contains("Partial: install changed github:owner/repo#packages.aarch64-darwin.Stats"),
        "{stderr}"
    );
    assert!(stderr.contains("Cause: failed to allocate"), "{stderr}");
    assert!(
        stderr.contains("nix log /nix/store/example-system-alexandria.drv"),
        "{stderr}"
    );
    assert_eq!(stderr.matches("failed to allocate").count(), 1, "{stderr}");
    assert!(!stderr.contains("❌"), "{stderr}");
    assert!(!stderr.contains("Cause: >"), "{stderr}");
    assert!(stderr.contains("pkg apps sync"), "{stderr}");
}

/// A sandbox refusal stays concise and offers the one-time setup instead
/// of replaying the importer's full setup paragraph. Without a terminal
/// nothing privileged runs: the command exits with the short manual path.
#[test]
fn tap_add_sandbox_refusal_is_concise_and_offers_setup() {
    let bin = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let nix = bin.path().join("nix");
    let script = r#"#!/bin/sh
case "$*" in
  *--version*) echo 'nix (Nix) 2.35.2';;
  *'config show system'*) printf '%s' "$FAKE_SYSTEM";;
  *'config show'*--json*) printf '%s' '{"sandbox":{"value":false},"sandbox-fallback":{"value":true}}';;
  *'config show'*) echo 'false';;
  *) echo 'unexpected nix call: '$* >&2; exit 9;;
esac
"#;
    std::fs::write(&nix, script).expect("write fake nix");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&nix).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&nix, permissions).expect("chmod");
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
    command
        .args(["tap", "add", "somebody/apps", "--trust"])
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
        .env(
            "FAKE_SYSTEM",
            format!(
                "{}-{}",
                std::env::consts::ARCH,
                if std::env::consts::OS == "macos" {
                    "darwin"
                } else {
                    std::env::consts::OS
                }
            ),
        )
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .stdin(std::process::Stdio::null());
    let output = command.output().expect("spawn pkg");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let all = format!("{stdout}{stderr}");
    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("not proven sandboxed"),
        "the concise cause must lead: {stderr}"
    );
    assert!(
        all.contains("one-time sandbox setup"),
        "the setup offer must be named: {all}"
    );
    assert!(
        !all.contains("Concrete setup:"),
        "the importer wall of text must not replay: {all}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn background_commands_retain_signal_exits_and_schedule_context() {
    use std::os::unix::fs::PermissionsExt as _;
    for (name, signal, code) in [
        ("INT", libc::SIGINT, 130),
        ("TERM", libc::SIGTERM, 143),
        ("HUP", libc::SIGHUP, 129),
    ] {
        for action in ["enable", "disable", "status"] {
            let home = tempfile::tempdir().expect("background fixture");
            let bin = home.path().join("bin");
            let config = home.path().join("config");
            let units = config.join("systemd/user");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::create_dir_all(config.join("pkg")).unwrap();
            std::fs::create_dir_all(&units).unwrap();
            let nix = bin.join("nix");
            let systemctl = bin.join("systemctl");
            std::fs::write(&nix, "#!/bin/sh\necho 'nix (Nix) 2.35.2'\n").unwrap();
            std::fs::write(
                &systemctl,
                r"#!/usr/bin/python3
import os,signal,sys
with open(os.environ['PKG_BACKGROUND_CALLS'],'a') as calls: calls.write(' '.join(sys.argv[1:])+'\n')
sig=int(os.environ['PKG_BACKGROUND_SIGNAL'])
signal.signal(sig,signal.SIG_DFL)
os.kill(os.getpid(),sig)
",
            )
            .unwrap();
            for file in [&nix, &systemctl] {
                std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            std::fs::write(
                config.join("pkg/config.toml"),
                format!(
                    "[runtime]\nnix = {}\n",
                    serde_json::to_string(nix.to_str().unwrap()).unwrap()
                ),
            )
            .unwrap();
            if action != "enable" {
                for unit in ["pkg-catalog-refresh.service", "pkg-catalog-refresh.timer"] {
                    std::fs::write(
                        units.join(unit),
                        "# Generated by pkg; catalog refresh only.\n",
                    )
                    .unwrap();
                }
            }
            let calls = home.path().join("calls");
            let output = Command::new(env!("CARGO_BIN_EXE_pkg"))
                .args(["background", action])
                .env("HOME", home.path())
                .env("XDG_CONFIG_HOME", &config)
                .env("XDG_STATE_HOME", home.path().join("state"))
                .env("XDG_CACHE_HOME", home.path().join("cache"))
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("PKG_BACKGROUND_SIGNAL", signal.to_string())
                .env("PKG_BACKGROUND_CALLS", &calls)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(code),
                "{name}/{action}: {output:?}"
            );
            assert!(
                output.stdout.is_empty(),
                "no completion after cancellation: {output:?}"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains(&format!("systemctl was cancelled by signal {signal}")),
                "{stderr}"
            );
            if action == "status" {
                assert!(
                    stderr.contains("job files exist, but the schedule is not active"),
                    "{stderr}"
                );
            }
            assert_eq!(
                std::fs::read_to_string(&calls).unwrap().lines().count(),
                1,
                "no later control command: {output:?}"
            );
            for unit in ["pkg-catalog-refresh.service", "pkg-catalog-refresh.timer"] {
                assert!(
                    units.join(unit).exists(),
                    "job files remain after cancelled control"
                );
            }
        }
    }
}

#[test]
fn history_color_policy_applies_to_native_output() {
    let home = tempfile::tempdir().expect("temporary home");
    let (runtime, _keep) = fake_nix(
        "case \"$*\" in
  *--version*) echo 'nix (Nix) 2.35.2';;
  *'profile history'*) printf '\\033[31mVersion 3 (2026-10-17)\\033[0m\\n\\033[1;32mgen 3\\033[m plain\\033b\\033[';;
  *) exit 9;;
esac",
    );
    let config = home.path().join("config/pkg");
    std::fs::create_dir_all(&config).unwrap();
    for (color, flags, expected) in [
        (
            "auto",
            &[][..],
            "\x1b[31mVersion 3 (2026-10-17)\x1b[0m\n\x1b[1;32mgen 3\x1b[m plain\x1bb\x1b[",
        ),
        (
            "auto",
            &["--no-color"][..],
            "Version 3 (2026-10-17)\ngen 3 plain\x1bb",
        ),
        ("never", &[][..], "Version 3 (2026-10-17)\ngen 3 plain\x1bb"),
    ] {
        std::fs::write(
            config.join("config.toml"),
            format!(
                "[runtime]\nnix = '{}'\n[display]\ncolor = '{color}'\n",
                runtime.display()
            ),
        )
        .unwrap();
        let mut args = vec!["history", "--verbose"];
        args.extend_from_slice(flags);
        let (code, stdout, stderr) = run_pkg_under_home(home.path(), &args, "/usr/bin:/bin");
        assert_eq!(code, 0, "{color}, {flags:?}: {stderr}");
        assert_eq!(stdout, expected, "{color}, {flags:?}: {stderr}");
    }
}
