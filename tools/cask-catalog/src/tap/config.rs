//! Local effective Nix config gate (pre-build, fail-closed).
//!
//! The gate reads the LOCAL effective settings (`nix config show --json`)
//! so an obviously weak local setup fails fast, before any Ruby runs.
//! It is NOT an authoritative statement of daemon behavior: client
//! `--option` flags are ignored for untrusted users and the daemon may
//! hold the real state. Actual sandbox behavior is proven by the fresh
//! in-build probe, which stays mandatory and authoritative.

use super::runtime::{isolated_nix_command, resolve_nix_exe, run_bounded_child};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Effective local Nix config gate (pre-build, fail-closed)
// ---------------------------------------------------------------------------

/// Wall-time limit for the `nix config show --json` query.
const NIX_CONFIG_WALLTIME: Duration = Duration::from_secs(120);

/// The ONLY mac sandbox paths accepted when self-mapped: the minimal
/// system set the raw-export runtime needs. Omitted (minimal) entries
/// are fine; ADDED arbitrary entries are not.
const MAC_ALLOWED_SANDBOX_PATHS: [&str; 5] = [
    "/bin/sh",
    "/bin/bash",
    "/System/Library/Frameworks",
    "/System/Library/PrivateFrameworks",
    "/usr/lib",
];

/// The ONLY mac `allowed-impure-host-deps` entries accepted (a subset,
/// including empty, is fine).
const MAC_ALLOWED_IMPURE_DEPS: [&str; 4] = ["/System/Library", "/bin/sh", "/dev", "/usr/lib"];

/// The fail-closed outcome of the local effective-config gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GateError {
    /// The observed effective settings refuse the raw tap import: weak
    /// sandbox settings, added host paths, or impure dependencies. This
    /// is the refusal the client may answer with its one-time
    /// administrator setup; the message carries the concrete setup
    /// advice.
    Refused(String),
    /// The effective settings could not be observed at all (query
    /// failure or malformed JSON). The client must not offer setup for
    /// this: no setting was shown to be wrong.
    Unverifiable(String),
}

/// Concrete fail-closed setup advice. NEVER prints the config dump
/// (it may contain secret settings); only the offending detail.
fn config_setup_error(detail: &str, mac: bool) -> String {
    let mac_note = if mac {
        " On macOS the daemon must use TMPDIR=/nix/var/nix/builds because \
         a global /tmp grant is unsafe."
    } else {
        ""
    };
    format!(
        "effective local nix config refuses the raw tap import: {detail}.\
         Concrete setup: configure the nix daemon with sandbox = true and \
         sandbox-fallback = false, and remove extra unsafe host paths \
         (sandbox-paths, extra-sandbox-paths, allowed-impure-host-deps).{mac_note} \
         Do not edit the daemon config here, do not rely on client --option \
         flags (they are ignored for untrusted users); the in-build sandbox \
         probe stays mandatory and authoritative."
    )
}

/// The Nix store hash alphabet: base32 (32 characters, no `e`, `o`,
/// `u`, or `t`), NEVER hex.
const NIX_BASE32: &str = "0123456789abcdfghijklmnpqrsvwxyz";

/// Whether a path is exactly
/// `/nix/store/<32-char-base32-hash>-busybox.../bin/busybox`: the
/// single `/bin/sh` mapping accepted on Linux. Nix store hashes are
/// base32, not hex; a hex-only restriction would refuse real hosts.
fn is_busybox_sh(source: &str) -> bool {
    let Some(rest) = source.strip_prefix("/nix/store/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let Some(store) = parts.next() else {
        return false;
    };
    store.len() > 32
        && store.is_char_boundary(32)
        && store[..32]
            .bytes()
            .all(|b| NIX_BASE32.as_bytes().contains(&b))
        && store[32..].starts_with("-busybox")
        && parts.next() == Some("bin")
        && parts.next() == Some("busybox")
        && parts.next().is_none()
}

/// Boolean `.value` of one setting object, or `None` when the setting
/// is missing or its value is not a boolean (wrong shape).
fn config_bool(config: &Value, name: &str) -> Option<bool> {
    config.get(name)?.get("value")?.as_bool()
}

/// Validate one EFFECTIVE `nix config show --json` document (the
/// effective document: every setting object carries `.value`).
///
/// Fail-closed: `sandbox` must be exactly `true`, `sandbox-fallback`
/// exactly `false`, and the merged `sandbox-paths` +
/// `extra-sandbox-paths` mappings must be mandatory (optional = false)
/// and limited to the OS-specific allowlist. `allowed-impure-host-deps`
/// must be empty on Linux and a subset of the mac allowlist on macOS.
pub fn verify_effective_config(config: &Value, os: &str) -> Result<(), String> {
    let mac = os == "macos";
    match config_bool(config, "sandbox") {
        Some(true) => {}
        Some(false) => {
            return Err(config_setup_error("sandbox is false", mac));
        }
        None => {
            return Err(config_setup_error(
                "sandbox is missing or its value is not a boolean",
                mac,
            ));
        }
    }
    match config_bool(config, "sandbox-fallback") {
        Some(false) => {}
        Some(true) => {
            return Err(config_setup_error("sandbox-fallback is true", mac));
        }
        None => {
            return Err(config_setup_error(
                "sandbox-fallback is missing or its value is not a boolean",
                mac,
            ));
        }
    }

    // Merged host path mappings: sandbox-paths with extra-sandbox-paths
    // folded in. Every entry must be an object with a string `source`
    // and `optional` exactly false.
    let mut paths: BTreeMap<String, String> = BTreeMap::new();
    for name in ["sandbox-paths", "extra-sandbox-paths"] {
        let Some(setting) = config.get(name) else {
            continue;
        };
        let Some(entries) = setting.get("value").and_then(Value::as_object) else {
            return Err(config_setup_error(
                &format!("{name} has no object value (wrong shape)"),
                mac,
            ));
        };
        for (target, entry) in entries {
            let Some(entry) = entry.as_object() else {
                return Err(config_setup_error(
                    &format!("{name}[{target}] is not an object"),
                    mac,
                ));
            };
            let Some(source) = entry.get("source").and_then(Value::as_str) else {
                return Err(config_setup_error(
                    &format!("{name}[{target}] has no string source path"),
                    mac,
                ));
            };
            match entry.get("optional").and_then(Value::as_bool) {
                Some(false) => {}
                Some(true) => {
                    return Err(config_setup_error(
                        &format!(
                            "{name}[{target}] is optional; only mandatory mappings are accepted"
                        ),
                        mac,
                    ));
                }
                None => {
                    return Err(config_setup_error(
                        &format!("{name}[{target}] optional is missing or not a boolean"),
                        mac,
                    ));
                }
            }
            paths.insert(target.clone(), source.to_string());
        }
    }

    // allowed-impure-host-deps: absent (default empty) is fine.
    let impure: Vec<String> = match config.get("allowed-impure-host-deps") {
        None | Some(Value::Null) => Vec::new(),
        Some(setting) => {
            let Some(items) = setting.get("value").and_then(Value::as_array) else {
                return Err(config_setup_error(
                    "allowed-impure-host-deps has no array value (wrong shape)",
                    mac,
                ));
            };
            let mut deps = Vec::new();
            for item in items {
                let Some(dep) = item.as_str() else {
                    return Err(config_setup_error(
                        "allowed-impure-host-deps carries a non-string entry",
                        mac,
                    ));
                };
                deps.push(dep.to_string());
            }
            deps
        }
    };

    match os {
        "linux" => {
            for (target, source) in &paths {
                if target != "/bin/sh" {
                    return Err(config_setup_error(
                        &format!(
                            "linux sandbox-paths carries the added host path {target}; \
                             only an empty set or /bin/sh is accepted"
                        ),
                        mac,
                    ));
                }
                if !is_busybox_sh(source) {
                    return Err(config_setup_error(
                        &format!(
                            "linux /bin/sh maps to {source}; only \
                             /nix/store/<hash>-busybox*/bin/busybox is accepted"
                        ),
                        mac,
                    ));
                }
            }
            if !impure.is_empty() {
                return Err(config_setup_error(
                    &format!("linux allowed-impure-host-deps is not empty ({impure:?})"),
                    mac,
                ));
            }
        }
        "macos" => {
            for (target, source) in &paths {
                if !MAC_ALLOWED_SANDBOX_PATHS.contains(&target.as_str()) {
                    return Err(config_setup_error(
                        &format!(
                            "mac sandbox-paths carries the added host path {target}; \
                             only self-mapped system paths are accepted"
                        ),
                        mac,
                    ));
                }
                if source != target {
                    return Err(config_setup_error(
                        &format!("mac {target} maps to {source}; only self-mappings are accepted"),
                        mac,
                    ));
                }
            }
            for dep in &impure {
                if !MAC_ALLOWED_IMPURE_DEPS.contains(&dep.as_str()) {
                    return Err(config_setup_error(
                        &format!("mac allowed-impure-host-deps carries {dep:?}"),
                        mac,
                    ));
                }
            }
        }
        other => {
            return Err(format!(
                "effective nix config check has no rule for host os {other:?}"
            ));
        }
    }
    Ok(())
}

/// Settings that repair known macOS defaults without removing custom grants.
/// Unknown grants require an administrator's manual review.
pub fn mac_setup_settings(config: &Value) -> Result<Option<String>, String> {
    let mut normalized = config.clone();
    normalized["sandbox"] = serde_json::json!({"value": true});
    normalized["sandbox-fallback"] = serde_json::json!({"value": false});
    let mut removed_tmp = false;
    for key in ["sandbox-paths", "extra-sandbox-paths"] {
        let Some(paths) = normalized
            .get_mut(key)
            .and_then(|setting| setting.get_mut("value"))
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        for path in ["/private/tmp", "/private/var/tmp"] {
            let Some(grant) = paths.get(path) else {
                continue;
            };
            if grant.get("source").and_then(Value::as_str) != Some(path)
                || grant.get("optional").and_then(Value::as_bool) != Some(false)
            {
                return Err(format!(
                    "Review the custom {path} mapping in sandbox-paths and extra-sandbox-paths by hand.",
                ));
            }
            paths.remove(path);
            removed_tmp = true;
        }
    }
    verify_effective_config(&normalized, "macos")?;
    Ok(removed_tmp.then(|| {
        format!(
            "sandbox-paths = {}\nextra-sandbox-paths =\n",
            MAC_ALLOWED_SANDBOX_PATHS.join(" ")
        )
    }))
}

impl From<String> for GateError {
    /// Environment, executable, and bounded-run failures mean the
    /// effective settings could not be observed; they are never a
    /// refusal.
    fn from(detail: String) -> Self {
        Self::Unverifiable(detail)
    }
}

/// Query the LOCAL effective Nix settings and verify they are
/// fail-closed BEFORE any Ruby runs. The query uses the exact isolated
/// environment of the build (private HOME and XDG_CONFIG_HOME, empty
/// NIX_USER_CONF_FILES, env_clear, whitelisted executable env) and NO
/// client sandbox override: client --options are ignored for
/// untrusted users anyway. This query reads the local effective
/// settings only; it is not an authoritative statement of what the
/// daemon enforces — the fresh in-build sandbox probe proves the
/// actual behavior and stays mandatory. The full config is never
/// printed (it may contain secret settings); only bounded error tails
/// and offending setting names leave this function.
fn nix_config_gate_with_limits(
    nix: &Path,
    staging: &Path,
    walltime: Duration,
) -> Result<(), GateError> {
    let nix_exe = resolve_nix_exe(nix)?;
    // Private per-gate roots (distinct from the build's): the shared
    // constructor creates them and owns the environment/process setup;
    // the gate keeps its own args, deadline, label, and NO sandbox
    // override (client --options are ignored for untrusted users).
    let gate = staging.join("gate");
    let nix_home = gate.join("home");
    let nix_tmp = gate.join("tmp");
    let mut command = isolated_nix_command(&nix_exe, &nix_home, &nix_tmp)?;
    command.arg("config").arg("show").arg("--json");

    let bounded = run_bounded_child(&mut command, walltime, "nix config show")?;
    if !bounded.status.success() {
        let tail =
            String::from_utf8_lossy(&bounded.stderr[bounded.stderr.len().saturating_sub(2000)..]);
        return Err(GateError::Unverifiable(format!(
            "nix config show --json failed ({}); the effective local config \
             cannot be verified. tail: {tail}",
            bounded.status
        )));
    }
    let config: Value = serde_json::from_slice(&bounded.stdout).map_err(|e| {
        GateError::Unverifiable(format!(
            "nix config show --json emitted malformed JSON: {e}"
        ))
    })?;
    verify_effective_config(&config, std::env::consts::OS).map_err(GateError::Refused)
}

pub(super) fn nix_config_gate(nix: &Path, staging: &Path) -> Result<(), GateError> {
    nix_config_gate_with_limits(nix, staging, NIX_CONFIG_WALLTIME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[cfg(unix)]
    fn fake_nix(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("fake-nix");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    // ------------------------------------------------------------------
    // Effective nix config gate: pure fixture shape checks
    // ------------------------------------------------------------------

    fn strong_effective_config(os: &str) -> Value {
        let mut config = json!({
            "sandbox": {"value": true},
            "sandbox-fallback": {"value": false},
        });
        if os == "macos" {
            config["sandbox-paths"] = json!({"value": {
                "/bin/sh": {"source": "/bin/sh", "optional": false},
                "/usr/lib": {"source": "/usr/lib", "optional": false},
            }});
            config["allowed-impure-host-deps"] = json!({"value": ["/bin/sh", "/usr/lib"]});
        } else {
            config["sandbox-paths"] = json!({"value": {
                "/bin/sh": {
                    "source": format!(
                        "/nix/store/csshf3ycx2wsxjaqrva1vr8rwgp382jw-busybox-1.37.0/bin/busybox"
                    ),
                    "optional": false,
                },
            }});
            config["allowed-impure-host-deps"] = json!({"value": []});
        }
        config
    }

    #[test]
    fn config_gate_accepts_strong_shapes_on_both_oses() {
        for os in ["linux", "macos"] {
            verify_effective_config(&strong_effective_config(os), os)
                .unwrap_or_else(|e| panic!("{os} strong config must pass: {e}"));
        }
        // Linux with NO paths at all is also fine.
        let mut linux = strong_effective_config("linux");
        linux["sandbox-paths"] = json!({"value": {}});
        verify_effective_config(&linux, "linux").expect("empty linux paths pass");
        // Mac with only a minimal subset is fine (no path required).
        let mut mac = strong_effective_config("macos");
        mac["sandbox-paths"] = json!({"value": {
            "/System/Library/Frameworks": {
                "source": "/System/Library/Frameworks", "optional": false
            }
        }});
        mac["allowed-impure-host-deps"] = json!({"value": []});
        verify_effective_config(&mac, "macos").expect("minimal mac paths pass");
    }

    // ------------------------------------------------------------------
    // Actual host config fixtures (committed, real `nix config show
    // --json` captures; never /tmp-dependent).
    // ------------------------------------------------------------------

    #[test]
    fn config_gate_accepts_the_actual_linux_config_fixture_when_fail_closed() {
        // The ACTUAL fixture carries the real busybox base32 store path
        // and the strong settings, but sandbox-fallback is still true
        // (weak current host state); it must pass ONLY once fallback is
        // false.
        let mut config: Value =
            serde_json::from_str(include_str!("testdata/linux-config-fixture.json"))
                .expect("linux fixture json parses");
        config["sandbox-fallback"]["value"] = json!(false);
        verify_effective_config(&config, "linux")
            .unwrap_or_else(|e| panic!("linux config must pass once fallback is false: {e}"));
    }

    #[test]
    fn config_gate_accepts_the_actual_macos_config_fixture() {
        let config: Value =
            serde_json::from_str(include_str!("testdata/macos-config-fixture.json"))
                .expect("macos fixture json parses");
        verify_effective_config(&config, "macos")
            .unwrap_or_else(|e| panic!("actual macos config must pass: {e}"));
    }

    #[test]
    fn mac_setup_repairs_default_temp_grants_but_refuses_custom_grants() {
        let mut config = strong_effective_config("macos");
        for path in ["/private/tmp", "/private/var/tmp"] {
            config["sandbox-paths"]["value"][path] = json!({"source": path, "optional": false});
        }
        let settings = mac_setup_settings(&config).unwrap().unwrap();
        assert!(!settings.contains("/private/tmp"));
        assert!(!settings.contains("/private/var/tmp"));
        config["sandbox-paths"]["value"]["/Users/admin"] =
            json!({"source": "/Users/admin", "optional": false});
        assert!(
            mac_setup_settings(&config)
                .unwrap_err()
                .contains("/Users/admin")
        );
        config["sandbox-paths"]["value"]
            .as_object_mut()
            .unwrap()
            .remove("/Users/admin");
        config["sandbox-paths"]["value"]["/private/tmp"]["optional"] = json!(true);
        assert!(
            mac_setup_settings(&config)
                .unwrap_err()
                .contains("custom /private/tmp")
        );
    }

    #[test]
    fn config_gate_rejects_the_linux_fixture_with_fallback_true() {
        // The ACTUAL linux fixture still has sandbox-fallback true; the
        // verifier must reject it as-is.
        let config: Value =
            serde_json::from_str(include_str!("testdata/linux-config-fixture.json")).unwrap();
        let err = verify_effective_config(&config, "linux").unwrap_err();
        assert!(err.contains("sandbox-fallback is true"), "{err}");
    }

    #[test]
    fn config_gate_rejects_the_linux_fixture_with_an_added_host_path() {
        let mut config: Value =
            serde_json::from_str(include_str!("testdata/linux-config-fixture.json")).unwrap();
        config["sandbox-fallback"]["value"] = json!(false);
        config["sandbox-paths"]["value"]["/home/user"] =
            json!({"source": "/home/user", "optional": false});
        let err = verify_effective_config(&config, "linux").unwrap_err();
        assert!(err.contains("/home/user"), "{err}");
    }

    #[test]
    fn nix_base32_alphabet_is_exact() {
        assert_eq!(NIX_BASE32.len(), 32);
        for banned in ['e', 'o', 't', 'u'] {
            assert!(
                !NIX_BASE32.contains(banned),
                "alphabet must not contain {banned}"
            );
        }
        assert_eq!(NIX_BASE32, "0123456789abcdfghijklmnpqrsvwxyz");
    }

    #[test]
    fn busybox_store_hash_uses_the_nix_base32_alphabet() {
        assert!(is_busybox_sh(
            "/nix/store/csshf3ycx2wsxjaqrva1vr8rwgp382jw-busybox-1.37.0/bin/busybox"
        ));
        // 32-hex (wrong alphabet) is NOT a Nix store hash spelling.
        assert!(!is_busybox_sh(&format!(
            "/nix/store/{}-busybox-1.37.0/bin/busybox",
            "0123456789abcdef0123456789abcdef"
        )));
        // Letters outside the base32 alphabet (e, o, u, t) refuse.
        assert!(!is_busybox_sh(
            "/nix/store/eoooooooooooooooooooooooooooooooo-busybox-1.37.0/bin/busybox"
        ));
    }

    #[test]
    fn config_gate_rejects_weak_defaults_and_missing_values() {
        for os in ["linux", "macos"] {
            // sandbox missing entirely (weak default view).
            let mut config = strong_effective_config(os);
            config.as_object_mut().unwrap().remove("sandbox");
            let err = verify_effective_config(&config, os).unwrap_err();
            assert!(err.contains("sandbox is missing"), "{os}: {err}");
            // sandbox false.
            let mut config = strong_effective_config(os);
            config["sandbox"] = json!({"value": false});
            let err = verify_effective_config(&config, os).unwrap_err();
            assert!(err.contains("sandbox is false"), "{os}: {err}");
            // wrong shape: value as a string, not a boolean.
            let mut config = strong_effective_config(os);
            config["sandbox"] = json!({"value": "true"});
            let err = verify_effective_config(&config, os).unwrap_err();
            assert!(err.contains("not a boolean"), "{os}: {err}");
            // sandbox-fallback true.
            let mut config = strong_effective_config(os);
            config["sandbox-fallback"] = json!({"value": true});
            let err = verify_effective_config(&config, os).unwrap_err();
            assert!(err.contains("sandbox-fallback is true"), "{os}: {err}");
            // sandbox-fallback missing.
            let mut config = strong_effective_config(os);
            config.as_object_mut().unwrap().remove("sandbox-fallback");
            assert!(verify_effective_config(&config, os).is_err());
        }
        // Concrete setup advice is always present, never a config dump.
        let err =
            verify_effective_config(&json!({"sandbox": {"value": false}}), "macos").unwrap_err();
        assert!(err.contains("sandbox = true"), "{err}");
        assert!(err.contains("TMPDIR=/nix/var/nix/builds"), "{err}");
    }

    #[test]
    fn config_gate_rejects_wrong_entry_shapes() {
        // sandbox-paths value not an object.
        let mut config = strong_effective_config("macos");
        config["sandbox-paths"] = json!({"value": "/bin/sh"});
        assert!(
            verify_effective_config(&config, "macos")
                .unwrap_err()
                .contains("wrong shape")
        );
        // entry not an object.
        let mut config = strong_effective_config("macos");
        config["sandbox-paths"]["value"]["/bin/sh"] = json!("/bin/sh");
        assert!(verify_effective_config(&config, "macos").is_err());
        // optional true.
        let mut config = strong_effective_config("macos");
        config["sandbox-paths"]["value"]["/bin/sh"]["optional"] = json!(true);
        assert!(
            verify_effective_config(&config, "macos")
                .unwrap_err()
                .contains("optional")
        );
        // optional missing.
        let mut config = strong_effective_config("macos");
        config["sandbox-paths"]["value"]["/bin/sh"]
            .as_object_mut()
            .unwrap()
            .remove("optional");
        assert!(verify_effective_config(&config, "macos").is_err());
        // source missing.
        let mut config = strong_effective_config("macos");
        config["sandbox-paths"]["value"]["/bin/sh"] = json!({"optional": false});
        assert!(verify_effective_config(&config, "macos").is_err());
        // allowed-impure-host-deps wrong shape (string, not array).
        let mut config = strong_effective_config("macos");
        config["allowed-impure-host-deps"] = json!({"value": "/dev"});
        assert!(verify_effective_config(&config, "macos").is_err());
    }

    #[test]
    fn config_gate_rejects_added_host_paths_on_both_oses() {
        // Linux: arbitrary home directory mapping.
        let mut config = strong_effective_config("linux");
        config["sandbox-paths"]["value"]["/home/user"] = json!({
            "source": "/home/user", "optional": false
        });
        let err = verify_effective_config(&config, "linux").unwrap_err();
        assert!(err.contains("/home/user"), "{err}");
        // Linux: /bin/sh mapped to something else.
        let mut config = strong_effective_config("linux");
        config["sandbox-paths"]["value"]["/bin/sh"]["source"] =
            json!("/nix/store/11111111111111111111111111111111-notbusybox/bin/sh");
        assert!(verify_effective_config(&config, "linux").is_err());
        // Mac: added arbitrary path.
        let mut config = strong_effective_config("macos");
        config["sandbox-paths"]["value"]["/Users/me"] = json!({
            "source": "/Users/me", "optional": false
        });
        let err = verify_effective_config(&config, "macos").unwrap_err();
        assert!(err.contains("/Users/me"), "{err}");
        // Mac: allowed path NOT self-mapped.
        let mut config = strong_effective_config("macos");
        config["sandbox-paths"]["value"]["/bin/sh"] = json!({
            "source": "/bin/dash", "optional": false
        });
        assert!(verify_effective_config(&config, "macos").is_err());
        // extra-sandbox-paths is merged and rejected when it adds paths.
        for os in ["linux", "macos"] {
            let mut config = strong_effective_config(os);
            config["extra-sandbox-paths"] = json!({"value": {
                "/etc": {"source": "/etc", "optional": false}
            }});
            assert!(
                verify_effective_config(&config, os).is_err(),
                "{os}: extra-sandbox-paths must be merged into the check"
            );
        }
    }

    #[test]
    fn config_gate_rejects_extra_impure_host_deps() {
        // Linux: any non-empty set is refused.
        let mut config = strong_effective_config("linux");
        config["allowed-impure-host-deps"] = json!({"value": ["/bin/sh"]});
        let err = verify_effective_config(&config, "linux").unwrap_err();
        assert!(err.contains("allowed-impure-host-deps"), "{err}");
        // Mac: outside the allowlist.
        let mut config = strong_effective_config("macos");
        config["allowed-impure-host-deps"] = json!({"value": ["/usr/bin"]});
        let err = verify_effective_config(&config, "macos").unwrap_err();
        assert!(err.contains("/usr/bin"), "{err}");
    }

    // ------------------------------------------------------------------
    // Effective nix config gate: focused fake-command tests
    // ------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn config_gate_passes_with_a_fake_strong_config_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = strong_effective_config(std::env::consts::OS).to_string();
        let nix = fake_nix(dir.path(), &format!("echo '{fixture}'"));
        nix_config_gate_with_limits(&nix, dir.path(), Duration::from_secs(30))
            .unwrap_or_else(|e| panic!("gate must pass with a strong fixture: {e:?}"));
    }

    #[cfg(unix)]
    #[test]
    fn config_gate_fails_closed_on_a_weak_fake_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nix = fake_nix(
            dir.path(),
            "echo '{\"sandbox\":{\"value\":false},\"sandbox-fallback\":{\"value\":true}}'",
        );
        let err = nix_config_gate_with_limits(&nix, dir.path(), Duration::from_secs(30))
            .expect_err("weak config");
        let GateError::Refused(detail) = &err else {
            panic!("an observed weak config is the typed refusal: {err:?}");
        };
        assert!(detail.contains("sandbox is false"), "{detail}");
    }

    #[cfg(unix)]
    #[test]
    fn config_gate_aborts_on_a_config_log_flood() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nix = fake_nix(dir.path(), "yes 0123456789abcdef | head -c 4194304; exit 0");
        let err = nix_config_gate_with_limits(&nix, dir.path(), Duration::from_secs(60))
            .expect_err("flood");
        let GateError::Unverifiable(detail) = &err else {
            panic!("the settings were never observed: {err:?}");
        };
        assert!(detail.contains("log cap"), "{detail}");
    }

    #[cfg(unix)]
    #[test]
    fn config_gate_times_out_and_kills_a_hung_query_promptly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nix = fake_nix(dir.path(), "sleep 300 &\nsleep 300");
        let started = std::time::Instant::now();
        let err = nix_config_gate_with_limits(&nix, dir.path(), Duration::from_secs(2))
            .expect_err("hang");
        let GateError::Unverifiable(detail) = &err else {
            panic!("a hung query observed nothing: {err:?}");
        };
        assert!(detail.contains("wall limit"), "{detail}");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the group kill + bounded joins must return promptly"
        );
    }

    /// Environment sentinel: a CHILD TEST PROCESS carries ambient
    /// `NIX_CONFIG`, proxy, and fake-credential sentinels and runs the
    /// gate with a fake nix that dumps its own environment. The nix
    /// child must see exactly the constructor whitelist plus shell
    /// bookkeeping: no ambient variable may leak through `env_clear`.
    #[cfg(unix)]
    #[test]
    fn config_gate_child_environment_is_exactly_the_whitelist() {
        if std::env::var_os("PKG_TAP_GATE_ENV_CHILD_MODE").is_some() {
            let dir =
                PathBuf::from(std::env::var("PKG_TAP_GATE_ENV_DIR").expect("gate env dir env"));
            let fixture = strong_effective_config(std::env::consts::OS).to_string();
            let dump = dir.join("child-env");
            let nix = fake_nix(
                &dir,
                &format!(
                    "/usr/bin/env > {d}\necho '{fixture}'",
                    d = dump.display(),
                    fixture = fixture.replace('\'', "'\\''")
                ),
            );
            nix_config_gate_with_limits(&nix, &dir, Duration::from_secs(30))
                .expect("gate passes with a strong fixture");
            let dumped = std::fs::read_to_string(&dump).expect("child env dump readable");
            let mut seen: BTreeMap<String, String> = BTreeMap::new();
            for line in dumped.lines() {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                seen.insert(key.to_string(), value.to_string());
            }
            let gate = dir.join("gate");
            let home = gate.join("home");
            let mut expected: BTreeMap<String, String> = [
                ("HOME", home.display().to_string()),
                (
                    "XDG_CONFIG_HOME",
                    home.join(".config").display().to_string(),
                ),
                (
                    "NIX_USER_CONF_FILES",
                    home.join("empty-nix.conf").display().to_string(),
                ),
                ("TMPDIR", gate.join("tmp").display().to_string()),
                ("LC_ALL", "C".to_string()),
                (
                    "PATH",
                    format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", dir.display()),
                ),
                // Added by /bin/sh itself (cwd bookkeeping), never by
                // the constructor; its presence is not a leak.
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
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("config_gate_child_environment_is_exactly_the_whitelist")
            .env_clear()
            .env("PKG_TAP_GATE_ENV_CHILD_MODE", "1")
            .env("PKG_TAP_GATE_ENV_DIR", dir.path())
            // Ambient hostiles this child test process carries: a
            // NIX_CONFIG override, proxies, and a FAKE credential.
            // None may reach the nix child. (Sentinel values only;
            // nothing real is copied or printed.)
            .env("NIX_CONFIG", "sentinel-nix-config")
            .env("http_proxy", "http://127.0.0.1:9")
            .env("https_proxy", "http://127.0.0.1:9")
            .env("no_proxy", "sentinel-no-proxy")
            .env("PKG_TAP_FAKE_CREDENTIAL", "sentinel-fake-token")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("child test process spawns");
        let status = child.wait().expect("child test process reaped");
        assert!(
            status.success(),
            "the child test process must pass the whitelist checks"
        );
    }
}
