//! Spike S4 (PR-6 / DR-004) — CLI slice: a dependency-free, CLOSED command-line
//! parser for the `s4-runner` spike binary.
//!
//! # What this module owns
//! This module owns argument parsing and *only* argument parsing. It turns a
//! flat stream of [`OsString`] arguments (everything after `argv[0]`) into a
//! fully-resolved [`Action`], or a bounded, deterministic [`CliError`].
//!
//! # What this module deliberately does NOT do
//!   * It performs **no I/O**: it never checks whether a path exists on disk.
//!   * It performs **no `PATH` search** for the `nix` binary.
//!   * It does **not** map [`CliError`] to a process exit code — that belongs to
//!     `main.rs`.
//!   * It supports **no** `--flag=value` forms, abbreviations, environment
//!     overrides, installable overrides, `--impure`-style flags, or positional
//!     package values.
//!
//! # Grammar
//! ```text
//! s4-runner fake [--out-dir PATH]
//! s4-runner real --nix-bin ABSOLUTE_PATH [--out-dir PATH]
//! s4-runner --help | -h
//! ```
//! Command and option names are matched by *exact* [`OsStr`] equality. The
//! default output directory is `out`. The mode (`fake`/`real`) must be the
//! first non-help token; any option that precedes it is rejected. A
//! value-taking option consumes exactly the following token as its value,
//! verbatim, as long as it is nonempty *and not itself a recognized
//! command/option/help token*; a recognized token in that slot is treated as a
//! missing value. A path that starts with `-` can still be supplied by escaping
//! it (e.g. `./--name`) or by giving an absolute path. Non-UTF-8 path values
//! are preserved untouched on Unix.
//!
//! # Determinism
//! A given input stream always yields the same [`Action`] or the same
//! [`CliError`] variant. The parser first pre-scans for `--help`/`-h`, which
//! must be the sole token. It then runs a single left-to-right scan (which can
//! fail early on an option-before-mode, unknown token, missing value, empty
//! value, duplicate option, or duplicate mode) followed by a fixed post-scan
//! validation order: mode-presence, then per-mode constraints.

#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;

/// Maximum number of lossy characters retained from a caller-supplied token or
/// path snippet when it is rendered through [`CliError`]'s [`fmt::Display`]
/// implementation. Truncation appends exactly one ellipsis marker.
const MAX_TOKEN_CHARS: usize = 64;

/// Program usage banner.
pub const USAGE: &str = "\
Usage:
    s4-runner fake [--out-dir PATH]
    s4-runner real --nix-bin ABSOLUTE_PATH [--out-dir PATH]
    s4-runner --help | -h
";

/// The execution mode requested on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMode {
    /// Synthetic, dependency-free evaluation (no network, no Nix).
    Fake,
    /// Real evaluation that invokes the absolute `nix` executable at `nix_bin`
    /// directly as a subprocess. The runner must never use a shell and must
    /// never perform a `PATH` lookup: `nix_bin` is the exact program path handed
    /// to the operating system, verbatim from argv.
    Real {
        /// Absolute filesystem path to the `nix` binary, verbatim from argv.
        nix_bin: PathBuf,
    },
}

/// Fully-resolved arguments for a run action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    /// The selected execution mode.
    pub mode: RunMode,
    /// Output directory. Defaults to `out` when `--out-dir` is absent.
    pub out_dir: PathBuf,
}

/// The top-level action selected by the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Run the spike with the parsed arguments.
    Run(RunArgs),
    /// The user asked for help (`--help` / `-h`), standalone.
    Help,
}

/// A value-taking option recognized by the parser.
///
/// This small closed enum keeps [`CliError`] *bounded*: every value-taking
/// option is known ahead of time, so error variants carry a fixed tag rather
/// than an arbitrary caller string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    /// `--out-dir PATH`
    OutDir,
    /// `--nix-bin ABSOLUTE_PATH`
    NixBin,
}

impl fmt::Display for OptionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OptionKind::OutDir => "--out-dir",
            OptionKind::NixBin => "--nix-bin",
        })
    }
}

/// A bounded, deterministic parse error.
///
/// Each variant corresponds to exactly one parse-failure category. No variant
/// carries an open-ended caller string; the only caller-supplied data retained
/// is the offending token ([`UnknownToken`]) or path value
/// ([`NotAbsoluteNixBin`]), and both are capped to [`MAX_TOKEN_CHARS`] lossy
/// characters when displayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    /// No mode token (`fake` or `real`) was supplied.
    MissingMode,
    /// More than one mode token was supplied.
    DuplicateMode,
    /// A value-taking option appeared before the mode token (`fake`/`real`).
    OptionBeforeMode(OptionKind),
    /// A token that matches no command, option name, or consumed value.
    UnknownToken(OsString),
    /// A value-taking option appeared as the final token (its value is absent).
    MissingValue(OptionKind),
    /// A value-taking option was supplied more than once.
    DuplicateOption(OptionKind),
    /// A value-taking option was given an empty value.
    EmptyValue(OptionKind),
    /// `--nix-bin` is not valid together with the `fake` mode.
    NixBinForbiddenInFake,
    /// The `real` mode requires `--nix-bin` but none was supplied.
    MissingNixBin,
    /// `--nix-bin` was supplied a non-absolute path (carried verbatim).
    NotAbsoluteNixBin(OsString),
    /// `--help` / `-h` appeared alongside any other token.
    HelpNotStandalone,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::MissingMode => write!(f, "missing mode: expected `fake` or `real`"),
            CliError::DuplicateMode => {
                write!(f, "duplicate mode: provide at most one of `fake` or `real`")
            }
            CliError::OptionBeforeMode(option) => {
                write!(
                    f,
                    "option `{option}` must appear after the mode `fake`/`real`"
                )
            }
            CliError::UnknownToken(token) => write!(f, "unknown token: {}", bound_lossy(token)),
            CliError::MissingValue(option) => write!(f, "missing value for option `{option}`"),
            CliError::DuplicateOption(option) => write!(f, "duplicate option `{option}`"),
            CliError::EmptyValue(option) => {
                write!(f, "option `{option}` requires a nonempty value")
            }
            CliError::NixBinForbiddenInFake => {
                write!(f, "option `--nix-bin` is not valid with the `fake` mode")
            }
            CliError::MissingNixBin => {
                write!(f, "the `real` mode requires `--nix-bin ABSOLUTE_PATH`")
            }
            CliError::NotAbsoluteNixBin(value) => write!(
                f,
                "option `--nix-bin` requires an absolute path: {}",
                bound_lossy(value)
            ),
            CliError::HelpNotStandalone => {
                write!(f, "option `--help`/`-h` must appear on its own")
            }
        }
    }
}

impl std::error::Error for CliError {}

/// Parse command-line arguments (excluding `argv[0]`) into an [`Action`].
///
/// The parser is closed: only the tokens `fake`, `real`, `--out-dir`,
/// `--nix-bin`, `--help`, and `-h` are recognized, by exact `OsStr` equality.
/// The mode (`fake`/`real`) must be the first non-help token. A value-taking
/// option consumes exactly the following token as its value, verbatim, as long
/// as it is nonempty and not itself a recognized command/option/help token; a
/// recognized token in that slot is a missing value. Non-UTF-8 path values are
/// preserved verbatim on Unix and never cause a panic.
pub fn parse<I>(args: I) -> Result<Action, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    let tokens: Vec<OsString> = args.into_iter().collect();
    parse_tokens(&tokens)
}

// ---- private helpers --------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeKind {
    Fake,
    Real,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Help,
    ModeFake,
    ModeReal,
    OptOutDir,
    OptNixBin,
    Other,
}

fn classify(token: &OsStr) -> TokenKind {
    if token == OsStr::new("--help") || token == OsStr::new("-h") {
        TokenKind::Help
    } else if token == OsStr::new("fake") {
        TokenKind::ModeFake
    } else if token == OsStr::new("real") {
        TokenKind::ModeReal
    } else if token == OsStr::new("--out-dir") {
        TokenKind::OptOutDir
    } else if token == OsStr::new("--nix-bin") {
        TokenKind::OptNixBin
    } else {
        TokenKind::Other
    }
}

fn set_mode(slot: &mut Option<ModeKind>, incoming: ModeKind) -> Result<(), CliError> {
    if slot.is_some() {
        return Err(CliError::DuplicateMode);
    }
    *slot = Some(incoming);
    Ok(())
}

/// Read the value for a value-taking option. On entry `*index` points at the
/// value slot (the caller has already advanced past the option flag). On
/// success `*index` is advanced past the value.
fn read_value(
    tokens: &[OsString],
    index: &mut usize,
    option: OptionKind,
) -> Result<OsString, CliError> {
    match tokens.get(*index) {
        Some(value) if !value.is_empty() => {
            // A recognized command/option/help token is not a path value: the
            // user almost certainly forgot the value, so treat it as missing.
            // A path that merely *starts* with `-` (e.g. `--bogus`, `./--name`,
            // or an absolute path) is not a recognized token and is consumed
            // verbatim.
            if matches!(classify(value.as_os_str()), TokenKind::Other) {
                let out = value.clone();
                *index += 1;
                Ok(out)
            } else {
                Err(CliError::MissingValue(option))
            }
        }
        Some(_) => Err(CliError::EmptyValue(option)),
        None => Err(CliError::MissingValue(option)),
    }
}

fn parse_tokens(tokens: &[OsString]) -> Result<Action, CliError> {
    // Help pre-scan: `--help`/`-h` must be the *sole* token. This check runs
    // first and takes precedence over every other parse outcome (including
    // unknown tokens), so e.g. `--help --bogus` is `HelpNotStandalone`, never
    // `UnknownToken`. After this pre-scan no help token can appear below.
    let help_present = tokens
        .iter()
        .any(|t| matches!(classify(t.as_os_str()), TokenKind::Help));
    if help_present {
        return if tokens.len() == 1 {
            Ok(Action::Help)
        } else {
            Err(CliError::HelpNotStandalone)
        };
    }

    let mut mode: Option<ModeKind> = None;
    let mut out_dir: Option<OsString> = None;
    let mut nix_bin: Option<OsString> = None;

    let mut index = 0usize;
    while index < tokens.len() {
        match classify(tokens[index].as_os_str()) {
            TokenKind::Help => {
                // Unreachable: every help-bearing input is resolved above.
                return Err(CliError::HelpNotStandalone);
            }
            TokenKind::ModeFake => {
                set_mode(&mut mode, ModeKind::Fake)?;
                index += 1;
            }
            TokenKind::ModeReal => {
                set_mode(&mut mode, ModeKind::Real)?;
                index += 1;
            }
            TokenKind::OptOutDir => {
                // The mode must come first; an option before it is structural.
                if mode.is_none() {
                    return Err(CliError::OptionBeforeMode(OptionKind::OutDir));
                }
                index += 1;
                let value = read_value(tokens, &mut index, OptionKind::OutDir)?;
                if out_dir.is_some() {
                    return Err(CliError::DuplicateOption(OptionKind::OutDir));
                }
                out_dir = Some(value);
            }
            TokenKind::OptNixBin => {
                if mode.is_none() {
                    return Err(CliError::OptionBeforeMode(OptionKind::NixBin));
                }
                index += 1;
                let value = read_value(tokens, &mut index, OptionKind::NixBin)?;
                if nix_bin.is_some() {
                    return Err(CliError::DuplicateOption(OptionKind::NixBin));
                }
                nix_bin = Some(value);
            }
            TokenKind::Other => {
                return Err(CliError::UnknownToken(tokens[index].clone()));
            }
        }
    }

    let mode = mode.ok_or(CliError::MissingMode)?;

    let run_mode = match mode {
        ModeKind::Fake => {
            if nix_bin.is_some() {
                return Err(CliError::NixBinForbiddenInFake);
            }
            RunMode::Fake
        }
        ModeKind::Real => {
            let raw = nix_bin.ok_or(CliError::MissingNixBin)?;
            let path = PathBuf::from(raw);
            if !path.is_absolute() {
                return Err(CliError::NotAbsoluteNixBin(path.into_os_string()));
            }
            RunMode::Real { nix_bin: path }
        }
    };

    let out_dir = match out_dir {
        Some(value) => PathBuf::from(value),
        None => PathBuf::from("out"),
    };

    Ok(Action::Run(RunArgs {
        mode: run_mode,
        out_dir,
    }))
}

/// Render `value` lossily, capped at [`MAX_TOKEN_CHARS`] characters and marked
/// with a single ellipsis (`U+2026`) when truncation occurs. Truncation is
/// char-boundary aware, so it never splits a multi-byte codepoint and never
/// panics on non-UTF-8 input (which `to_string_lossy` turns into `U+FFFD`).
fn bound_lossy(value: &OsStr) -> String {
    let lossy = value.to_string_lossy();
    let text: &str = lossy.as_ref();
    if text.chars().count() <= MAX_TOKEN_CHARS {
        return text.to_string();
    }
    let mut head: String = text.chars().take(MAX_TOKEN_CHARS).collect();
    head.push('\u{2026}');
    head
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Build a [`Vec<OsString>`] from `&str` slices for readable test inputs.
    fn args(slice: &[&str]) -> Vec<OsString> {
        slice.iter().map(OsString::from).collect()
    }

    fn run(mode: RunMode, out_dir: &str) -> Action {
        Action::Run(RunArgs {
            mode,
            out_dir: PathBuf::from(out_dir),
        })
    }

    fn real(p: &str) -> RunMode {
        RunMode::Real {
            nix_bin: PathBuf::from(p),
        }
    }

    const ELLIPSIS: char = '\u{2026}';

    // ---- structural / sanity -------------------------------------------------

    #[test]
    fn usage_documents_the_full_grammar() {
        assert!(USAGE.contains("s4-runner fake [--out-dir PATH]"));
        assert!(USAGE.contains("s4-runner real --nix-bin ABSOLUTE_PATH [--out-dir PATH]"));
        assert!(USAGE.contains("s4-runner --help | -h"));
    }

    #[test]
    fn cli_error_is_a_std_error() {
        fn requires_error<E: std::error::Error>() {}
        requires_error::<CliError>();
    }

    #[test]
    fn option_kind_renders_its_flag_name() {
        assert_eq!(OptionKind::OutDir.to_string(), "--out-dir");
        assert_eq!(OptionKind::NixBin.to_string(), "--nix-bin");
    }

    // ---- happy paths: fake ---------------------------------------------------

    #[test]
    fn fake_uses_default_out_dir() {
        assert_eq!(parse(args(&["fake"])).unwrap(), run(RunMode::Fake, "out"));
    }

    #[test]
    fn fake_with_explicit_out_dir() {
        assert_eq!(
            parse(args(&["fake", "--out-dir", "custom"])).unwrap(),
            run(RunMode::Fake, "custom")
        );
    }

    #[test]
    fn out_dir_before_mode_is_rejected() {
        // The first non-help token must be the mode; an option before it is a
        // structural error reported immediately, without consuming its value.
        assert_eq!(
            parse(args(&["--out-dir", "early", "fake"])).unwrap_err(),
            CliError::OptionBeforeMode(OptionKind::OutDir)
        );
    }

    #[test]
    fn out_dir_without_mode_is_option_before_mode() {
        assert_eq!(
            parse(args(&["--out-dir", "x"])).unwrap_err(),
            CliError::OptionBeforeMode(OptionKind::OutDir)
        );
    }

    #[test]
    fn options_before_mode_reported_in_order() {
        // The first offending option wins; later tokens are not examined.
        assert_eq!(
            parse(args(&["--out-dir", "o2", "--nix-bin", "/n", "real"])).unwrap_err(),
            CliError::OptionBeforeMode(OptionKind::OutDir)
        );
    }

    #[test]
    fn fake_absolute_out_dir_is_allowed() {
        assert_eq!(
            parse(args(&["fake", "--out-dir", "/tmp/s4-out"])).unwrap(),
            run(RunMode::Fake, "/tmp/s4-out")
        );
    }

    // ---- happy paths: real ---------------------------------------------------

    #[test]
    fn real_minimum_with_default_out_dir() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", "/usr/bin/nix"])).unwrap(),
            run(real("/usr/bin/nix"), "out")
        );
    }

    #[test]
    fn real_with_explicit_out_dir() {
        assert_eq!(
            parse(args(&[
                "real",
                "--nix-bin",
                "/usr/bin/nix",
                "--out-dir",
                "res"
            ]))
            .unwrap(),
            run(real("/usr/bin/nix"), "res")
        );
    }

    #[test]
    fn real_root_path_is_absolute() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", "/"])).unwrap(),
            run(real("/"), "out")
        );
    }

    #[test]
    fn nix_bin_before_mode_is_rejected() {
        assert_eq!(
            parse(args(&["--nix-bin", "/usr/bin/nix", "real"])).unwrap_err(),
            CliError::OptionBeforeMode(OptionKind::NixBin)
        );
    }

    #[test]
    fn nix_bin_without_mode_is_option_before_mode() {
        assert_eq!(
            parse(args(&["--nix-bin", "/x"])).unwrap_err(),
            CliError::OptionBeforeMode(OptionKind::NixBin)
        );
    }

    #[test]
    fn trailing_out_dir_after_mode_is_accepted() {
        // Sanity: options that follow the mode are still perfectly valid.
        assert_eq!(
            parse(args(&["real", "--out-dir", "o", "--nix-bin", "/n"])).unwrap(),
            run(real("/n"), "o")
        );
    }

    #[test]
    fn real_flags_after_mode_in_any_order() {
        // nix-bin before out-dir, both after the mode — order is free post-mode.
        assert_eq!(
            parse(args(&["real", "--nix-bin", "/n", "--out-dir", "o2"])).unwrap(),
            run(real("/n"), "o2")
        );
    }

    // ---- happy paths: help ---------------------------------------------------

    #[test]
    fn help_long_form_alone() {
        assert_eq!(parse(args(&["--help"])).unwrap(), Action::Help);
    }

    #[test]
    fn help_short_form_alone() {
        assert_eq!(parse(args(&["-h"])).unwrap(), Action::Help);
    }

    // ---- default / empty inputs ---------------------------------------------

    #[test]
    fn empty_input_is_missing_mode() {
        assert_eq!(parse(args(&[])).unwrap_err(), CliError::MissingMode);
    }

    // ---- recognized tokens are not path values (#2 boundary) --------------

    #[test]
    fn out_dir_value_that_is_recognized_option_is_missing_value() {
        assert_eq!(
            parse(args(&["fake", "--out-dir", "--nix-bin"])).unwrap_err(),
            CliError::MissingValue(OptionKind::OutDir)
        );
    }

    #[test]
    fn out_dir_value_that_is_mode_token_is_missing_value() {
        assert_eq!(
            parse(args(&["fake", "--out-dir", "real"])).unwrap_err(),
            CliError::MissingValue(OptionKind::OutDir)
        );
    }

    #[test]
    fn nix_bin_value_that_is_recognized_option_is_missing_value() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", "--out-dir"])).unwrap_err(),
            CliError::MissingValue(OptionKind::NixBin)
        );
    }

    #[test]
    fn nix_bin_value_that_is_mode_token_is_missing_value() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", "fake"])).unwrap_err(),
            CliError::MissingValue(OptionKind::NixBin)
        );
    }

    // ---- relative / empty nix-bin -------------------------------------------

    #[test]
    fn real_relative_nix_bin_is_rejected() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", "relative/path"])).unwrap_err(),
            CliError::NotAbsoluteNixBin(OsString::from("relative/path"))
        );
    }

    #[test]
    fn real_dot_relative_nix_bin_is_rejected() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", "./nix"])).unwrap_err(),
            CliError::NotAbsoluteNixBin(OsString::from("./nix"))
        );
    }

    #[test]
    fn real_empty_nix_bin_value_is_rejected() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", ""])).unwrap_err(),
            CliError::EmptyValue(OptionKind::NixBin)
        );
    }

    #[test]
    fn fake_empty_out_dir_value_is_rejected() {
        assert_eq!(
            parse(args(&["fake", "--out-dir", ""])).unwrap_err(),
            CliError::EmptyValue(OptionKind::OutDir)
        );
    }

    // ---- duplicate flags -----------------------------------------------------

    #[test]
    fn duplicate_out_dir_is_rejected() {
        assert_eq!(
            parse(args(&["fake", "--out-dir", "a", "--out-dir", "b"])).unwrap_err(),
            CliError::DuplicateOption(OptionKind::OutDir)
        );
    }

    #[test]
    fn duplicate_nix_bin_is_rejected() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", "/a", "--nix-bin", "/b"])).unwrap_err(),
            CliError::DuplicateOption(OptionKind::NixBin)
        );
    }

    #[test]
    fn duplicate_nix_bin_with_out_dir_between_is_rejected() {
        assert_eq!(
            parse(args(&[
                "real",
                "--nix-bin",
                "/a",
                "--out-dir",
                "x",
                "--nix-bin",
                "/b"
            ]))
            .unwrap_err(),
            CliError::DuplicateOption(OptionKind::NixBin)
        );
    }

    // ---- missing values ------------------------------------------------------

    #[test]
    fn fake_trailing_out_dir_missing_value() {
        assert_eq!(
            parse(args(&["fake", "--out-dir"])).unwrap_err(),
            CliError::MissingValue(OptionKind::OutDir)
        );
    }

    #[test]
    fn real_trailing_nix_bin_missing_value() {
        assert_eq!(
            parse(args(&["real", "--nix-bin"])).unwrap_err(),
            CliError::MissingValue(OptionKind::NixBin)
        );
    }

    #[test]
    fn real_out_dir_missing_value_after_nix_bin() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", "/a", "--out-dir"])).unwrap_err(),
            CliError::MissingValue(OptionKind::OutDir)
        );
    }

    // ---- unknown tokens / non-supported forms -------------------------------

    #[test]
    fn unknown_bare_token() {
        assert_eq!(
            parse(args(&["bogus"])).unwrap_err(),
            CliError::UnknownToken(OsString::from("bogus"))
        );
    }

    #[test]
    fn unknown_token_after_mode() {
        assert_eq!(
            parse(args(&["fake", "bogus"])).unwrap_err(),
            CliError::UnknownToken(OsString::from("bogus"))
        );
    }

    #[test]
    fn unknown_long_option() {
        assert_eq!(
            parse(args(&["--unknown"])).unwrap_err(),
            CliError::UnknownToken(OsString::from("--unknown"))
        );
    }

    #[test]
    fn unknown_long_option_after_valid_args() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", "/a", "--bogus"])).unwrap_err(),
            CliError::UnknownToken(OsString::from("--bogus"))
        );
    }

    #[test]
    fn abbreviation_is_not_supported() {
        // `--out` is not `--out-dir`; no abbreviation matching.
        assert_eq!(
            parse(args(&["fake", "--out", "x"])).unwrap_err(),
            CliError::UnknownToken(OsString::from("--out"))
        );
    }

    #[test]
    fn equals_form_is_not_supported() {
        // `--nix-bin=/a` is a single unknown token; no `=` splitting.
        assert_eq!(
            parse(args(&["real", "--nix-bin=/a"])).unwrap_err(),
            CliError::UnknownToken(OsString::from("--nix-bin=/a"))
        );
    }

    #[test]
    fn short_help_typo_is_unknown() {
        // Only `-h` is help; `-help` is an unknown token.
        assert_eq!(
            parse(args(&["-help"])).unwrap_err(),
            CliError::UnknownToken(OsString::from("-help"))
        );
    }

    #[test]
    fn positional_package_is_not_supported() {
        // A bare path after the mode is an unknown token (no installable override).
        assert_eq!(
            parse(args(&["fake", "nixpkgs#hello"])).unwrap_err(),
            CliError::UnknownToken(OsString::from("nixpkgs#hello"))
        );
    }

    // ---- mode misuse ---------------------------------------------------------

    #[test]
    fn fake_then_real_is_duplicate_mode() {
        assert_eq!(
            parse(args(&["fake", "real"])).unwrap_err(),
            CliError::DuplicateMode
        );
    }

    #[test]
    fn fake_then_fake_is_duplicate_mode() {
        assert_eq!(
            parse(args(&["fake", "fake"])).unwrap_err(),
            CliError::DuplicateMode
        );
    }

    #[test]
    fn real_then_real_is_duplicate_mode() {
        assert_eq!(
            parse(args(&["real", "real"])).unwrap_err(),
            CliError::DuplicateMode
        );
    }

    #[test]
    fn real_then_fake_is_duplicate_mode() {
        assert_eq!(
            parse(args(&["real", "fake"])).unwrap_err(),
            CliError::DuplicateMode
        );
    }

    #[test]
    fn fake_forbids_nix_bin() {
        assert_eq!(
            parse(args(&["fake", "--nix-bin", "/a"])).unwrap_err(),
            CliError::NixBinForbiddenInFake
        );
    }

    #[test]
    fn real_requires_nix_bin() {
        assert_eq!(parse(args(&["real"])).unwrap_err(), CliError::MissingNixBin);
    }

    // ---- help must be standalone --------------------------------------------

    #[test]
    fn fake_then_help_is_rejected() {
        assert_eq!(
            parse(args(&["fake", "--help"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    #[test]
    fn help_then_fake_is_rejected() {
        assert_eq!(
            parse(args(&["--help", "fake"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    #[test]
    fn real_then_help_is_rejected() {
        assert_eq!(
            parse(args(&["real", "--help"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    #[test]
    fn help_with_out_dir_is_rejected() {
        assert_eq!(
            parse(args(&["--help", "--out-dir", "x"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    #[test]
    fn out_dir_then_help_is_rejected() {
        assert_eq!(
            parse(args(&["--out-dir", "x", "--help"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    #[test]
    fn real_with_nix_bin_then_help_is_rejected() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", "/a", "--help"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    #[test]
    fn help_twice_is_rejected() {
        assert_eq!(
            parse(args(&["--help", "--help"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    #[test]
    fn help_long_and_short_together_is_rejected() {
        assert_eq!(
            parse(args(&["--help", "-h"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    #[test]
    fn short_help_then_fake_is_rejected() {
        assert_eq!(
            parse(args(&["-h", "fake"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    #[test]
    fn help_with_unknown_token_is_help_not_standalone() {
        // Help precedence: an unknown token does not turn this into
        // `UnknownToken`; help alongside anything else is `HelpNotStandalone`.
        assert_eq!(
            parse(args(&["--help", "--bogus"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    #[test]
    fn unknown_token_then_help_is_help_not_standalone() {
        assert_eq!(
            parse(args(&["--bogus", "--help"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    #[test]
    fn short_help_with_unknown_token_is_help_not_standalone() {
        assert_eq!(
            parse(args(&["-h", "--bogus"])).unwrap_err(),
            CliError::HelpNotStandalone
        );
    }

    // ---- closed-parser value consumption ------------------------------------

    #[test]
    fn out_dir_consumes_unrecognized_dash_token_verbatim() {
        // An unrecognized `-`-prefixed token is a legitimate path value.
        assert_eq!(
            parse(args(&["fake", "--out-dir", "--bogus"])).unwrap(),
            run(RunMode::Fake, "--bogus")
        );
    }

    #[test]
    fn out_dir_accepts_dashes_via_dot_slash_escape() {
        assert_eq!(
            parse(args(&["fake", "--out-dir", "./--nix-bin"])).unwrap(),
            run(RunMode::Fake, "./--nix-bin")
        );
    }

    #[test]
    fn out_dir_accepts_absolute_path_starting_with_dashes() {
        assert_eq!(
            parse(args(&["fake", "--out-dir", "/--weird"])).unwrap(),
            run(RunMode::Fake, "/--weird")
        );
    }

    #[test]
    fn nix_bin_accepts_absolute_path_starting_with_dashes() {
        assert_eq!(
            parse(args(&["real", "--nix-bin", "/--weird"])).unwrap(),
            run(real("/--weird"), "out")
        );
    }

    // ---- Display bounding ----------------------------------------------------

    #[test]
    fn unknown_token_display_is_bounded_at_exactly_64_chars() {
        let exact = "a".repeat(64);
        let err = parse(args(&[&exact])).unwrap_err();
        assert_eq!(err, CliError::UnknownToken(OsString::from(&exact)));
        assert_eq!(err.to_string(), format!("unknown token: {exact}"));
        assert!(!err.to_string().contains(ELLIPSIS));
    }

    #[test]
    fn unknown_token_display_truncates_at_65_chars() {
        let input = "a".repeat(65);
        let err = parse(args(&[&input])).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("unknown token: {}{ELLIPSIS}", "a".repeat(64))
        );
    }

    #[test]
    fn unknown_token_display_truncates_long_input() {
        let input = "a".repeat(200);
        let err = parse(args(&[&input])).unwrap_err();
        let displayed = err.to_string();
        assert!(displayed.starts_with("unknown token: "));
        let body = displayed.strip_prefix("unknown token: ").unwrap();
        // 64 codepoints + exactly one ellipsis marker.
        assert_eq!(body.chars().count(), 65);
        assert_eq!(body.chars().last(), Some(ELLIPSIS));
    }

    #[test]
    fn unknown_token_display_truncation_is_char_aware() {
        // Each 'é' is one codepoint but two UTF-8 bytes; byte slicing would
        // mis-truncate. Verify char-boundary aware truncation.
        let input = "é".repeat(70);
        let err = parse(args(&[&input])).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("unknown token: {}{ELLIPSIS}", "é".repeat(64))
        );
    }

    #[test]
    fn not_absolute_nix_bin_display_is_bounded() {
        let long_rel = "z".repeat(120);
        let err = parse(args(&["real", "--nix-bin", &long_rel])).unwrap_err();
        assert_eq!(err, CliError::NotAbsoluteNixBin(OsString::from(&long_rel)));
        assert_eq!(
            err.to_string(),
            format!(
                "option `--nix-bin` requires an absolute path: {}{ELLIPSIS}",
                "z".repeat(64)
            )
        );
    }

    #[test]
    fn display_messages_are_human_readable() {
        assert_eq!(
            CliError::MissingMode.to_string(),
            "missing mode: expected `fake` or `real`"
        );
        assert_eq!(
            CliError::DuplicateMode.to_string(),
            "duplicate mode: provide at most one of `fake` or `real`"
        );
        assert_eq!(
            CliError::OptionBeforeMode(OptionKind::NixBin).to_string(),
            "option `--nix-bin` must appear after the mode `fake`/`real`"
        );
        assert_eq!(
            CliError::MissingValue(OptionKind::NixBin).to_string(),
            "missing value for option `--nix-bin`"
        );
        assert_eq!(
            CliError::NixBinForbiddenInFake.to_string(),
            "option `--nix-bin` is not valid with the `fake` mode"
        );
        assert_eq!(
            CliError::MissingNixBin.to_string(),
            "the `real` mode requires `--nix-bin ABSOLUTE_PATH`"
        );
        assert_eq!(
            CliError::HelpNotStandalone.to_string(),
            "option `--help`/`-h` must appear on its own"
        );
    }

    // ---- non-UTF-8 preservation (Unix only) ---------------------------------

    #[cfg(unix)]
    #[test]
    fn non_utf8_absolute_nix_bin_is_preserved_unix() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let raw: Vec<u8> = vec![b'/', 0xFF, b'x'];
        let arg = OsString::from_vec(raw.clone());
        let action = parse(vec![
            OsString::from("real"),
            OsString::from("--nix-bin"),
            arg,
        ])
        .expect("absolute non-UTF-8 path must parse");
        match action {
            Action::Run(RunArgs {
                mode: RunMode::Real { nix_bin },
                out_dir,
            }) => {
                assert_eq!(nix_bin.as_os_str().as_bytes(), &raw[..]);
                assert_eq!(out_dir, PathBuf::from("out"));
            }
            other => panic!("expected Run(Real), got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_out_dir_is_preserved_unix() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let raw: Vec<u8> = vec![0xFF, b'x'];
        let arg = OsString::from_vec(raw.clone());
        let action = parse(vec![
            OsString::from("fake"),
            OsString::from("--out-dir"),
            arg,
        ])
        .expect("non-UTF-8 out-dir must parse in fake mode");
        match action {
            Action::Run(RunArgs {
                mode: RunMode::Fake,
                out_dir,
            }) => {
                assert_eq!(out_dir.as_os_str().as_bytes(), &raw[..]);
            }
            other => panic!("expected Run(Fake), got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_relative_nix_bin_is_not_absolute_unix() {
        use std::os::unix::ffi::OsStringExt;
        let raw: Vec<u8> = vec![0xFF, b'x']; // relative, non-UTF-8
        let arg = OsString::from_vec(raw.clone());
        let err = parse(vec![
            OsString::from("real"),
            OsString::from("--nix-bin"),
            arg,
        ])
        .unwrap_err();
        assert!(matches!(err, CliError::NotAbsoluteNixBin(_)));
        // Must not panic on non-UTF-8 during Display.
        let displayed = format!("{err}");
        assert!(displayed.starts_with("option `--nix-bin` requires an absolute path: "));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_unknown_token_display_does_not_panic_unix() {
        use std::os::unix::ffi::OsStringExt;
        let arg = OsString::from_vec(vec![0xFF, b'q']);
        let err = parse(vec![arg]).unwrap_err();
        let displayed = format!("{err}");
        assert!(displayed.starts_with("unknown token: "));
        // Lossy decoding yields U+FFFD for the invalid byte.
        assert!(displayed.contains('\u{FFFD}'));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_long_relative_nix_bin_display_is_bounded_unix() {
        use std::os::unix::ffi::OsStringExt;
        // A long non-UTF-8 relative path: many 'a's then an invalid byte.
        let mut raw: Vec<u8> = vec![b'a'; 100];
        raw.push(0xFF);
        let arg = OsString::from_vec(raw);
        let err = parse(vec![
            OsString::from("real"),
            OsString::from("--nix-bin"),
            arg,
        ])
        .unwrap_err();
        let displayed = format!("{err}");
        let prefix = "option `--nix-bin` requires an absolute path: ";
        assert!(displayed.starts_with(prefix));
        let body = displayed.strip_prefix(prefix).unwrap();
        // 64 lossy chars + ellipsis (the invalid byte becomes one U+FFFD char
        // but is past the 64-char cut, so the body is exactly 65 codepoints).
        assert_eq!(body.chars().count(), 65);
        assert_eq!(body.chars().last(), Some(ELLIPSIS));
    }
}
