//! Builds the signed channel index for one pinned nixpkgs revision.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use pkg_core::{ChannelSequence, System};
use pkg_index::{BuildMetadata, build_index_from_json, compress_index};
use pkg_nix::{NixpkgsPin, RealNixAdapter, fetch_pinned_nixpkgs};
use serde_json::Value;
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

#[allow(clippy::print_stdout, reason = "the release tool only product output")]
#[allow(clippy::print_stderr, reason = "the release tool only failure output")]
fn main() {
    if let Err(message) = run(env::args_os()) {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<(), &'static str> {
    let arguments: Vec<_> = arguments.into_iter().collect();
    if arguments.get(1).is_some_and(|value| value == "--stage") {
        return stage(&arguments);
    }
    build(parse(arguments)?)
}

fn build(input: Input) -> Result<(), &'static str> {
    let adapter = RealNixAdapter::new_standard_determinate(Path::new(PRIVATE_HOME))
        .map_err(|_| "pkg release index refused: vendor runtime unavailable")?;
    let source = fetch_pinned_nixpkgs(&input.pin, &adapter)
        .map_err(|_| "pkg release index refused: source authentication failed")?;
    let projection = adapter
        .project_nixpkgs_index(&source, input.system)
        .map_err(|_| "pkg release index refused: fixed projection failed")?;
    let index = build_index_from_json(input.metadata, &projection)
        .map_err(|_| "pkg release index refused: index validation failed")?;
    if index.document().records().is_empty() {
        return Err("pkg release index refused: no packages were discovered");
    }
    let compressed = compress_index(index.bytes())
        .map_err(|_| "pkg release index refused: index compression failed")?;
    write_exclusive(&input.output, &compressed)?;
    println!("index packages {}", index.document().records().len());
    println!("index sha256 {}", hex::encode(Sha256::digest(&compressed)));
    Ok(())
}

// Use the descriptor as the only source of the release sequence and Nixpkgs pin.
// Build into a temporary directory. Bind the descriptor only after success.
fn stage(arguments: &[OsString]) -> Result<(), &'static str> {
    if arguments.len() != 5 {
        return Err("usage: pkg-release-index --stage TARGETS SYSTEM GENERATED_AT");
    }
    let targets = Path::new(&arguments[2]);
    let descriptor_path = targets.join("descriptor.json");
    let mut descriptor: Value = serde_json::from_slice(
        &fs::read(&descriptor_path).map_err(|_| "release descriptor unavailable")?,
    )
    .map_err(|_| "invalid release descriptor")?;
    let temporary = tempfile::tempdir_in(targets).map_err(|_| "output unavailable")?;
    let output = temporary.path().join("index.json.br");
    let input = staged_input(&descriptor, &arguments[3], &arguments[4], &output)?;
    let system = input.system;
    build(input)?;
    let bytes = fs::read(&output).map_err(|_| "index output unavailable")?;
    let relative = format!("index/{}/{system}.json.br", descriptor["sequence"]);
    let destination = targets.join(&relative);
    fs::create_dir_all(destination.parent().ok_or("invalid output path")?)
        .map_err(|_| "output directory unavailable")?;
    write_exclusive(&destination, &bytes)?;
    descriptor["index"]["perSystem"][system.as_str()] = serde_json::json!({
        "target": relative,
        "sha256": hex::encode(Sha256::digest(&bytes)),
    });
    let mut updated = NamedTempFile::new_in(targets).map_err(|_| "output unavailable")?;
    serde_json::to_writer(&mut updated, &descriptor).map_err(|_| "descriptor write failed")?;
    updated
        .as_file()
        .sync_all()
        .map_err(|_| "descriptor sync failed")?;
    updated
        .persist(descriptor_path)
        .map_err(|_| "descriptor replacement failed")?;
    Ok(())
}

fn staged_input(
    descriptor: &Value,
    system: &OsString,
    generated_at: &OsString,
    output: &Path,
) -> Result<Input, &'static str> {
    let system_text = system.to_str().ok_or("invalid system")?;
    if descriptor["schemaVersion"] != 1
        || descriptor["index"]["source"] != "self-built"
        || !descriptor["supportedSystems"]
            .as_array()
            .is_some_and(|systems| systems.iter().any(|value| value == system_text))
        || !descriptor["index"]["perSystem"].is_object()
    {
        return Err("invalid release descriptor or unsupported system");
    }
    let sequence = descriptor["sequence"].as_u64().ok_or("invalid sequence")?;
    let revision = descriptor["nixpkgs"]["rev"]
        .as_str()
        .ok_or("invalid revision")?;
    let hash = descriptor["nixpkgs"]["narHash"]
        .as_str()
        .ok_or("invalid source hash")?;
    if descriptor["nixpkgs"]["owner"] != "NixOS" || descriptor["nixpkgs"]["repo"] != "nixpkgs" {
        return Err("invalid Nixpkgs source");
    }
    parse([
        "pkg-release-index".into(),
        sequence.to_string().into(),
        system.clone(),
        revision.into(),
        hash.into(),
        generated_at.clone(),
        output.as_os_str().to_owned(),
    ])
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

    #[test]
    fn staging_uses_descriptor_pin_and_refuses_unsupported_systems() {
        let mut descriptor = serde_json::json!({
            "schemaVersion": 1, "sequence": 48,
            "supportedSystems": ["aarch64-darwin"],
            "nixpkgs": {"owner": "NixOS", "repo": "nixpkgs", "rev": REVISION, "narHash": NAR_HASH},
            "index": {"source": "self-built", "perSystem": {}}
        });
        let system = OsString::from("aarch64-darwin");
        let generated_at = OsString::from("2026-09-23T00:00:00Z");
        let output = Path::new("index.json.br");
        let input = staged_input(&descriptor, &system, &generated_at, output).unwrap();
        assert_eq!(input.pin.revision().as_str(), REVISION);
        assert_eq!(input.output, output);
        assert!(staged_input(&descriptor, &"x86_64-linux".into(), &generated_at, output).is_err());
        descriptor["nixpkgs"]["repo"] = "other".into();
        assert!(staged_input(&descriptor, &system, &generated_at, output).is_err());
        descriptor["nixpkgs"]["repo"] = "nixpkgs".into();
        descriptor["sequence"] = "../../escape".into();
        assert!(staged_input(&descriptor, &system, &generated_at, output).is_err());
    }
}
