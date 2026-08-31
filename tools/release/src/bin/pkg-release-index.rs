use std::env;
use std::ffi::OsString;
use std::io::Write;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use pkg_core::{ChannelSequence, System};
use pkg_index::{BuildMetadata, build_index_from_json, compress_index};
use pkg_nix::{NixpkgsPin, RealNixAdapter, fetch_pinned_nixpkgs};
use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;

#[cfg(target_os = "linux")]
const PRIVATE_HOME: &str = "/var/lib/pkg/broker-home";
#[cfg(target_os = "macos")]
const PRIVATE_HOME: &str = "/Library/Application Support/pkg/broker-home";

struct Input {
    metadata: BuildMetadata,
    pin: NixpkgsPin,
    output: PathBuf,
    system: System,
}

fn main() {
    if let Err(message) = run(env::args_os()) {
        #[expect(
            clippy::print_stderr,
            reason = "the release tool's only product output"
        )]
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<(), &'static str> {
    let input = parse(arguments)?;
    let adapter = RealNixAdapter::new_standard_determinate(Path::new(PRIVATE_HOME))
        .map_err(|_| "pkg release index refused: vendor runtime unavailable")?;
    let source = fetch_pinned_nixpkgs(&input.pin, &adapter)
        .map_err(|_| "pkg release index refused: source authentication failed")?;
    let projection = adapter
        .project_nixpkgs_index(&source, input.system)
        .map_err(|_| "pkg release index refused: fixed projection failed")?;
    let index = build_index_from_json(input.metadata, &projection)
        .map_err(|_| "pkg release index refused: index validation failed")?;
    let compressed = compress_index(index.bytes())
        .map_err(|_| "pkg release index refused: index compression failed")?;
    write_exclusive(&input.output, &compressed)?;
    #[expect(
        clippy::print_stdout,
        reason = "the release tool's only product output"
    )]
    println!("index sha256 {}", hex::encode(Sha256::digest(&compressed)));
    Ok(())
}

fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Input, &'static str> {
    let mut arguments = arguments.into_iter();
    let _program = arguments.next();
    let sequence = next_text(&mut arguments)?
        .parse::<u64>()
        .ok()
        .and_then(NonZeroU64::new)
        .map(ChannelSequence::new)
        .ok_or("pkg release index refused: invalid input")?;
    let system = System::from_str(&next_text(&mut arguments)?)
        .map_err(|_| "pkg release index refused: invalid input")?;
    let revision = next_text(&mut arguments)?;
    let nar_hash = next_text(&mut arguments)?;
    let generated_at = next_text(&mut arguments)?;
    let output = PathBuf::from(
        arguments
            .next()
            .ok_or("pkg release index refused: invalid input")?,
    );
    if arguments.next().is_some() {
        return Err("pkg release index refused: invalid input");
    }
    let pin = NixpkgsPin::new(&revision, &nar_hash)
        .map_err(|_| "pkg release index refused: invalid input")?;
    let metadata = BuildMetadata::new(sequence, system, pin.revision().clone(), &generated_at)
        .map_err(|_| "pkg release index refused: invalid input")?;
    Ok(Input {
        metadata,
        pin,
        output,
        system,
    })
}

fn next_text(arguments: &mut impl Iterator<Item = OsString>) -> Result<String, &'static str> {
    arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or("pkg release index refused: invalid input")
}

fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<(), &'static str> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut output = NamedTempFile::new_in(parent)
        .map_err(|_| "pkg release index refused: output unavailable")?;
    output
        .write_all(bytes)
        .and_then(|()| output.as_file().sync_all())
        .map_err(|_| "pkg release index refused: output failed")?;
    output
        .persist_noclobber(path)
        .map(|_| ())
        .map_err(|_| "pkg release index refused: output unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const NAR_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn arguments(output: &Path) -> Vec<OsString> {
        [
            "pkg-release-index".into(),
            "7".into(),
            "aarch64-darwin".into(),
            REVISION.into(),
            NAR_HASH.into(),
            "2026-08-18T00:00:00Z".into(),
            output.as_os_str().to_owned(),
        ]
        .into()
    }

    #[test]
    fn parser_accepts_only_the_closed_release_inputs() {
        let input = parse(arguments(Path::new("index.json.br"))).unwrap();
        assert_eq!(input.system, System::Aarch64Darwin);
        assert_eq!(input.pin.revision().as_str(), REVISION);

        let mut extra = arguments(Path::new("index.json.br"));
        extra.push("--option".into());
        assert!(parse(extra).is_err());
    }

    #[test]
    fn output_must_not_exist() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("index.json.br");
        write_exclusive(&output, b"first").unwrap();
        assert_eq!(
            write_exclusive(&output, b"second"),
            Err("pkg release index refused: output unavailable")
        );
        assert_eq!(std::fs::read(output).unwrap(), b"first");
    }
}
