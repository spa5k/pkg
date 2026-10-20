//! The native adapter's failure modes.

/// A native operation failure category with preserved diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum NixError {
    /// The configured `nix` value cannot be used. pkg never falls back to
    /// another runtime (discovery design D2).
    #[error("{}", configured_diagnostic(requested, detail))]
    ConfiguredUnavailable {
        /// The configured executable name or path.
        requested: String,
        /// Why the configured value is unusable.
        detail: String,
    },
    /// The configured runtime failed its version probe.
    #[error("{}", configured_diagnostic(requested, &format!("the version probe failed: {source}")))]
    ConfiguredProbe {
        /// The configured executable name or path.
        requested: String,
        /// The original native probe failure, retained in its owned box.
        source: Box<Self>,
    },
    /// Default discovery found no usable `nix` in `PATH` or the stable
    /// profile paths (discovery design D1, D3).
    #[error("{}", not_found_diagnostic(*path_entries, standard))]
    NotFound {
        /// How many `PATH` directories were searched.
        path_entries: usize,
        /// The stable Nix profile paths that were searched.
        standard: Vec<String>,
    },
    /// A native command could not be started.
    #[error("could not start the Nix command: {0}")]
    Spawn(#[source] std::io::Error),
    /// A child supervision failure, with cancellation and its I/O cause.
    #[error("could not supervise the Nix command: {0}")]
    Process(#[from] pkg_process::RunError),
    /// Reading native state failed.
    #[error("could not read {what}: {source}")]
    Io {
        /// The state being read.
        what: &'static str,
        /// The filesystem cause.
        source: std::io::Error,
    },
    /// A discovery evaluator was stopped before exceeding its resource budget.
    #[error("discovery evaluation stopped: {0}")]
    ResourceLimit(String),
    /// A native command failed. Upstream diagnostics are preserved.
    #[error("native command `nix {}` failed ({status}){}", command_line(args), if stderr.trim().is_empty() { String::new() } else { format!("\n{}", stderr.trim()) })]
    Failed {
        /// The argument vector pkg ran.
        args: Vec<String>,
        /// The exit status message.
        status: String,
        /// Captured stderr, when available.
        stderr: String,
    },
    /// A native command was terminated by a signal after it started.
    ///
    /// The child was reaped by pkg; the profile may be in an intermediate
    /// state, so callers must re-read native state before reporting.
    #[error(
        "native command `nix {}` was terminated by signal {signal}; the operation may have partially applied",
        command_line(args)
    )]
    Interrupted {
        /// The argument vector pkg ran.
        args: Vec<String>,
        /// The terminating signal number.
        signal: i32,
    },
    /// Native output could not be decoded into a required shape.
    #[error("could not decode required {what} from Nix: {detail}")]
    Decode {
        /// What pkg was decoding.
        what: &'static str,
        /// Why decoding failed.
        detail: String,
    },
    /// Native JSON did not match the required type.
    #[error("could not decode required {what} from Nix: {source}")]
    Json {
        /// What pkg was decoding.
        what: &'static str,
        /// The JSON decoding or serialization cause.
        source: serde_json::Error,
    },
}

impl NixError {
    /// The cancellation signal, including a failed supervision or probe.
    #[must_use]
    pub fn signal(&self) -> Option<i32> {
        match self {
            Self::Interrupted { signal, .. } => Some(*signal),
            Self::Process(error) => error.cancelled,
            Self::ConfiguredProbe { source, .. } => source.signal(),
            _ => None,
        }
    }

    /// The innermost evaluation message of a failed native command.
    ///
    /// Nix prints a trace of nested `error:` frames. The last nonempty
    /// frame names the actual cause, for example a removed Nixpkgs alias.
    #[must_use]
    pub fn evaluation_cause(&self) -> Option<&str> {
        let Self::Failed { stderr, .. } = self else {
            return None;
        };
        stderr
            .lines()
            .map(str::trim)
            .filter_map(|line| line.strip_prefix("error:"))
            .map(str::trim)
            .rfind(|cause| !cause.is_empty())
    }

    /// Decode a native profile conflict without losing the raw diagnostics.
    #[must_use]
    pub fn profile_conflict(&self) -> Option<super::ProfileConflict> {
        match self {
            Self::Failed { stderr, .. } => super::conflict::decode(stderr),
            _ => None,
        }
    }
}

/// One readable line for a native argument vector.
///
/// Generated expressions and other long arguments become `…`, so an error
/// names the command without printing a Nix program. `--verbose` traces
/// show the full arguments.
fn command_line(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.contains('\n') || arg.chars().count() > 120 {
                "…"
            } else {
                arg.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn not_found_diagnostic(path_entries: usize, standard: &[String]) -> String {
    format!(
        "pkg needs Nix, but no working Nix was found\n\
         pkg searched {path_entries} PATH directories and these standard Nix locations:\n  {}\n\
         If Nix is installed, open a new terminal window. If Nix is still not found,\n\
         add its bin directory to PATH.\n\
         If Nix is not installed, install Determinate Nix:\n  \
         curl -fsSL https://install.determinate.systems/nix | sh -s -- install",
        standard.join("\n  "),
    )
}

fn configured_diagnostic(requested: &str, detail: &str) -> String {
    format!(
        "the configured Nix executable ({requested}) is unusable: {detail}\n\
         pkg uses only this configured value and does not fall back to another runtime.\n\
         Fix the path, or remove `runtime.nix` from the pkg config file\n\
         (default ~/.config/pkg/config.toml), then run this command again."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The not-found diagnostic must name the searched locations and give a
    /// next action for a machine where Nix exists but is unreachable, not
    /// only reinstall advice. The configured-value form is covered
    /// end-to-end by the CLI subprocess test.
    #[test]
    fn not_found_names_locations_and_useful_next_actions() {
        let error = NixError::NotFound {
            path_entries: 7,
            standard: vec![
                String::from("/nix/var/nix/profiles/default/bin/nix"),
                String::from("/home/u/.local/state/nix/profile/bin/nix"),
                String::from("/home/u/.nix-profile/bin/nix"),
            ],
        };
        let text = error.to_string();
        assert!(text.contains("7 PATH directories"), "{text}");
        assert!(
            text.contains("/nix/var/nix/profiles/default/bin/nix"),
            "{text}"
        );
        assert!(
            text.contains("/home/u/.local/state/nix/profile/bin/nix"),
            "{text}"
        );
        assert!(text.contains("/home/u/.nix-profile/bin/nix"), "{text}");
        assert!(text.contains("add its bin directory to PATH"), "{text}");
        assert!(
            text.contains("https://install.determinate.systems/nix"),
            "{text}"
        );
    }

    #[test]
    fn failed_evaluations_name_the_command_and_the_innermost_cause() {
        let error = NixError::Failed {
            args: vec![
                String::from("eval"),
                String::from("--json"),
                String::from("--expr"),
                String::from("let\n  flake = 1;\nin flake"),
            ],
            status: String::from("exit code 1"),
            stderr: String::from(
                "error (ignored): opening file \"/etc/nix/sentry-endpoint\": Permission denied\n\
                 error:\n       … while evaluating list element at index 1\n\n       \
                 error: `python` previously pointed to Python 2; use `python3` instead\n",
            ),
        };
        let text = error.to_string();
        assert!(
            text.starts_with("native command `nix eval --json --expr …` failed"),
            "{text}"
        );
        assert!(!text.contains("flake = 1"), "{text}");
        assert_eq!(
            error.evaluation_cause(),
            Some("`python` previously pointed to Python 2; use `python3` instead")
        );
    }
}
