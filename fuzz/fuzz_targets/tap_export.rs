//! Tap export conversion must not panic, and it writes only inside its
//! output folder.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let root = tempfile::tempdir().unwrap();
    let _ = cask_catalog::tap::fuzz::convert(data, &root.path().join("out"));
    let outside: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name != "out")
        .collect();
    assert!(outside.is_empty(), "{outside:?}");
});
