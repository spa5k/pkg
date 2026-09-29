//! Direct checks for the reduced client grammar and path rules.

use std::process::Command;

fn pkg(args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
    command.args(args).env_remove("XDG_STATE_HOME");
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
}

#[test]
fn removed_commands_are_usage_errors() {
    for removed in ["pin", "unpin", "outdated", "repair", "gc"] {
        let (code, _, stderr) = pkg(&[removed], &[]);
        assert_eq!(code, 2, "`{removed}` must fail as a usage error");
        assert!(
            stderr.contains("removed") || stderr.contains("Usage"),
            "`{removed}` diagnostic should explain the removal"
        );
    }
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
    let (code, _, _) = pkg(&["shellenv"], &[]);
    assert_eq!(code, 0);
}

#[test]
fn relative_xdg_bases_are_rejected() {
    let (code, _, stderr) = pkg(&["shellenv"], &[("XDG_STATE_HOME", "relative/state")]);
    assert_eq!(code, 1);
    assert!(stderr.contains("absolute"), "stderr: {stderr}");
}

#[test]
fn prune_requires_an_explicit_positive_age() {
    let (code, _, stderr) = pkg(&["prune", "--older-than", "0d"], &[]);
    assert_eq!(code, 1);
    assert!(stderr.contains("positive"));
    let (code, _, _) = pkg(&["prune"], &[]);
    assert_eq!(code, 2, "missing --older-than is a usage error");
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
fn run_with_home(args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let home = tempfile::tempdir().expect("tempdir");
    let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
    command
        .args(args)
        .env_remove("NO_COLOR")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_CONFIG_HOME")
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
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_CONFIG_HOME")
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
        let (code, stdout, stderr) = run_with_home(&["doctor"], &[("PATH", "/usr/bin:/bin")]);
        let _ = code; // daemon health is machine state; discovery is not.
        let expected = system_profile_nix_version();
        assert!(
            stdout.contains(&expected),
            "the system profile Nix must be found and probed without PATH: {stdout}{stderr}"
        );
    } else {
        let (code, stdout, stderr) = run_with_home(&["doctor"], &[("PATH", "/nonexistent")]);
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
/// assert exactly what each machine can prove — the host Nix never masks
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
        let (code, stdout, stderr) = run_pkg_under_home(home.path(), &["doctor"], "/usr/bin:/bin");
        assert_eq!(
            code, 0,
            "tier {tier} must satisfy doctor without PATH\nstdout: {stdout}\nstderr: {stderr}"
        );
        if system_profile_nix_present() {
            assert!(
                stdout.contains(&system_profile_nix_version()),
                "the real system profile runtime wins over tier {tier}: {stdout}"
            );
        } else {
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
        &["doctor"],
        &format!("{}:/usr/bin:/bin", plain.path().display()),
    );
    assert_eq!(
        code, 0,
        "the non-executable PATH candidate must be skipped\nstdout: {stdout}\nstderr: {stderr}"
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
        &["doctor"],
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
    let (code, stdout, stderr) = run_pkg_under_home(home.path(), &["doctor"], "/usr/bin:/bin");
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
    // (The version probe inside discovery always ran without options; that
    // pre-existing behavior is unchanged.)
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
    let (code, stdout, stderr) = run(&["doctor", "--no-color"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("no-color=1"),
        "NO_COLOR forwarded: stdout: {stdout}\nstderr: {stderr}"
    );
    // Without the flag the child sees no NO_COLOR.
    let (code, stdout, stderr) = run(&["doctor"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("no-color=unset"),
        "no ambient NO_COLOR: stdout: {stdout}\nstderr: {stderr}"
    );
}

/// The cask lane answers from the generated catalog index through a fake
/// Nix runtime: info reports an excluded token with its recorded reason,
/// install refuses excluded and unknown tokens before any profile change,
/// and search returns eligible cask rows without evaluating derivations.
#[test]
fn cask_flows_read_the_generated_index() {
    let bin = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let nix = bin.path().join("nix");
    let catalog = r#"{"schema":"pkg-cask-catalog/3",
      "generator":{"name":"cask-catalog","version":"0.1.0"},
      "inputs":{"homebrew/cask":{"url":"github:BatteredBunny/brew-api/245947c0","revision":"245947c0",
        "sha256":"9f3b1c47ae5d2801c6a1b74f0e39c4d2a8f60c15d7e3b28a4c05f6e9d1a7b3c2",
        "license":"Homebrew license"}},
      "targets":["aarch64-darwin","x86_64-linux"],
      "macosBaseline":"15.7.7",
      "systems":{"aarch64-darwin":{"entries":{}},
        "x86_64-linux":{"entries":{
        "homebrew/cask/1password-cli":{"source":"homebrew/cask","token":"1password-cli","name":"1Password CLI",
          "description":"The 1Password command-line tool","version":"2.39.0",
          "homepage":"https://developer.1password.com/docs/cli/",
          "status":"eligible","kind":"binary","reason":null,"detail":null},
        "homebrew/cask/iterm2":{"source":"homebrew/cask","token":"iterm2","name":"iTerm2","description":null,
          "version":"3.6.11","homepage":"https://iterm2.com/","status":"excluded",
          "kind":null,"reason":"unsupported-platform","detail":null}}}}}"#;
    std::fs::write(
        &nix,
        format!(
            "#!/bin/sh\ncase \"$*\" in\n  *--version*) echo 'nix (Nix) 2.35.2';;\n  *'config show system'*) echo 'x86_64-linux';;\n  *'flake metadata'*) echo '{{\"url\":\"github:spa5k/pkg/240304?dir=nix/casks\",\"locked\":{{\"type\":\"github\",\"owner\":\"spa5k\",\"repo\":\"pkg\",\"rev\":\"2403040105060708090a0b0c0d0e0f1011121314\"}}}}';;\n  *'eval --json'*) cat <<'JSON'\n{catalog}\nJSON\n;;\n  *'profile list'*) echo '{{\"version\":3,\"elements\":{{}}}}';;\n  *) echo 'unexpected nix call: '$* >&2; exit 9;;\nesac\n"
        ),
    )
    .expect("write fake nix");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&nix).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(nix, permissions).expect("chmod");
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

    // An excluded token reports its recorded reason through info.
    let (code, stdout, _) = run(&["info", "cask:iterm2"]);
    assert_eq!(code, 0, "info reports recorded exclusion");
    assert!(stdout.contains("excluded"), "{stdout}");
    assert!(stdout.contains("unsupported-platform"), "{stdout}");

    // Install refuses the excluded token and an unknown token before any
    // profile mutation; the unexpected-call guard proves no install ran.
    // A bare token that matches no source is refused at resolution; the
    // qualified form reaches the gate and names the generated catalog.
    let (code, _, stderr) = run(&["install", "cask:iterm2"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("unsupported-platform"), "{stderr}");
    let (code, _, stderr) = run(&["install", "cask:missing-token"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("missing-token"), "{stderr}");
    let (code, _, stderr) = run(&["install", "cask:homebrew/cask/missing-token"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("not in the generated catalog"), "{stderr}");

    // Search lists only eligible cask rows and never calls nix search.
    let (code, stdout, _) = run(&["search", "iterm|1password"]);
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("cask:homebrew/cask/1password-cli"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("cask:homebrew/cask/iterm2"),
        "excluded rows stay hidden: {stdout}"
    );
    assert!(stdout.contains("eligible"), "{stdout}");
}

/// The nixpkgs lane amortizes evaluation with a revision snapshot: the
/// first search fetches the complete map once (`nix search <ref> ^`), a
/// different query at the same revision answers from the snapshot without
/// a second native search or index evaluation, and an invalid pattern
/// fails before any child runs. Proven by a fake Nix runtime that logs
/// every search and eval call.
#[test]
fn search_snapshots_amortize_new_queries() {
    let bin = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let nix = bin.path().join("nix");
    let log = bin.path().join("calls.log");
    std::fs::write(
        &nix,
        format!(
            "#!/bin/sh\nargs=\"$*\"\ncase \"$args\" in\n  *--version*) echo 'nix (Nix) 2.35.2';;\n  *'config show system'*) echo 'x86_64-linux';;\n  *'flake metadata'*nixpkgs*) echo '{{\"url\":\"github:NixOS/nixpkgs/nixpkgs-unstable\",\"locked\":{{\"type\":\"github\",\"owner\":\"NixOS\",\"repo\":\"nixpkgs\",\"rev\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}}}}';;\n  *'flake metadata'*) echo '{{\"url\":\"github:spa5k/pkg/240304?dir=nix/casks\",\"locked\":{{\"type\":\"github\",\"owner\":\"spa5k\",\"repo\":\"pkg\",\"rev\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"}}}}';;\n  *'search'*) echo \"search $args\" >> {log}; echo '{{\"legacyPackages.x86_64-linux.ripgrep\":{{\"pname\":\"ripgrep\",\"version\":\"15.2.0\",\"description\":\"grep-like searcher\"}},\"legacyPackages.x86_64-linux.gnugrep\":{{\"pname\":\"grep\",\"version\":\"3.11\",\"description\":\"search tool\"}},\"legacyPackages.x86_64-linux.zzz-empty\":{{\"pname\":\"zzz-empty\",\"version\":\"1.0\",\"description\":\"\"}}}}';;\n  *'eval --json'*) echo \"eval $args\" >> {log}; echo '{{\"schema\":\"pkg-cask-catalog/3\",\"generator\":{{\"name\":\"cask-catalog\",\"version\":\"0.1.0\"}},\"inputs\":{{\"homebrew/cask\":{{\"url\":\"https://example.com/cask.json\",\"revision\":\"245947c0\",\"sha256\":\"0000000000000000000000000000000000000000000000000000000000000000\",\"license\":\"Homebrew license\"}}}},\"targets\":[\"x86_64-linux\"],\"macosBaseline\":\"15.7.7\",\"systems\":{{\"x86_64-linux\":{{\"entries\":{{}}}}}}}}';;\n  *) echo 'unexpected nix call: '$* >&2; exit 9;;\nesac\n",
            log = log.display()
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
    let cache = home.path().join("cache");
    let run = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pkg"));
        command
            .args(args)
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env("PATH", format!("{}:/usr/bin:/bin", bin.path().display()))
            .env("HOME", home.path())
            .env("XDG_STATE_HOME", home.path().join("state"))
            .env("XDG_CACHE_HOME", &cache);
        let output = command.output().expect("spawn pkg");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };

    // First query: one full native search (pattern `^`) and one index eval.
    let (code, stdout, stderr) = run(&["search", "ripgrep"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("nixpkgs:ripgrep"), "{stdout}");
    assert!(stdout.contains("[fresh]"), "{stdout}");
    let calls = std::fs::read_to_string(&log).expect("log written");
    assert_eq!(calls.lines().count(), 2, "one search + one eval: {calls}");
    assert!(calls.contains("search "), "{calls}");
    // A snapshot file was written for both sources.
    let pkg_cache = cache.join("pkg");
    let snapshots = std::fs::read_dir(&pkg_cache)
        .expect("cache dir")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("catalog-"))
        .count();
    assert_eq!(snapshots, 2, "one snapshot per source");

    // A different query at the same revision: no new search, no new eval.
    let (code, stdout, stderr) = run(&["search", "grep|searcher"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("nixpkgs:ripgrep"), "{stdout}");
    assert!(stdout.contains("nixpkgs:gnugrep"), "{stdout}");
    let calls = std::fs::read_to_string(&log).expect("log written");
    assert_eq!(calls.lines().count(), 2, "snapshot reused: {calls}");
    assert!(stdout.contains("[fresh]"), "{stdout}");
    // The zzz-empty fixture stays hidden here: its name and description
    // (empty) do not match `grep|searcher`.
    assert!(!stdout.contains("zzz-empty"), "{stdout}");

    // Native matching fields, locally: a pattern anchored past the pname
    // matches only through the full attribute path.
    let (code, stdout, stderr) = run(&["search", "^legacyPackages\\.x86_64-linux\\.zzz"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("nixpkgs:zzz-empty"), "{stdout}");
    assert!(!stdout.contains("nixpkgs:ripgrep"), "{stdout}");
    let calls = std::fs::read_to_string(&log).expect("log written");
    assert_eq!(calls.lines().count(), 2, "no new calls: {calls}");

    // `^$` matches an empty description natively; it is not skipped.
    let (code, stdout, stderr) = run(&["search", "^$"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("nixpkgs:zzz-empty"), "{stdout}");
    assert!(!stdout.contains("nixpkgs:ripgrep"), "{stdout}");
    assert!(!stdout.contains("nixpkgs:gnugrep"), "{stdout}");
    let calls = std::fs::read_to_string(&log).expect("log written");
    assert_eq!(calls.lines().count(), 2, "no new calls: {calls}");

    // An invalid pattern fails before any child runs.
    let (code, _, stderr) = run(&["search", "("]);
    assert_eq!(code, 1);
    assert!(stderr.contains("invalid regex"), "{stderr}");
    let calls = std::fs::read_to_string(&log).expect("log written");
    assert_eq!(calls.lines().count(), 2, "no new calls: {calls}");
}

/// Percent-encode one path the way a `path:` flake reference is encoded.
fn encode_ref(path: &str) -> String {
    let mut out = String::new();
    for &byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The upgrade refusal rules for removed taps, through a fake Nix runtime:
/// an explicitly named entry from an unregistered tap is refused before
/// any `profile upgrade` runs, and `--all` skips such entries with a note
/// and passes only the explicit remaining IDs — never `--all`.
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
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env_remove("XDG_CONFIG_HOME")
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

    // `--all` skips the removed tap entry with a clear note and passes the
    // remaining entry IDs explicitly — never `--all`.
    let (code, stdout, stderr) = run(&["upgrade", "--all"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("tap-entry"), "skip note: {stdout}");
    assert!(stdout.contains("keeping locked outputs"), "{stdout}");
    let recorded = std::fs::read_to_string(&args_file).expect("upgrade args recorded");
    assert!(recorded.contains("github-entry"), "{recorded}");
    assert!(!recorded.contains("tap-entry"), "{recorded}");
    assert!(!recorded.contains("--all"), "{recorded}");
}

/// Successful flake installs reduce native stderr to signal, not chatter:
/// git fetch and evaluation lines are dropped, meaningful warnings stay,
/// Nix's interactive trust prompt passes through before its newline
/// exists, and the profile result is still reported. Proven with a fake
/// Nix runtime that replays the Helix-style stderr captured from a real
/// install.
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
    assert!(stdout.contains("Installed 1 entry"), "stdout: {stdout}");
    for hidden in [
        "fetching Git repository",
        "Receiving objects",
        "Unpacking objects",
        "redirecting to",
        "evaluating derivation",
        "unexpected nix call",
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
    assert!(stderr.contains("error: Cannot build"), "stderr: {stderr}");
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

/// A sandbox refusal stays concise and offers the one-time setup instead
/// of replaying the importer's full setup paragraph. Without a terminal
/// nothing privileged runs: the command exits with the short manual path.
#[test]
fn tap_add_sandbox_refusal_is_concise_and_offers_setup() {
    let bin = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let nix = bin.path().join("nix");
    let script = r##"#!/bin/sh
case "$*" in
  *--version*) echo 'nix (Nix) 2.35.2';;
  *'config show system'*) printf '%s' "$FAKE_SYSTEM";;
  *'config show'*--json*) printf '%s' '{"sandbox":{"value":false},"sandbox-fallback":{"value":true}}';;
  *'config show'*) echo 'false';;
  *) echo 'unexpected nix call: '$* >&2; exit 9;;
esac
"##;
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
