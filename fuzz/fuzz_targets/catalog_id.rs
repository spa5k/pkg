//! Package ID parsing must not panic on any text.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    if let Ok(parsed) = pkg_cli::catalog::parse_id(text) {
        let _ = format!("{parsed:?}");
    }
    let _ = pkg_cli::catalog::canonical_source(text);
});
