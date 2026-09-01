//! Tests for the `system` module.

use super::*;

#[test]
fn all_contains_exactly_four() {
    assert_eq!(System::ALL.len(), 4);
    let mut seen = std::collections::HashSet::new();
    for s in System::ALL {
        assert!(seen.insert(s), "duplicate system in ALL");
    }
}

#[test]
fn display_and_as_str_match_canonical() {
    assert_eq!(System::X8664Linux.to_string(), "x86_64-linux");
    assert_eq!(System::Aarch64Linux.to_string(), "aarch64-linux");
    assert_eq!(System::X8664Darwin.to_string(), "x86_64-darwin");
    assert_eq!(System::Aarch64Darwin.to_string(), "aarch64-darwin");
}

#[test]
fn architecture_and_os_decompose() {
    assert_eq!(System::X8664Linux.architecture(), Architecture::X8664);
    assert_eq!(System::X8664Linux.os(), Os::Linux);
    assert_eq!(System::Aarch64Darwin.architecture(), Architecture::Aarch64);
    assert_eq!(System::Aarch64Darwin.os(), Os::Darwin);
    assert_eq!(Architecture::X8664.to_string(), "x86_64");
    assert_eq!(Architecture::Aarch64.to_string(), "aarch64");
    assert_eq!(Os::Linux.to_string(), "linux");
    assert_eq!(Os::Darwin.to_string(), "darwin");
}

#[test]
fn parse_round_trip() {
    for s in System::ALL {
        let string = s.to_string();
        let back = System::from_str(&string).unwrap();
        assert_eq!(s, back);
    }
}

#[test]
fn rejects_rust_target_triples_and_others() {
    let bad = [
        "",
        "x86_64-unknown-linux-gnu",
        "aarch64-apple-darwin",
        "x86_64",
        "linux",
        "darwin",
        "i686-linux",
        "armv7l-linux",
        "x86_64-Linux",
        "X86_64-linux",
        " x86_64-linux",
        "x86_64-linux ",
    ];
    for input in bad {
        let err = System::from_str(input).unwrap_err();
        assert_eq!(err, SystemError::try_parse(input), "input={input:?}");
    }
}

impl SystemError {
    fn try_parse(input: &str) -> SystemError {
        if input.is_empty() {
            SystemError::Empty
        } else {
            SystemError::Unknown {
                input: input.to_owned(),
            }
        }
    }
}

#[test]
fn error_display_is_informative() {
    let err = System::from_str("i686-linux").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("i686-linux"), "msg = {msg}");
    assert!(msg.contains("x86_64-linux"), "msg = {msg}");
}
