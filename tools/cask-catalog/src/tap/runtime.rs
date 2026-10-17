//! Host runtime controls: canary + loopback positive controls, the
//! bounded child runner, and the sandboxed Nix raw-export build.

use super::sha256_hex;
use super::{ImportError, ImportRequest};
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Trusted raw-export runtime assets embedded verbatim.
const RAW_EXPORT_NIX: &str = include_str!("../../../../nix/casks/lib/tap/raw-export.nix");
const EXPORT_RB: &str = include_str!("../../../../nix/casks/lib/tap/export.rb");
const READER_RB: &str = include_str!("../../../../nix/casks/lib/tap/reader.rb");
const PROBE_SH: &str = include_str!("../../../../nix/casks/lib/tap/probe.sh");
const BOOT_ADAPTER_SH: &str = include_str!("../../../../nix/casks/lib/tap/brew-boot-adapter.sh");

/// Nix build wall time limit (the runtime itself bounds export at 15
/// minutes; nixpkgs fetchTarball and store work need headroom).
pub(super) const NIX_WALLTIME: Duration = Duration::from_mins(45);
/// Per-stream captured Nix log limit. Overflow ABORTS the build; it
/// never silently truncates into a reported success.
const MAX_STREAM_BYTES: usize = 1024 * 1024;
/// Trusted `--file` wrapper: converts `--argstr`-provided STRINGS into
/// the argument types raw-export.nix expects. No caller-controlled
/// value is ever evaluated as a Nix expression: the tap path arrives as
/// a plain string (safe with spaces, `$`, quotes) and the canary set is
/// parsed by `builtins.fromJSON`.
const RUN_WRAPPER_NIX: &str = r#"
{ tapSourcePath
, canaryJson
, ...
}@args:
import ./raw-export.nix
  (builtins.removeAttrs args [ "tapSourcePath" "canaryJson" ] // {
    tapSource = builtins.path { path = builtins.toPath tapSourcePath; name = "tap-source"; };
    canary = builtins.fromJSON canaryJson;
  })
"#;

pub(super) struct CanaryControl {
    /// RAII guard over the public canary directory: even failures
    /// BEFORE this struct is fully built cannot leak the directory,
    /// because the `TempDir` guard owns it from creation.
    #[allow(dead_code, reason = "the guard field is owned for Drop only")]
    pub(super) dir: tempfile::TempDir,
    pub(super) path: String,
    pub(super) sha256: String,
    pub(super) port: u16,
    stop: Arc<AtomicBool>,
    listener_thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for CanaryControl {
    /// RAII cleanup on EVERY path (success, timeout, failure): stop the
    /// accept loop, reap the thread, remove ONLY the paths this
    /// control created (the nonce file, then its own directory via the
    /// `TempDir` guard).
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.listener_thread.take() {
            let _ = handle.join();
        }
        let _ = std::fs::remove_file(&self.path);
        // The TempDir guard removes the (now empty) public directory.
    }
}

/// Randomness from the OS. RNG failure fails the import: there is no
/// fallback.
fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut file = std::fs::File::open("/dev/urandom")
        .map_err(|e| format!("cannot open /dev/urandom: {e}"))?;
    let mut buf = vec![0u8; n];
    file.read_exact(&mut buf)
        .map_err(|e| format!("cannot read {n} bytes from /dev/urandom: {e}"))?;
    Ok(buf)
}

/// Create the fresh public canary and the live loopback listener, and
/// verify both positive controls from the host side before the build.
/// The canary directory is owned by a `TempDir` guard FROM CREATION, so
/// every early failure (dir, chmod, nonce write, read-back, listener
/// bind, thread spawn) cleans up; the `CanaryControl` is constructed
/// BEFORE the listener positive control runs, so even that error path
/// gets full Drop cleanup.
pub(super) fn setup_canary() -> Result<CanaryControl, String> {
    {
        use std::os::unix::fs::PermissionsExt as _;
        let nonce = random_bytes(32)?;
        // Fresh OS randomness for the directory name; no timestamp
        // fallback ever.
        // The parent MUST be the literal PUBLIC /tmp: on macOS the
        // default std temp dir has private ancestors (per-user
        // dirs), which makes the sandbox deny the canary for reasons
        // that are NOT the build's fault. Never std::env::temp_dir()
        // (inherited TMPDIR) here.
        let dir = tempfile::TempDir::with_prefix_in("pkg-import-", "/tmp")
            .map_err(|e| format!("cannot create public canary dir: {e}"))?;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777))
            .map_err(|e| format!("cannot chmod 0777 {}: {e}", dir.path().display()))?;
        let path = dir.path().join("canary");
        std::fs::write(&path, &nonce)
            .map_err(|e| format!("cannot write canary {}: {e}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| format!("cannot chmod 0644 {}: {e}", path.display()))?;
        let sha256 = sha256_hex(&nonce);
        // Positive control: the canary must read back with the exact
        // fresh nonce hash.
        let read_back =
            std::fs::read(&path).map_err(|e| format!("canary positive control failed ({e})"))?;
        if sha256_hex(&read_back) != sha256 {
            return Err("canary positive control failed: read-back hash mismatch".to_string());
        }

        // Live listener: 127.0.0.1 ephemeral port, held for the whole
        // build. Positive control: one real connect is accepted.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("cannot bind listener: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("listener has no local address: {e}"))?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("cannot make listener nonblocking: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let served = Arc::new(AtomicBool::new(false));
        let served_for_thread = Arc::clone(&served);
        let handle = std::thread::Builder::new()
            .name("pkg-canary-listener".into())
            .spawn(move || {
                while !stop_for_thread.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            served_for_thread.store(true, Ordering::SeqCst);
                            drop(stream);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| format!("cannot start the canary listener: {e}"))?;
        // The control is FULLY constructed (dir guard, thread, stop
        // flag) BEFORE the listener positive control below: any error
        // from here on drops the control, stopping the thread and
        // removing the canary paths.
        let control = CanaryControl {
            dir,
            path: path.display().to_string(),
            sha256,
            port,
            stop,
            listener_thread: Some(handle),
        };
        TcpStream::connect(("127.0.0.1", port))
            .map_err(|e| format!("canary listener positive control failed ({e})"))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !served.load(Ordering::SeqCst) {
            if std::time::Instant::now() > deadline {
                return Err(
                    "canary listener positive control failed: self-connect never accepted"
                        .to_string(),
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(control)
    }
}

// ---------------------------------------------------------------------------
// Nix raw-export build (ordinary derivation, bounded process)
// ---------------------------------------------------------------------------

/// Resolve the configured Nix executable: an existing file path is
/// used directly; otherwise the name is looked up in PATH.
pub(super) fn resolve_nix_exe(nix: &Path) -> Result<PathBuf, String> {
    if nix.is_file() {
        return Ok(nix.to_path_buf());
    }
    let name = nix
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("nix executable {} is not a usable path", nix.display()))?;
    if name.contains('/') {
        return Err(format!("nix executable {} is not a file", nix.display()));
    }
    let path = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    for dir in path {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "nix executable {name:?} was not found in PATH; pass --nix /path/to/bin/nix"
    ))
}

pub(super) fn write_asset(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))
        .map_err(|e| format!("cannot stage a write next to {}: {e}", path.display()))?;
    std::io::Write::write_all(&mut tmp.as_file(), contents.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    tmp.persist(path)
        .map_err(|e| format!("cannot publish {}: {e}", path.display()))?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        let permissions = std::fs::Permissions::from_mode(0o644);
        std::fs::set_permissions(path, permissions)
            .map_err(|e| format!("cannot make {} readable: {e}", path.display()))?;
    }
    Ok(())
}
/// The fixed child `PATH` whitelist: the resolved nix executable's
/// own bin directory first, then the system default. Nothing else —
/// no caller-controlled directories, no ambient PATH entries survive
/// `env_clear`.
fn isolated_path_value(nix_exe: &Path) -> String {
    let mut path = String::new();
    if let Some(bin) = nix_exe.parent() {
        path.push_str(&bin.display().to_string());
        path.push(':');
    }
    path.push_str("/usr/bin:/bin:/usr/sbin:/sbin");
    path
}

/// The ONE shared constructor of the isolated Nix child environment,
/// used by BOTH nix child callers (the early local config gate and
/// the fresh authoritative raw-export build). It creates the CALLER'S
/// private roots (each caller passes its own `home` and `tmp`
/// directories — there is no shared or reused home/session state)
/// and returns a `Command` already carrying the complete process
/// setup the two callers previously duplicated:
/// - cleared environment (`env_clear`: no ambient `NIX_CONFIG`, no
///   proxies, no credentials, no inherited HOME),
/// - the exact whitelist `PATH` (nix bin dir + system default),
/// - `LC_ALL=C`,
/// - a private `HOME` with a `.config` directory and an empty
///   `NIX_USER_CONF_FILES` (with no HOME at all Nix infers the REAL
///   user home and its credentials),
/// - a private `TMPDIR`,
/// - null stdin and piped stdout/stderr.
///
/// Arguments, deadlines, log caps, labels, sandbox/fallback flags,
/// and process ownership stay with the callers: this constructor owns
/// environment/process setup ONLY.
pub(super) fn isolated_nix_command(
    nix_exe: &Path,
    home: &Path,
    tmp: &Path,
) -> Result<std::process::Command, String> {
    let config = home.join(".config");
    std::fs::create_dir_all(&config)
        .map_err(|e| format!("cannot create {}: {e}", config.display()))?;
    let user_conf = home.join("empty-nix.conf");
    write_asset(&user_conf, "")?;
    std::fs::create_dir_all(tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    let mut command = std::process::Command::new(nix_exe);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env_clear()
        .env("PATH", isolated_path_value(nix_exe))
        .env("TMPDIR", tmp)
        .env("LC_ALL", "C")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", &config)
        .env("NIX_USER_CONF_FILES", &user_conf);
    Ok(command)
}

pub(super) fn run_bounded_child(
    command: std::process::Command,
    walltime: Duration,
    what: &str,
) -> Result<std::process::Output, ImportError> {
    let reaped = pkg_process::run_child(
        command,
        pkg_process::IoMode::Capture,
        Some(pkg_process::ChildLimits {
            resident_bytes: None,
            wall_time: walltime,
            output_bytes: MAX_STREAM_BYTES,
        }),
        None,
    )
    .map_err(|source| ImportError::Process {
        detail: format!("cannot run the {what} process: {source}"),
        source,
    })?;
    if let pkg_process::Outcome::Interrupted { signal } = reaped.classify() {
        return Err(ImportError::Interrupted {
            signal,
            detail: format!("the {what} was interrupted; its process group was killed"),
        });
    }
    if let Some(reason) = reaped.resource_limit {
        return Err(format!("the {what} {reason} ({} s wall limit; {MAX_STREAM_BYTES} byte per-stream log cap); its process group was killed", walltime.as_secs()).into());
    }
    Ok(reaped.output)
}

/// Write the trusted embedded runtime assets into `runtime/` and build
/// the raw-export derivation with the configured Nix executable.
pub(super) fn run_nix_export(
    request: &ImportRequest,
    staging: &Path,
    tap_tree: &Path,
    source: &str,
    revision: &str,
    canary: &CanaryControl,
    walltime: Duration,
) -> Result<PathBuf, ImportError> {
    let runtime = staging.join("runtime");
    write_asset(&runtime.join("raw-export.nix"), RAW_EXPORT_NIX)?;
    write_asset(&runtime.join("run.nix"), RUN_WRAPPER_NIX)?;
    write_asset(&runtime.join("export.rb"), EXPORT_RB)?;
    write_asset(&runtime.join("reader.rb"), READER_RB)?;
    write_asset(&runtime.join("probe.sh"), PROBE_SH)?;
    write_asset(&runtime.join("brew-boot-adapter.sh"), BOOT_ADAPTER_SH)?;

    // All caller-controlled values travel as --argstr STRINGS through
    // the trusted wrapper above: no bare-path `--arg` (spaces or `${`
    // would break), no interpolated Nix expression. The canary set is
    // serde-encoded JSON parsed by builtins.fromJSON inside the
    // wrapper. Every value travels as a plain --argstr STRING: no
    // caller-controlled value is ever evaluated as a Nix expression.
    let canary_json = serde_json::json!({
        "path": canary.path,
        "sha256": canary.sha256,
        "port": canary.port,
    })
    .to_string();

    let nix_exe = resolve_nix_exe(&request.nix)?;
    let out_link = staging.join("export-out");
    // Isolated per-build roots (distinct from the config gate's): the
    // shared constructor creates them and owns the environment/process
    // setup; the build keeps its own args, deadline, label, and
    // sandbox/fallback demands below.
    let nix_build_dir = staging.join("nix-tmp");
    let nix_home = staging.join("nix-home");
    let mut command = isolated_nix_command(&nix_exe, &nix_home, &nix_build_dir)?;
    command
        .arg("build")
        .arg("--file")
        .arg(runtime.join("run.nix"))
        .arg("--argstr")
        .arg("tapSourcePath")
        .arg(tap_tree)
        .arg("--argstr")
        .arg("canaryJson")
        .arg(&canary_json)
        .arg("--argstr")
        .arg("source")
        .arg(source)
        .arg("--argstr")
        .arg("revision")
        .arg(revision)
        .arg("--argstr")
        .arg("system")
        .arg(&request.system)
        // Fail-closed sandbox demands. `sandbox-fallback false` is the
        // real option name; there is no `fallback` option. Client
        // --options are IGNORED for untrusted users on macOS; only the
        // DAEMON config counts, and the in-build probe stays
        // authoritative either way.
        .arg("--option")
        .arg("sandbox")
        .arg("true")
        .arg("--option")
        .arg("sandbox-fallback")
        .arg("false")
        // Real build-log plumbing: -L surfaces builder logs on failure,
        // and max-build-log-size makes the BUILDER itself die when it
        // exceeds 1 MiB (the same bound as the parent stdout/stderr
        // caps above — both bounds are active, they do not replace
        // each other). This is the raw build only; the config query
        // carries no log options.
        .arg("--print-build-logs")
        .arg("--option")
        .arg("max-build-log-size")
        .arg("1048576")
        .arg("--out-link")
        .arg(&out_link);

    let bounded = run_bounded_child(command, walltime, "nix raw-export build")?;
    let status = bounded.status;
    let stdout = bounded.stdout;
    let stderr = bounded.stderr;
    if !status.success() {
        let tail = |bytes: &[u8]| {
            let start = bytes.len().saturating_sub(4000);
            String::from_utf8_lossy(&bytes[start..]).to_string()
        };
        return Err(format!(
            "the nix raw-export build failed ({status}).\n\
             --- stdout tail ---\n{}\n--- stderr tail ---\n{}\n\
             Fail-closed setup checklist (macOS daemon config, trusted user):\n\
             sandbox=true, sandbox-fallback=false, minimal read-only paths, daemon \\
             TMPDIR=/nix/var/nix/builds (root:wheel 0755). Client --options \\
             are ignored for untrusted users; only the daemon config counts. \\
             The in-build sandbox probe is authoritative: host read, host \\
             write, and loopback connect must all be DENIED for the build \\
             to succeed.",
            tail(&stdout),
            tail(&stderr)
        )
        .into());
    }
    if !out_link.join("export.json").is_file() {
        return Err(format!(
            "the nix build succeeded but {} is missing export.json",
            out_link.display()
        )
        .into());
    }
    Ok(out_link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tap::native_target;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    #[cfg(unix)]
    fn fake_nix(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("fake-nix");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn fake_request(dir: &Path, nix: PathBuf) -> ImportRequest {
        ImportRequest {
            source: "example/foo".to_string(),
            revision: None,
            system: native_target().unwrap().to_string(),
            nix,
            output_dir: dir.to_path_buf(),
        }
    }
    #[cfg(unix)]
    #[test]
    fn nix_timeout_kills_the_process_group_and_returns_promptly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nix = fake_nix(
            dir.path(),
            // A DESCENDANT holds the stdout pipe open; killing only the
            // leader would leave the drain readers blocked forever.
            "sleep 300 &\nsleep 300",
        );
        let canary = setup_canary().expect("canary");
        let canary_path = PathBuf::from(&canary.path);
        let started = std::time::Instant::now();
        let err = run_nix_export(
            &fake_request(dir.path(), nix),
            dir.path(),
            dir.path(),
            "example/foo",
            REVISION,
            &canary,
            Duration::from_secs(2),
        )
        .expect_err("timeout");
        drop(canary);
        assert!(
            err.message().contains("wall limit"),
            "unexpected error: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the group kill + bounded joins must return promptly"
        );
        // RAII removed the canary paths.
        assert!(
            !canary_path.exists(),
            "RAII must remove the canary file at {}",
            canary_path.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn nix_log_flood_aborts_instead_of_truncating_into_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        // 4 MiB of stdout then exit 0: the capture must ABORT, never
        // silently truncate and report success.
        let nix = fake_nix(dir.path(), "yes 0123456789abcdef | head -c 4194304; exit 0");
        let canary = setup_canary().expect("canary");
        let err = run_nix_export(
            &fake_request(dir.path(), nix),
            dir.path(),
            dir.path(),
            "example/foo",
            REVISION,
            &canary,
            Duration::from_mins(2),
        )
        .expect_err("flood");
        drop(canary);
        assert!(err.message().contains("log cap"), "unexpected error: {err}");
    }

    // 2 MiB of stdout, then a descendant HANGS while holding the pipes
    // open: the shared overflow flag must abort the build IMMEDIATELY
    // (at the 1 MiB cap), not after EOF, and the group kill must free
    // the bounded drains.
    #[cfg(unix)]
    #[test]
    fn nix_log_flood_with_hung_descendant_aborts_within_seconds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nix = fake_nix(
            dir.path(),
            "yes 0123456789abcdef | head -c 2097152 &\nsleep 300",
        );
        let canary = setup_canary().expect("canary");
        let started = std::time::Instant::now();
        let err = run_nix_export(
            &fake_request(dir.path(), nix),
            dir.path(),
            dir.path(),
            "example/foo",
            REVISION,
            &canary,
            Duration::from_mins(2),
        )
        .expect_err("flood with hang");
        drop(canary);
        assert!(err.message().contains("log cap"), "unexpected error: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the overflow flag + group kill must abort within 5 s, took {:?}",
            started.elapsed()
        );
    }

    /// The raw-export build child must see EXACTLY the same shared
    /// constructor whitelist as the config gate (plus shell bookkeeping):
    /// the fake nix dumps its environment and exits
    /// 0 without producing export.json, so the run fails at the
    /// out-link check — AFTER the child ran — and the dump can be
    /// compared exactly. A separate test process carries only controlled
    /// sentinels, so an isolation failure cannot dump host credentials.
    #[cfg(unix)]
    #[test]
    fn nix_export_child_environment_is_exactly_the_whitelist() {
        if std::env::var_os("PKG_TAP_EXPORT_ENV_CHILD_MODE").is_none() {
            let dir = tempfile::tempdir().expect("tempdir");
            let status =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .arg("nix_export_child_environment_is_exactly_the_whitelist")
                    .env_clear()
                    .env("PKG_TAP_EXPORT_ENV_CHILD_MODE", "1")
                    .env("PKG_TAP_EXPORT_ENV_DIR", dir.path())
                    .env("NIX_CONFIG", "sentinel-nix-config")
                    .env("http_proxy", "http://127.0.0.1:9")
                    .env("https_proxy", "http://127.0.0.1:9")
                    .env("no_proxy", "sentinel-no-proxy")
                    .env("PKG_TAP_FAKE_CREDENTIAL", "sentinel-fake-token")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit())
                    .status()
                    .expect("child test process reaped");
            assert!(status.success(), "child environment checks must pass");
            return;
        }
        let dir = PathBuf::from(std::env::var("PKG_TAP_EXPORT_ENV_DIR").expect("export env dir"));
        let dump = dir.join("export-child-env");
        let nix = fake_nix(
            &dir,
            &format!("/usr/bin/env > {d}\nexit 0", d = dump.display()),
        );
        let canary = setup_canary().expect("canary");
        let err = run_nix_export(
            &fake_request(&dir, nix),
            &dir,
            &dir,
            "example/foo",
            REVISION,
            &canary,
            Duration::from_mins(1),
        )
        .expect_err("no export.json");
        drop(canary);
        assert!(
            err.message().contains("missing export.json"),
            "unexpected error: {err}"
        );
        let dumped = std::fs::read_to_string(&dump).expect("export child env dump");
        let mut seen: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for line in dumped.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            seen.insert(key.to_string(), value.to_string());
        }
        let home = dir.join("nix-home");
        let mut expected: std::collections::BTreeMap<String, String> = [
            ("HOME", home.display().to_string()),
            (
                "XDG_CONFIG_HOME",
                home.join(".config").display().to_string(),
            ),
            (
                "NIX_USER_CONF_FILES",
                home.join("empty-nix.conf").display().to_string(),
            ),
            ("TMPDIR", dir.join("nix-tmp").display().to_string()),
            ("LC_ALL", "C".to_string()),
            (
                "PATH",
                format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", dir.display()),
            ),
            // Added by /bin/sh itself (cwd bookkeeping), not by the
            // constructor; its presence is not a leak.
            (
                "PWD",
                std::env::current_dir().expect("cwd").display().to_string(),
            ),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
        if cfg!(target_os = "macos") {
            expected.insert("SHLVL".to_string(), "1".to_string());
            expected.insert("_".to_string(), "/usr/bin/env".to_string());
        }
        assert_eq!(
            seen.keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>()
        );
        for (key, value) in expected {
            assert_eq!(seen.get(&key), Some(&value), "wrong value for {key}");
        }
    }

    // The spawned Nix child runs in its OWN process group; the group
    // kill (libc only, no external kill binary) must terminate the
    #[cfg(unix)]
    #[test]
    fn bounded_child_interrupt_tears_down_group_and_restores_handlers() {
        if std::env::var("PKG_TAP_SIGNAL_CHILD_MODE").is_ok() {
            // Child mode: this process IS the manager under signal. The
            // parent supplied the working directory so it can watch the
            // same marker files.
            let root = std::path::PathBuf::from(
                std::env::var("PKG_TAP_SIGNAL_DIR").expect("signal dir env"),
            );
            let dir = std::path::PathBuf::from(&root);
            let started = dir.join("started");
            let grandchild_marker = dir.join("grandchild-lived");
            let ok_marker = dir.join("child-ok");
            // Unrelated process in THIS process's group: the interrupt
            // path must never signal it (no group 0 / manager group).
            let mut unrelated = std::process::Command::new("/bin/sleep")
                .arg("30")
                .spawn()
                .expect("unrelated sleep spawns");
            let mut command = std::process::Command::new("/bin/sh");
            command
                .arg("-c")
                .arg(format!(
                    "touch {s}; (sleep 1; touch {g}) & sleep 300",
                    s = started.display(),
                    g = grandchild_marker.display(),
                ))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let Err(error) = run_bounded_child(command, Duration::from_mins(1), "signal test")
            else {
                panic!("interruption must error, not succeed");
            };
            assert!(
                matches!(
                    error,
                    ImportError::Interrupted {
                        signal: libc::SIGTERM,
                        ..
                    }
                ),
                "unexpected error: {error}"
            );
            // The OWNED group is gone: the grandchild never runs.
            std::thread::sleep(Duration::from_millis(1500));
            assert!(
                !grandchild_marker.exists(),
                "the grandchild must be dead after the interruption"
            );
            // The unrelated process was never signalled.
            assert!(
                unrelated.try_wait().expect("poll unrelated").is_none(),
                "an unrelated same-group process must survive the interruption"
            );
            let _ = unrelated.kill();
            let _ = unrelated.wait();
            std::fs::write(&ok_marker, b"ok").expect("write ok marker");
            // The guard is dropped here; the DEFAULT SIGTERM disposition
            // is restored, so this re-raise must terminate THIS process.
            unsafe { libc::raise(libc::SIGTERM) };
            unreachable!("raise(SIGTERM) must terminate the child test process");
        }
        // Parent mode: drive the isolated child test process.
        let dir = tempfile::tempdir().expect("tempdir");
        let started = dir.path().join("started");
        let ok_marker = dir.path().join("child-ok");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("bounded_child_interrupt_tears_down_group_and_restores_handlers")
            .env("PKG_TAP_SIGNAL_CHILD_MODE", "1")
            .env("PKG_TAP_SIGNAL_DIR", dir.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("child test process spawns");
        // Wait until the bounded child was spawned and the guard is
        // registered (the `started` marker is written by the bounded
        // child itself, i.e. AFTER guard installation).
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !started.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the child test process never started its bounded child"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(100));
        // SIGTERM to the PARENT PROCESS ONLY: no group signal.
        assert_eq!(
            unsafe { libc::kill(libc::pid_t::try_from(child.id()).unwrap(), libc::SIGTERM,) },
            0
        );
        let status = child.wait().expect("child test process reaped");
        assert_eq!(
            status.code(),
            None,
            "the child must die by the re-raised SIGTERM, not exit on its own"
        );
        assert!(
            ok_marker.exists(),
            "the child must have passed all interruption checks first"
        );
    }

    /// The canary parent MUST be the literal PUBLIC /tmp (canonical
    /// alias /private/tmp on macOS), NEVER the inherited TMPDIR: a
    /// per-user private default temp dir causes false sandbox
    /// denials on macOS. Runs in a child test process so the private
    /// TMPDIR never leaks into parallel tests.
    #[cfg(unix)]
    #[test]
    fn canary_parent_is_public_tmp_not_inherited_tmpdir() {
        if std::env::var("PKG_TAP_CANARY_TMPDIR_CHILD_MODE").is_ok() {
            let canary = setup_canary().expect("canary");
            // The canary FILE lives at <tmpdir>/canary, so the temp dir
            // parent (its PUBLIC parent) is two `.parent()` hops up.
            let public_parent = std::path::Path::new(&canary.path)
                .parent()
                .and_then(|dir| dir.parent())
                .expect("canary dir has a public parent");
            let parent = public_parent
                .canonicalize()
                .expect("canary parent canonicalizes");
            assert!(
                parent == std::path::Path::new("/tmp")
                    || parent == std::path::Path::new("/private/tmp"),
                "canary parent must be the public /tmp (macOS alias /private/tmp), got {}",
                parent.display()
            );
            let inherited = std::env::temp_dir();
            assert_ne!(
                parent,
                inherited
                    .canonicalize()
                    .unwrap_or_else(|_| inherited.clone()),
                "the canary parent must not be the inherited TMPDIR"
            );
            return;
        }
        let private_tmp = tempfile::tempdir().expect("private tmpdir");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("canary_parent_is_public_tmp_not_inherited_tmpdir")
            .env("PKG_TAP_CANARY_TMPDIR_CHILD_MODE", "1")
            .env("TMPDIR", private_tmp.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("child test process spawns");
        let status = child.wait().expect("child test process reaped");
        assert!(
            status.success(),
            "the child test process must pass the canary parent checks"
        );
    }

    /// A bounded-child spawn FAILURE must still restore the previous
    /// signal handlers: after the error, a raised SIGTERM must take
    /// the DEFAULT disposition and terminate the child test process.
    #[cfg(unix)]
    #[test]
    fn bounded_child_spawn_failure_restores_signal_handlers() {
        if std::env::var("PKG_TAP_SIGNAL_SPAWNFAIL_CHILD_MODE").is_ok() {
            let dir = std::path::PathBuf::from(
                std::env::var("PKG_TAP_SIGNAL_DIR").expect("signal dir env"),
            );
            let command = std::process::Command::new("/nonexistent/pkg-spawn-failure");
            let Err(error) = run_bounded_child(command, Duration::from_secs(5), "spawn failure")
            else {
                panic!("spawn must fail");
            };
            assert!(
                error.message().contains("cannot run"),
                "unexpected error: {error}"
            );
            std::fs::write(dir.join("spawn-fail-ok"), b"ok").expect("write marker");
            // Handlers must already be restored: this re-raise must
            // terminate THIS child test process by default disposition.
            unsafe { libc::raise(libc::SIGTERM) };
            unreachable!("raise(SIGTERM) must terminate the child test process");
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("bounded_child_spawn_failure_restores_signal_handlers")
            .env("PKG_TAP_SIGNAL_SPAWNFAIL_CHILD_MODE", "1")
            .env("PKG_TAP_SIGNAL_DIR", dir.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("child test process spawns");
        let status = child.wait().expect("child test process reaped");
        assert_eq!(
            status.code(),
            None,
            "the child must die by the re-raised SIGTERM, not exit on its own"
        );
        assert!(
            dir.path().join("spawn-fail-ok").exists(),
            "the child must pass the spawn-failure checks before the re-raise"
        );
    }
}
