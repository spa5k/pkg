//! One-command release key management and channel publication.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use pkg_release::{Environment, PublishChannel, RelError, init_key_set, publish_channel};

#[allow(
    clippy::print_stdout,
    reason = "the release tool prints the key identity and the release card"
)]
#[allow(clippy::print_stderr, reason = "the release tool only prints failures")]
fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            report(&format!("the tokio runtime is unavailable: {error}"));
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(env::args_os())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            report(&message);
            ExitCode::FAILURE
        }
    }
}

fn report(message: &str) {
    eprintln!("pkg-rel refused: {message}");
}

fn describe(error: &RelError) -> String {
    error.to_string()
}

#[expect(
    clippy::future_not_send,
    reason = "the pkg-rel runtime is single-threaded; signing futures stay on it"
)]
async fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    let mut arguments = arguments.into_iter();
    let _program = arguments.next();
    let command = match arguments.next().map(text) {
        Some(command) => command?,
        None => return Err(usage().to_owned()),
    };
    match command.as_str() {
        "key" => key_command(arguments).await,
        "publish" => publish_command(arguments).await,
        _ => Err(usage().to_owned()),
    }
}

const fn usage() -> &'static str {
    "usage: pkg-rel key init --env test|prod --out DIR\n\
     usage: pkg-rel publish --key-dir DIR --targets DIR --out DIR --sequence N\n\
     [--lane NAME] [--url URL] [--commit SHA]"
}

async fn key_command(arguments: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    let mut arguments = arguments.into_iter();
    if next_text(&mut arguments)? != "init" {
        return Err("usage: pkg-rel key init --env test|prod --out DIR".to_owned());
    }
    let flags = parse_flags(arguments)?;
    key_flags(&flags)?;
    let environment = Environment::parse(&flag_value(&flags, "--env", "key init")?)
        .map_err(|error| describe(&error))?;
    let out = PathBuf::from(flag_value(&flags, "--out", "key init")?);
    let key_set = init_key_set(&out, environment)
        .await
        .map_err(|error| describe(&error))?;
    println!("online key id: {}", key_set.online_key_id);
    println!("root sha256: {}", key_set.root_sha256);
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "the pkg-rel runtime is single-threaded; signing futures stay on it"
)]
async fn publish_command(arguments: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    let flags = parse_flags(arguments)?;
    publish_flags(&flags)?;
    let publish = PublishInput {
        key_dir: PathBuf::from(flag_value(&flags, "--key-dir", "publish")?),
        targets_dir: PathBuf::from(flag_value(&flags, "--targets", "publish")?),
        channel_dir: PathBuf::from(flag_value(&flags, "--out", "publish")?),
        sequence: flag_value(&flags, "--sequence", "publish")?
            .parse()
            .map_err(|_| "pkg-rel publish requires a numeric --sequence".to_owned())?,
        lane: flags.get("--lane").cloned(),
        url: flags.get("--url").cloned(),
        commit: flags.get("--commit").cloned(),
    };
    let card = publish_channel(PublishChannel {
        key_dir: &publish.key_dir,
        targets_dir: &publish.targets_dir,
        channel_dir: &publish.channel_dir,
        sequence: publish.sequence,
        lane: publish.lane.as_deref(),
        url: publish.url.as_deref(),
        commit: publish.commit.as_deref(),
    })
    .await
    .map_err(|error| describe(&error))?;
    println!("{}", card.to_json_line());
    Ok(())
}

struct PublishInput {
    key_dir: PathBuf,
    targets_dir: PathBuf,
    channel_dir: PathBuf,
    sequence: u64,
    lane: Option<String>,
    url: Option<String>,
    commit: Option<String>,
}

const PUBLISH_FLAGS: [&str; 7] = [
    "--key-dir",
    "--targets",
    "--out",
    "--sequence",
    "--lane",
    "--url",
    "--commit",
];

fn parse_flags(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<BTreeMap<String, String>, String> {
    let mut flags = BTreeMap::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let flag = text(argument)?;
        if !flag.starts_with("--") {
            return Err(format!("pkg-rel refuses the argument {flag}"));
        }
        let value = next_text(&mut arguments)?;
        if flags.insert(flag.clone(), value).is_some() {
            return Err(format!("pkg-rel refuses the repeated argument {flag}"));
        }
    }
    Ok(flags)
}

fn key_flags(flags: &BTreeMap<String, String>) -> Result<(), String> {
    for flag in flags.keys() {
        if flag != "--env" && flag != "--out" {
            return Err("pkg-rel key init accepts only --env and --out".to_owned());
        }
    }
    Ok(())
}

fn publish_flags(flags: &BTreeMap<String, String>) -> Result<(), String> {
    for flag in flags.keys() {
        if !PUBLISH_FLAGS.contains(&flag.as_str()) {
            return Err(format!("pkg-rel publish refuses the argument {flag}"));
        }
    }
    Ok(())
}

fn text(value: OsString) -> Result<String, String> {
    value
        .into_string()
        .map_err(|_| "pkg-rel arguments must be valid UTF-8".to_owned())
}

fn next_text(arguments: &mut impl Iterator<Item = OsString>) -> Result<String, String> {
    arguments
        .next()
        .map(text)
        .unwrap_or_else(|| Err("pkg-rel expected another argument".to_owned()))
}

fn flag_value(
    flags: &BTreeMap<String, String>,
    flag: &str,
    command: &str,
) -> Result<String, String> {
    flags
        .get(flag)
        .cloned()
        .ok_or_else(|| format!("pkg-rel {command} requires {flag}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_arguments() -> Vec<OsString> {
        [
            "--env".to_owned(),
            "test".to_owned(),
            "--out".to_owned(),
            "keys".to_owned(),
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    fn publish_arguments() -> Vec<OsString> {
        [
            "--key-dir".to_owned(),
            "keys".to_owned(),
            "--targets".to_owned(),
            "targets".to_owned(),
            "--out".to_owned(),
            "channel".to_owned(),
            "--sequence".to_owned(),
            "3".to_owned(),
            "--lane".to_owned(),
            "alpha".to_owned(),
            "--url".to_owned(),
            "https://channel.kelv.dev/alpha/".to_owned(),
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    #[test]
    fn key_init_parse_accepts_only_the_closed_inputs() {
        let mut arguments = key_arguments();
        arguments.pop();
        let error = parse_flags(arguments).expect_err("missing --out value");
        assert_eq!(error, "pkg-rel expected another argument");
        let flags = parse_flags(key_arguments()).expect("valid key init input");
        key_flags(&flags).expect("closed key init flags");
        assert_eq!(
            flag_value(&flags, "--env", "key init").as_deref(),
            Ok("test")
        );
    }
    #[test]
    fn publish_parse_accepts_the_closed_inputs_and_keeps_defaults() {
        let flags = parse_flags(publish_arguments()).expect("valid publish input");
        assert_eq!(flags.get("--sequence").map(String::as_str), Some("3"));
        assert_eq!(flags.len(), 6);
        publish_flags(&flags).expect("closed publish flags");

        let mut defaults = publish_arguments();
        for _ in 0..4 {
            defaults.pop();
        }
        let flags = parse_flags(defaults).expect("valid publish defaults");
        assert!(!flags.contains_key("--lane"));
        assert!(!flags.contains_key("--url"));
        assert!(!flags.contains_key("--commit"));
    }

    #[test]
    fn flag_parsing_rejects_unknown_repeated_and_bare_arguments() {
        let mut unknown = key_arguments();
        unknown.push("--lane".to_owned().into());
        unknown.push("alpha".to_owned().into());
        let flags = parse_flags(unknown).expect("flags parse before validation");
        assert_eq!(
            key_flags(&flags).expect_err("unknown key init flag"),
            "pkg-rel key init accepts only --env and --out"
        );

        let mut repeated = publish_arguments();
        repeated.push("--url".to_owned().into());
        repeated.push("https://channel.kelv.dev/beta/".to_owned().into());
        assert_eq!(
            parse_flags(repeated).expect_err("repeated publish flag"),
            "pkg-rel refuses the repeated argument --url"
        );
        let bare = vec![
            "--sequence".to_owned().into(),
            "1".to_owned().into(),
            "2".to_owned().into(),
        ];
        assert_eq!(
            parse_flags(bare).expect_err("bare flag value"),
            "pkg-rel refuses the argument 2"
        );
    }
}
