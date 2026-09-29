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

#[test]
fn missing_nix_gives_vendor_guidance_not_a_silent_result() {
    // Doctor is read-only and must fail (nonzero) with guidance when Nix is
    // absent, without starting any installer (spec: external-nix-runtime).
    let (code, stdout, stderr) = pkg(&["doctor"], &[("PATH", "/nonexistent")]);
    assert_eq!(code, 1);
    let text = format!("{stdout}{stderr}");
    assert!(text.contains("Determinate"), "guidance: {text}");
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
    assert!(stderr.contains("no longer registered"), "stderr: {stderr}");
    assert!(
        !args_file.exists(),
        "no native upgrade may run for a removed tap entry"
    );

    // `--all` skips the removed tap entry with a clear note and passes the
    // remaining entry IDs explicitly — never `--all`.
    let (code, stdout, stderr) = run(&["upgrade", "--all"]);
    assert_eq!(code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("tap-entry"), "skip note: {stdout}");
    assert!(stdout.contains("keeps its locked outputs"), "{stdout}");
    let recorded = std::fs::read_to_string(&args_file).expect("upgrade args recorded");
    assert!(recorded.contains("github-entry"), "{recorded}");
    assert!(!recorded.contains("tap-entry"), "{recorded}");
    assert!(!recorded.contains("--all"), "{recorded}");
}
