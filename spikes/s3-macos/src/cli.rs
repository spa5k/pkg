//! Spike S3 (PR-7) — CLI slice: a deliberately CLOSED argument grammar for the
//! `s3-probe` binary.
//!
//! # Grammar
//! ```text
//! s3-probe --help|-h
//! s3-probe fake     [--out-dir PATH]
//! s3-probe detect   [--out-dir PATH] [--nix-bin ABSOLUTE_PATH]
//! s3-probe preflight --nix-bin ABSOLUTE_PATH [--out-dir PATH]
//! ```
//! No-args prints help and exits 0. The parser REJECTS: duplicate flags,
//! `--flag=value` equals forms, abbreviations, positional tokens, recognized
//! flags before the mode keyword, `--nix-bin` in `fake` mode, a relative or
//! empty `--nix-bin`, a missing `--nix-bin` in `preflight` mode, and every
//! signing credential-shaped option (`--identity`, `--keychain`, `--password`,
//! `--team-id`, …).
//!
//! # Bounded Display
//! [`CliError::Display`] is deterministic and bounded. It NEVER echoes a
//! credential value, a raw positional token, or an unbounded path. Non-UTF-8
//! paths are preserved: `out-dir`/`nix-bin` are carried as [`PathBuf`] (built
//! directly from the [`OsString`] value), and only the ASCII flag NAMES are
//! decoded for comparison.
//!
//! `#![forbid(unsafe_code)]`.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

/// Maximum characters of a flag name echoed in a bounded error message.
const FLAG_SNIPPET_MAX: usize = 64;

/// The fixed usage banner printed for `--help`/`-h` and bare invocation.
pub const USAGE: &str = "\
s3-probe — macOS signing/notarization capability probe (S3 spike)

USAGE:
    s3-probe --help|-h
    s3-probe fake     [--out-dir PATH]
    s3-probe detect   [--out-dir PATH] [--nix-bin ABSOLUTE_PATH]
    s3-probe preflight --nix-bin ABSOLUTE_PATH [--out-dir PATH]

NOTES:
    --out-dir PATH          directory for report.json + summary.md (default: .)
    --nix-bin ABSOLUTE_PATH absolute path to a Nix binary:
                              * detect:    optional; existence-only check
                                           (never executed, never searched on PATH)
                              * preflight: REQUIRED; the supplied absolute Nix
                                           binary is executed (never itself
                                           pinned, and never shell/PATH-
                                           resolved); its exact Nix 2.34.8
                                           version is verified at runtime. It
                                           runs build-free probes only: flake
                                           prefetch fetches the pinned GitHub
                                           flake/source, while store-info/path-
                                           info availability queries target
                                           cache.nixos.org. NOT read-only: may
                                           write normal Nix-managed fetch/
                                           evaluation state. Still no package
                                           build/profile activation, signing, or
                                           shell/PATH lookup.
    This binary never accepts signing credentials (identity/keychain/password/
    team-id/...). It runs NO build/sign/notarization execution.
";

/// The resolved CLI action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Print [`USAGE`] and exit 0.
    Help,
    /// Run a mode.
    Run(RunArgs),
}

/// Resolved run arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    /// The selected mode.
    pub mode: RunMode,
    /// Output directory for artifacts (default `.`).
    pub out_dir: PathBuf,
}

/// The selected run mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMode {
    /// Pure-harness Fake lane.
    Fake,
    /// Read-only host Detect lane, with an optional absolute Nix binary path.
    Detect {
        /// Optional absolute path to a Nix binary (existence check only).
        nix_bin: Option<PathBuf>,
    },
    /// Preflight cache-coverage lane, with a REQUIRED absolute Nix binary path
    /// (caller-supplied, never itself pinned) whose exact Nix 2.34.8 version is
    /// verified at runtime. It runs build-free probes only: flake prefetch
    /// fetches the pinned GitHub flake/source, while `nix store info`/`nix
    /// path-info` availability queries target cache.nixos.org (no package
    /// build/profile activation, signing, or shell/PATH lookup). Build-free does
    /// NOT mean read-only or mutation-free: prefetch may add the pinned source to
    /// the Nix store/fetch cache and evaluation may populate ordinary Nix-managed
    /// state.
    Preflight {
        /// Required absolute path to a Nix binary (executed build-free; may
        /// populate the Nix store/fetch cache and ordinary Nix-managed state).
        nix_bin: PathBuf,
    },
}

/// A bounded, credential-safe CLI parse failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    /// A recognized flag (or any flag) appeared before the mode keyword.
    FlagBeforeMode,
    /// The first token was not a recognized mode and not `--help`/`-h`.
    UnrecognizedMode,
    /// A flag token was not recognized for the selected mode (also catches
    /// abbreviations). Carries a bounded snippet of the flag name only.
    UnknownFlag {
        /// Bounded snippet of the offending flag name (never a value).
        flag: String,
    },
    /// A `--flag=value` equals form was used (only space-separated values apply).
    EqualsForm,
    /// A flag was given more than once.
    DuplicateFlag,
    /// A bare positional token appeared where no positional is accepted.
    PositionalToken,
    /// A flag expected a value but the next token was absent or another flag.
    MissingValue,
    /// A signing credential-shaped option was offered (denied outright).
    SigningOption,
    /// `--nix-bin` was given a relative or empty path.
    NixBinNotAbsolute,
    /// `--nix-bin` was offered in `fake` mode (where it is meaningless).
    NixBinInFakeMode,
    /// `--nix-bin` was missing in `preflight` mode (where it is required).
    NixBinRequired,
    /// `--help`/`-h` was followed by one or more trailing tokens. Help MUST be
    /// standalone; the trailing token is NEVER inspected or echoed (not even a
    /// credential-shaped one).
    HelpNotStandalone,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::FlagBeforeMode => {
                f.write_str("cli: a flag appeared before the mode (expected fake|detect|preflight)")
            }
            CliError::UnrecognizedMode => {
                f.write_str("cli: unrecognized mode (expected fake|detect|preflight)")
            }
            CliError::UnknownFlag { flag } => {
                write!(f, "cli: unrecognized option {flag:?}")
            }
            CliError::EqualsForm => {
                f.write_str("cli: --flag=value form not accepted (use --flag value)")
            }
            CliError::DuplicateFlag => f.write_str("cli: a flag was given more than once"),
            CliError::PositionalToken => {
                f.write_str("cli: unexpected positional token (no positionals accepted)")
            }
            CliError::MissingValue => f.write_str("cli: a flag expected a value but found none"),
            CliError::SigningOption => f.write_str("cli: signing credential option not accepted"),
            CliError::NixBinNotAbsolute => f.write_str("cli: --nix-bin must be an absolute path"),
            CliError::NixBinInFakeMode => f.write_str("cli: --nix-bin is not valid in fake mode"),
            CliError::NixBinRequired => {
                f.write_str("cli: --nix-bin is required (an absolute path to a Nix binary)")
            }
            CliError::HelpNotStandalone => {
                f.write_str("cli: --help/-h must be standalone (no trailing arguments)")
            }
        }
    }
}

impl std::error::Error for CliError {}

/// Parse `argv[1..]` (as owned [`OsString`]s) into an [`Action`].
pub fn parse(args: Vec<OsString>) -> Result<Action, CliError> {
    // No-args ⇒ Help (friendliest; still a closed grammar).
    let mut iter = args.into_iter();
    let first = match iter.next() {
        None => return Ok(Action::Help),
        Some(t) => t,
    };

    match first.as_os_str().to_str() {
        Some("--help") | Some("-h") => {
            // --help/-h MUST be standalone: any trailing token is a closed,
            // bounded error that never echoes the token (not even a credential).
            if iter.next().is_some() {
                return Err(CliError::HelpNotStandalone);
            }
            Ok(Action::Help)
        }
        Some("fake") => Ok(Action::Run(parse_mode_fake(iter.collect())?)),
        Some("detect") => Ok(Action::Run(parse_mode_detect(iter.collect())?)),
        Some("preflight") => Ok(Action::Run(parse_mode_preflight(iter.collect())?)),
        // A leading dash before the mode keyword is a recognized/unknown flag
        // before the mode; anything else is an unrecognized mode.
        Some(s) if s.starts_with('-') => Err(CliError::FlagBeforeMode),
        _ => Err(CliError::UnrecognizedMode),
    }
}

/// Parse the `fake` mode's flag tail: only `--out-dir PATH`.
fn parse_mode_fake(rest: Vec<OsString>) -> Result<RunArgs, CliError> {
    let mut out_dir: Option<PathBuf> = None;
    let mut tokens = rest.into_iter().peekable();
    while let Some(tok) = tokens.next() {
        reject_signing_raw(&tok)?;
        let name = flag_name(&tok)?;
        match name.as_str() {
            "--out-dir" => {
                if out_dir.is_some() {
                    return Err(CliError::DuplicateFlag);
                }
                out_dir = Some(take_value(&mut tokens)?);
            }
            // --nix-bin is structurally a known flag but invalid in fake mode.
            "--nix-bin" => return Err(CliError::NixBinInFakeMode),
            _ => {
                return Err(CliError::UnknownFlag {
                    flag: bound_flag(&name),
                });
            }
        }
    }
    Ok(RunArgs {
        mode: RunMode::Fake,
        out_dir: out_dir.unwrap_or_else(|| PathBuf::from(".")),
    })
}

/// Parse the `preflight` mode's flag tail: `--nix-bin ABSOLUTE_PATH` (REQUIRED,
/// exactly once, absolute and nonempty) and `--out-dir PATH` (defaults to `.`).
fn parse_mode_preflight(rest: Vec<OsString>) -> Result<RunArgs, CliError> {
    let mut out_dir: Option<PathBuf> = None;
    let mut nix_bin: Option<PathBuf> = None;
    let mut tokens = rest.into_iter().peekable();
    while let Some(tok) = tokens.next() {
        reject_signing_raw(&tok)?;
        let name = flag_name(&tok)?;
        match name.as_str() {
            "--out-dir" => {
                if out_dir.is_some() {
                    return Err(CliError::DuplicateFlag);
                }
                out_dir = Some(take_value(&mut tokens)?);
            }
            "--nix-bin" => {
                if nix_bin.is_some() {
                    return Err(CliError::DuplicateFlag);
                }
                let value = take_value(&mut tokens)?;
                if !is_absolute_nonempty(&value) {
                    return Err(CliError::NixBinNotAbsolute);
                }
                nix_bin = Some(value);
            }
            _ => {
                return Err(CliError::UnknownFlag {
                    flag: bound_flag(&name),
                });
            }
        }
    }
    let nix_bin = nix_bin.ok_or(CliError::NixBinRequired)?;
    Ok(RunArgs {
        mode: RunMode::Preflight { nix_bin },
        out_dir: out_dir.unwrap_or_else(|| PathBuf::from(".")),
    })
}

/// Parse the `detect` mode's flag tail: `--out-dir PATH` and
/// `--nix-bin ABSOLUTE_PATH`.
fn parse_mode_detect(rest: Vec<OsString>) -> Result<RunArgs, CliError> {
    let mut out_dir: Option<PathBuf> = None;
    let mut nix_bin: Option<PathBuf> = None;
    let mut tokens = rest.into_iter().peekable();
    while let Some(tok) = tokens.next() {
        reject_signing_raw(&tok)?;
        let name = flag_name(&tok)?;
        match name.as_str() {
            "--out-dir" => {
                if out_dir.is_some() {
                    return Err(CliError::DuplicateFlag);
                }
                out_dir = Some(take_value(&mut tokens)?);
            }
            "--nix-bin" => {
                if nix_bin.is_some() {
                    return Err(CliError::DuplicateFlag);
                }
                let value = take_value(&mut tokens)?;
                if !is_absolute_nonempty(&value) {
                    return Err(CliError::NixBinNotAbsolute);
                }
                nix_bin = Some(value);
            }
            _ => {
                return Err(CliError::UnknownFlag {
                    flag: bound_flag(&name),
                });
            }
        }
    }
    Ok(RunArgs {
        mode: RunMode::Detect { nix_bin },
        out_dir: out_dir.unwrap_or_else(|| PathBuf::from(".")),
    })
}

/// Extract the flag NAME from a token: it must start with `-`, must NOT contain
/// `=` (we reject the equals form), and must be a valid `--name` shape. Returns
/// the lowercased ASCII name (e.g. `--out-dir`). A token that does not start
/// with `-` is a stray positional.
fn flag_name(token: &OsString) -> Result<String, CliError> {
    let s = token.to_str().ok_or(CliError::PositionalToken)?;
    if !s.starts_with('-') {
        // A non-flag token where a flag is expected: stray positional.
        return Err(CliError::PositionalToken);
    }
    if s.contains('=') {
        return Err(CliError::EqualsForm);
    }
    // Normalize to lowercase ASCII for matching. Flag names are caller-chosen;
    // normalizing keeps the closed set match case-insensitive without echoing
    // any value.
    Ok(s.to_ascii_lowercase())
}

/// Reject a signing credential-shaped flag from its RAW token, BEFORE the
/// equals-form check, so `--identity=x` is denied as [`CliError::SigningOption`]
/// (never as [`CliError::EqualsForm`]). Only the NAME part before any `=` is
/// inspected; the offered value is never touched.
fn reject_signing_raw(token: &OsString) -> Result<(), CliError> {
    let s = match token.to_str() {
        Some(s) => s,
        // A non-UTF-8 token is left for `flag_name` to reject as a positional.
        None => return Ok(()),
    };
    if !s.starts_with('-') {
        // A positional token is left for `flag_name` to reject.
        return Ok(());
    }
    // Inspect only the name part before any `=`, so `--identity=x` is caught by
    // its bare name `identity`, never by the value `x`.
    let name_part = s.split('=').next().unwrap_or(s);
    let bare = name_part.trim_start_matches('-');
    // Normalize `-`/`_` so both kebab and snake forms are caught, and lowercase
    // ASCII so `--Identity`/`--KEYCHAIN` are denied too.
    let norm: String = bare
        .chars()
        .map(|c| {
            if c == '_' {
                '-'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    if SIGNING_OPTIONS.contains(&norm.as_str()) {
        return Err(CliError::SigningOption);
    }
    Ok(())
}

/// Consume the next token as a flag VALUE. It must exist and must not itself
/// look like a flag (start with `-`); values are paths, which never start with
/// `-`. Carried as a [`PathBuf`] to preserve non-UTF-8 bytes.
fn take_value(
    tokens: &mut std::iter::Peekable<std::vec::IntoIter<OsString>>,
) -> Result<PathBuf, CliError> {
    match tokens.next() {
        None => Err(CliError::MissingValue),
        Some(v) => {
            if v.as_os_str()
                .to_str()
                .is_some_and(|s| s.starts_with('-') && s.len() > 1)
            {
                return Err(CliError::MissingValue);
            }
            Ok(PathBuf::from(v))
        }
    }
}

/// `true` iff `path` is absolute and non-empty.
fn is_absolute_nonempty(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.is_absolute()
}

/// Bound a flag-name snippet for an error message.
fn bound_flag(name: &str) -> String {
    if name.len() <= FLAG_SNIPPET_MAX {
        name.to_string()
    } else {
        let mut end = FLAG_SNIPPET_MAX;
        while end > 0 && !name.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &name[..end])
    }
}

/// The closed denylist of signing credential-shaped option names (kebab-case,
/// normalized from snake-case too). Matching any of these is an outright
/// [`CliError::SigningOption`], before any value is inspected.
const SIGNING_OPTIONS: &[&str] = &[
    "identity",
    "identities",
    "keychain",
    "keychains",
    "password",
    "passphrase",
    "pass",
    "api-key",
    "apikey",
    "api-key-id",
    "team-id",
    "teamid",
    "apple-id",
    "appleid",
    "notary-profile",
    "notarization-profile",
    "notary",
    "sign",
    "signing",
    "signed",
    "certificate",
    "cert",
    "secret",
    "token",
    "credential",
    "credentials",
    "p12",
    "keystore",
    "provisioning-profile",
    "developer-id",
    "developerid",
    "entitlements",
    "entitlement",
    "app-password",
    "app-specific-password",
    "key",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_args_is_help() {
        assert_eq!(parse(os(&[])), Ok(Action::Help));
    }

    #[test]
    fn help_and_short_help() {
        assert_eq!(parse(os(&["--help"])), Ok(Action::Help));
        assert_eq!(parse(os(&["-h"])), Ok(Action::Help));
    }

    // ---- --help/-h must be standalone ----------------------------

    #[test]
    fn help_must_be_standalone() {
        // --help/-h alone is Help.
        assert_eq!(parse(os(&["--help"])), Ok(Action::Help));
        assert_eq!(parse(os(&["-h"])), Ok(Action::Help));
        // ANY trailing token is a closed, bounded error (exit 64). The trailing
        // token is never echoed.
        assert_eq!(
            parse(os(&["--help", "extra"])),
            Err(CliError::HelpNotStandalone)
        );
        assert_eq!(
            parse(os(&["-h", "extra"])),
            Err(CliError::HelpNotStandalone)
        );
        // Even a second --help is a trailing token.
        assert_eq!(
            parse(os(&["--help", "--help"])),
            Err(CliError::HelpNotStandalone)
        );
        // A flag-shaped trailing token is still HelpNotStandalone (the token is
        // not inspected as a flag here — help is closed).
        assert_eq!(
            parse(os(&["--help", "--out-dir"])),
            Err(CliError::HelpNotStandalone)
        );
    }

    #[test]
    fn help_with_signing_option_never_echoes_credential() {
        // --help followed by a credential-shaped option is HelpNotStandalone
        // (NOT SigningOption, NOT Help): the trailing token is never inspected
        // or echoed. The offered value must never reach any bounded message.
        let err = parse(os(&["--help", "--keychain-password", "SECRET"])).unwrap_err();
        assert_eq!(err, CliError::HelpNotStandalone);
        let s = err.to_string();
        assert!(!s.contains("SECRET"));
        assert!(!s.contains("keychain"));
        assert!(!s.contains("password"));
        assert!(s.starts_with("cli: --help/-h must be standalone"));
        assert!(s.len() < 128);
        // A non-UTF-8 trailing token is still HelpNotStandalone (it is never
        // decoded).
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let bad = OsString::from_vec(vec![0xff, 0xfe]);
            let args = vec![OsString::from("--help"), bad];
            assert_eq!(parse(args), Err(CliError::HelpNotStandalone));
        }
    }

    #[test]
    fn fake_default_out_dir() {
        match parse(os(&["fake"])).unwrap() {
            Action::Run(RunArgs {
                mode: RunMode::Fake,
                out_dir,
            }) => assert_eq!(out_dir, PathBuf::from(".")),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn fake_with_out_dir() {
        match parse(os(&["fake", "--out-dir", "/tmp/r"])).unwrap() {
            Action::Run(RunArgs {
                mode: RunMode::Fake,
                out_dir,
            }) => assert_eq!(out_dir, PathBuf::from("/tmp/r")),
            _ => panic!("unexpected"),
        }
    }

    #[test]
    fn detect_with_out_dir_and_nix_bin() {
        match parse(os(&[
            "detect",
            "--out-dir",
            "/tmp/r",
            "--nix-bin",
            "/nix/var/nix/bin/nix",
        ]))
        .unwrap()
        {
            Action::Run(RunArgs {
                mode: RunMode::Detect { nix_bin },
                out_dir,
            }) => {
                assert_eq!(out_dir, PathBuf::from("/tmp/r"));
                assert_eq!(nix_bin, Some(PathBuf::from("/nix/var/nix/bin/nix")));
            }
            _ => panic!("unexpected"),
        }
    }

    #[test]
    fn detect_default_out_dir_no_nix_bin() {
        match parse(os(&["detect"])).unwrap() {
            Action::Run(RunArgs {
                mode: RunMode::Detect { nix_bin: None },
                out_dir,
            }) => assert_eq!(out_dir, PathBuf::from(".")),
            _ => panic!("unexpected"),
        }
    }

    // ---- preflight mode ---------------------------------------------------

    #[test]
    fn preflight_requires_nix_bin() {
        // Bare preflight: --nix-bin is required.
        assert_eq!(parse(os(&["preflight"])), Err(CliError::NixBinRequired));
        // --out-dir alone does NOT satisfy the requirement.
        assert_eq!(
            parse(os(&["preflight", "--out-dir", "/tmp/r"])),
            Err(CliError::NixBinRequired)
        );
    }

    #[test]
    fn preflight_with_nix_bin_default_out_dir() {
        match parse(os(&["preflight", "--nix-bin", "/nix/var/nix/bin/nix"])).unwrap() {
            Action::Run(RunArgs {
                mode: RunMode::Preflight { nix_bin },
                out_dir,
            }) => {
                assert_eq!(nix_bin, PathBuf::from("/nix/var/nix/bin/nix"));
                assert_eq!(out_dir, PathBuf::from("."));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn preflight_with_nix_bin_and_out_dir() {
        match parse(os(&[
            "preflight",
            "--nix-bin",
            "/nix/var/nix/bin/nix",
            "--out-dir",
            "/tmp/r",
        ]))
        .unwrap()
        {
            Action::Run(RunArgs {
                mode: RunMode::Preflight { nix_bin },
                out_dir,
            }) => {
                assert_eq!(nix_bin, PathBuf::from("/nix/var/nix/bin/nix"));
                assert_eq!(out_dir, PathBuf::from("/tmp/r"));
            }
            _ => panic!("unexpected"),
        }
    }

    #[test]
    fn preflight_flags_in_either_order() {
        // --out-dir before --nix-bin is fine.
        match parse(os(&[
            "preflight",
            "--out-dir",
            "/tmp/r",
            "--nix-bin",
            "/nix/var/nix/bin/nix",
        ]))
        .unwrap()
        {
            Action::Run(RunArgs {
                mode: RunMode::Preflight { nix_bin },
                out_dir,
            }) => {
                assert_eq!(nix_bin, PathBuf::from("/nix/var/nix/bin/nix"));
                assert_eq!(out_dir, PathBuf::from("/tmp/r"));
            }
            _ => panic!("unexpected"),
        }
    }

    #[test]
    fn preflight_rejects_duplicate_nix_bin() {
        assert_eq!(
            parse(os(&[
                "preflight",
                "--nix-bin",
                "/a/nix",
                "--nix-bin",
                "/b/nix",
            ])),
            Err(CliError::DuplicateFlag)
        );
    }

    #[test]
    fn preflight_rejects_duplicate_out_dir() {
        assert_eq!(
            parse(os(&[
                "preflight",
                "--nix-bin",
                "/nix",
                "--out-dir",
                "a",
                "--out-dir",
                "b",
            ])),
            Err(CliError::DuplicateFlag)
        );
    }

    #[test]
    fn preflight_rejects_relative_and_empty_nix_bin() {
        assert_eq!(
            parse(os(&["preflight", "--nix-bin", "nix"])),
            Err(CliError::NixBinNotAbsolute)
        );
        assert_eq!(
            parse(os(&["preflight", "--nix-bin", "./nix"])),
            Err(CliError::NixBinNotAbsolute)
        );
        assert_eq!(
            parse(os(&["preflight", "--nix-bin", ""])),
            Err(CliError::NixBinNotAbsolute)
        );
    }

    #[test]
    fn preflight_rejects_equals_form() {
        // Equals form is rejected (only space-separated values apply).
        assert_eq!(
            parse(os(&["preflight", "--nix-bin=/nix"])),
            Err(CliError::EqualsForm)
        );
        assert_eq!(
            parse(os(&["preflight", "--out-dir=/tmp"])),
            Err(CliError::EqualsForm)
        );
    }

    #[test]
    fn preflight_rejects_unknown_flag_and_abbreviation() {
        assert!(matches!(
            parse(os(&["preflight", "--nix-bin", "/nix", "--verbose"])),
            Err(CliError::UnknownFlag { .. })
        ));
        // Abbreviations are unknown flags.
        assert!(matches!(
            parse(os(&["preflight", "--nix", "/nix"])),
            Err(CliError::UnknownFlag { .. })
        ));
    }

    #[test]
    fn preflight_rejects_positional_token() {
        assert_eq!(
            parse(os(&["preflight", "--nix-bin", "/nix", "extra"])),
            Err(CliError::PositionalToken)
        );
    }

    #[test]
    fn preflight_rejects_missing_value() {
        assert_eq!(
            parse(os(&["preflight", "--nix-bin"])),
            Err(CliError::MissingValue)
        );
        assert_eq!(
            parse(os(&["preflight", "--out-dir"])),
            Err(CliError::MissingValue)
        );
        // A flag where a value is expected is missing-value.
        assert_eq!(
            parse(os(&["preflight", "--nix-bin", "--out-dir", "/nix",])),
            Err(CliError::MissingValue)
        );
    }

    #[test]
    fn preflight_rejects_signing_credential_options() {
        for opt in [
            "--identity",
            "--keychain",
            "--password",
            "--team-id",
            "--sign",
        ] {
            let err = parse(os(&["preflight", opt, "x", "--nix-bin", "/nix"])).unwrap_err();
            assert_eq!(err, CliError::SigningOption, "opt {opt:?} should be denied");
        }
        // Equals form of a signing option is caught by its bare name first.
        assert_eq!(
            parse(os(&["preflight", "--identity=x", "--nix-bin", "/nix"])),
            Err(CliError::SigningOption)
        );
        // The offered value is never echoed in the bounded message.
        let s = CliError::SigningOption.to_string();
        assert!(!s.contains('x'));
    }

    #[test]
    fn preflight_rejects_flag_before_mode() {
        assert_eq!(
            parse(os(&["--nix-bin", "/nix", "preflight"])),
            Err(CliError::FlagBeforeMode)
        );
    }

    #[test]
    fn preflight_message_bounded_no_secret_and_usage_mentions_preflight() {
        // Required-flag message is bounded and static.
        let s = CliError::NixBinRequired.to_string();
        assert!(s.starts_with("cli: --nix-bin is required"));
        assert!(s.len() < 128);
        // USAGE truthfully advertises the preflight mode and the build/sign
        // boundary.
        assert!(USAGE.contains("preflight --nix-bin"));
        assert!(USAGE.contains("NO build/sign/notarization execution"));
        // Mode-list display now includes preflight.
        assert!(CliError::UnrecognizedMode.to_string().contains("preflight"));
        assert!(CliError::FlagBeforeMode.to_string().contains("preflight"));
    }

    #[test]
    fn usage_freezes_preflight_effect_contract_honestly() {
        // USAGE must NOT imply every Preflight operation targets cache.nixos.org:
        // flake prefetch fetches the pinned GITHUB flake/source, and ONLY the
        // store-info/path-info AVAILABILITY queries target cache.nixos.org.
        assert!(USAGE.contains("prefetch fetches the pinned GitHub"));
        assert!(USAGE.contains("availability queries target"));
        assert!(USAGE.contains("cache.nixos.org"));
        // The exact Nix 2.34.8 version is verified at RUNTIME from the supplied
        // absolute binary; the binary itself is caller-supplied, never pinned.
        assert!(USAGE.contains("2.34.8"));
        assert!(USAGE.contains("version is verified at runtime"));
        assert!(USAGE.contains("supplied absolute Nix"));
        // Preflight is honestly NOT read-only and may write normal Nix-managed
        // state ...
        assert!(USAGE.contains("NOT read-only"));
        assert!(USAGE.contains("write normal Nix-managed"));
        // ... while still drawing the hard line: no package build/profile
        // activation, no signing (and no shell/PATH lookup).
        assert!(USAGE.contains("no package"));
        assert!(USAGE.contains("activation"));
        assert!(USAGE.contains("signing"));
        // The global build/sign/notarization boundary line is still present.
        assert!(USAGE.contains("NO build/sign/notarization execution"));
    }

    #[test]
    fn preflight_non_utf8_nix_bin_value_is_preserved() {
        // A non-UTF-8 --nix-bin absolute value is carried losslessly; the
        // existence/absolute check operates on the OsStr directly.
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let bad = OsString::from_vec(vec![b'/', b'n', b'i', b'x', 0xff]);
            let mut args = vec![OsString::from("preflight"), OsString::from("--nix-bin")];
            args.push(bad.clone());
            match parse(args).unwrap() {
                Action::Run(RunArgs {
                    mode: RunMode::Preflight { nix_bin },
                    out_dir,
                }) => {
                    assert_eq!(nix_bin.as_os_str(), bad.as_os_str());
                    assert_eq!(out_dir, PathBuf::from("."));
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    // ---- rejections --------------------------------------------------------

    #[test]
    fn rejects_flag_before_mode() {
        assert_eq!(
            parse(os(&["--out-dir", "x", "fake"])),
            Err(CliError::FlagBeforeMode)
        );
    }

    #[test]
    fn rejects_unrecognized_mode() {
        assert_eq!(parse(os(&["bogus"])), Err(CliError::UnrecognizedMode));
    }

    #[test]
    fn rejects_equals_form() {
        assert_eq!(
            parse(os(&["fake", "--out-dir=/tmp"])),
            Err(CliError::EqualsForm)
        );
    }

    #[test]
    fn rejects_duplicate_flag() {
        assert_eq!(
            parse(os(&["fake", "--out-dir", "a", "--out-dir", "b"])),
            Err(CliError::DuplicateFlag)
        );
    }

    #[test]
    fn rejects_abbreviation() {
        // Abbreviations are unknown flags.
        assert!(matches!(
            parse(os(&["fake", "--out", "a"])),
            Err(CliError::UnknownFlag { .. })
        ));
    }

    #[test]
    fn rejects_positional_token() {
        assert_eq!(
            parse(os(&["fake", "extra"])),
            Err(CliError::PositionalToken)
        );
    }

    #[test]
    fn rejects_missing_value() {
        assert_eq!(
            parse(os(&["fake", "--out-dir"])),
            Err(CliError::MissingValue)
        );
        // A flag where a value is expected is also missing-value.
        assert_eq!(
            parse(os(&["fake", "--out-dir", "--out-dir"])),
            Err(CliError::MissingValue)
        );
    }

    #[test]
    fn rejects_nix_bin_in_fake() {
        assert_eq!(
            parse(os(&["fake", "--nix-bin", "/nix"])),
            Err(CliError::NixBinInFakeMode)
        );
    }

    #[test]
    fn rejects_relative_and_empty_nix_bin() {
        assert_eq!(
            parse(os(&["detect", "--nix-bin", "nix"])),
            Err(CliError::NixBinNotAbsolute)
        );
        assert_eq!(
            parse(os(&["detect", "--nix-bin", "./nix"])),
            Err(CliError::NixBinNotAbsolute)
        );
        assert_eq!(
            parse(os(&["detect", "--nix-bin", ""])),
            Err(CliError::NixBinNotAbsolute)
        );
    }

    #[test]
    fn rejects_signing_credential_options() {
        for opt in [
            "--identity",
            "--Identity",
            "--keychain",
            "--password",
            "--pass",
            "--api-key",
            "--api_key",
            "--team-id",
            "--teamid",
            "--apple-id",
            "--appleid",
            "--notary-profile",
            "--sign",
            "--certificate",
            "--cert",
            "--secret",
            "--token",
            "--credential",
            "--p12",
            "--developer-id",
            "--app-password",
            "--key",
        ] {
            let err = parse(os(&["detect", opt, "x"])).unwrap_err();
            assert_eq!(err, CliError::SigningOption, "opt {opt:?} should be denied");
        }
        // Equals form of a signing option is ALSO caught as signing (name checked
        // before the equals rejection would apply).
        let err = parse(os(&["detect", "--identity=x"])).unwrap_err();
        assert_eq!(err, CliError::SigningOption);
    }

    // ---- bounded display + non-UTF8 path preservation ---------------------

    #[test]
    fn display_is_bounded_and_has_no_credentials() {
        let huge = format!("--{}", "x".repeat(10_000));
        let err = parse(os(&["fake", &huge])).unwrap_err();
        let s = err.to_string();
        assert!(s.len() <= FLAG_SNIPPET_MAX + 48, "was {}: {s:?}", s.len());
        assert!(s.contains("..."));
        // SigningOption never echoes the offered token.
        let s = CliError::SigningOption.to_string();
        assert!(!s.contains("identity"));
    }

    #[test]
    fn non_utf8_out_dir_value_is_preserved() {
        // A non-UTF-8 OsString out-dir value is carried as a PathBuf (lossless).
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let bad = OsString::from_vec(vec![b'/', b't', b'm', b'p', 0xff, b'x']);
            let mut args = vec![OsString::from("fake"), OsString::from("--out-dir")];
            args.push(bad.clone());
            match parse(args).unwrap() {
                Action::Run(RunArgs {
                    mode: RunMode::Fake,
                    out_dir,
                }) => assert_eq!(out_dir.as_os_str(), bad.as_os_str()),
                _ => panic!("unexpected"),
            }
        }
    }
}
