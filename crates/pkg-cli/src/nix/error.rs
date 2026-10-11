//! The native adapter's failure modes.

use std::fmt;

/// A native operation failure category with preserved diagnostics.
#[derive(Debug)]
pub enum NixError {
    /// The configured `nix` value cannot be used. pkg never falls back to
    /// another runtime (discovery design D2).
    ConfiguredUnavailable {
        /// The configured executable name or path.
        requested: String,
        /// Why the configured value is unusable.
        detail: String,
    },
    /// The configured runtime failed its version probe.
    ConfiguredProbe {
        /// The configured executable name or path.
        requested: String,
        /// The native probe failure.
        source: Box<NixError>,
    },
    /// Default discovery found no usable `nix` in `PATH` or the stable
    /// profile paths (discovery design D1, D3).
    NotFound {
        /// How many `PATH` directories were searched.
        path_entries: usize,
        /// The stable Nix profile paths that were searched.
        standard: Vec<String>,
    },
    /// A native command could not be started.
    Spawn(std::io::Error),
    /// Reading native state failed.
    Io {
        /// The state being read.
        what: &'static str,
        /// The filesystem cause.
        source: std::io::Error,
    },
    /// A discovery evaluator was stopped before exceeding its resource budget.
    ResourceLimit(String),
    /// A native command failed. Upstream diagnostics are preserved.
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
    Interrupted {
        /// The argument vector pkg ran.
        args: Vec<String>,
        /// The terminating signal number.
        signal: i32,
    },
    /// Native output could not be decoded into a required shape.
    Decode {
        /// What pkg was decoding.
        what: &'static str,
        /// Why decoding failed.
        detail: String,
    },
    /// Native JSON did not match the required type.
    Json {
        /// What pkg was decoding.
        what: &'static str,
        /// The JSON decoding or serialization cause.
        source: serde_json::Error,
    },
}

impl NixError {
    /// Decode a native profile conflict without losing the raw diagnostics.
    #[must_use]
    pub fn profile_conflict(&self) -> Option<super::ProfileConflict> {
        match self {
            Self::Failed { stderr, .. } => super::conflict::decode(stderr),
            _ => None,
        }
    }
}

impl fmt::Display for NixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfiguredUnavailable { requested, detail } => {
                configured_diagnostic(f, requested, detail)
            }
            Self::ConfiguredProbe { requested, source } => {
                configured_diagnostic(f, requested, &format!("the version probe failed: {source}"))
            }
            Self::NotFound {
                path_entries,
                standard,
            } => {
                writeln!(f, "no usable Nix executable was found")?;
                writeln!(
                    f,
                    "pkg searched {path_entries} PATH directories and these standard Nix locations:"
                )?;
                for location in standard {
                    writeln!(f, "  {location}")?;
                }
                write!(
                    f,
                    "Next steps:\n\
                     - If Nix is installed, add its bin directory to PATH, for example:\n\
                       export PATH=\"/nix/var/nix/profiles/default/bin:$PATH\"\n\
                     - Or set `runtime.nix` in the pkg config file \
                     (default ~/.config/pkg/config.toml) to the nix executable.\n\
                     - If Nix is not installed, install Determinate Nix from\n\
                       https://docs.determinate.systems/determinate-nix/\n\
                     pkg does not install, update, or repair the Nix runtime."
                )
            }
            Self::Spawn(detail) => write!(f, "could not start the Nix command: {detail}"),
            Self::Io { what, source } => write!(f, "could not read {what}: {source}"),
            Self::ResourceLimit(detail) => write!(f, "discovery evaluation stopped: {detail}"),
            Self::Failed {
                args,
                status,
                stderr,
            } => {
                write!(
                    f,
                    "native command `nix {}` failed ({status})",
                    args.join(" ")
                )?;
                if !stderr.trim().is_empty() {
                    write!(f, "\n{}", stderr.trim())?;
                }
                Ok(())
            }
            Self::Interrupted { args, signal } => write!(
                f,
                "native command `nix {}` was terminated by signal {signal}; \
                 the operation may have partially applied",
                args.join(" ")
            ),
            Self::Decode { what, detail } => {
                write!(f, "could not decode required {what} from Nix: {detail}")
            }
            Self::Json { what, source } => {
                write!(f, "could not decode required {what} from Nix: {source}")
            }
        }
    }
}

impl std::error::Error for NixError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(source) => Some(source),
            Self::ConfiguredProbe { source, .. } => Some(source.as_ref()),
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn configured_diagnostic(f: &mut fmt::Formatter<'_>, requested: &str, detail: &str) -> fmt::Result {
    write!(
        f,
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
        assert!(text.contains("runtime.nix"), "{text}");
        assert!(
            text.contains("https://docs.determinate.systems/determinate-nix/"),
            "{text}"
        );
        assert!(text.contains("does not install"), "{text}");
    }
}
