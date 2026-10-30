//! Tap archive unpacking must not panic. It writes only plain files and
//! folders, and only inside its destination.
#![no_main]

use std::io::Write as _;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // A first byte of 0 marks a raw tar stream, which this target compresses.
    let bytes = match data.split_first() {
        Some((0, tar)) => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(tar).unwrap();
            encoder.finish().unwrap()
        }
        _ => data.to_vec(),
    };
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("tree");
    let result = cask_catalog::tap::fuzz::extract(&bytes, &tree);
    let outside: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name != "tree")
        .collect();
    assert!(outside.is_empty(), "{outside:?}");
    if result.is_ok() {
        let mut folders = vec![tree.clone()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                assert!(kind.is_file() || kind.is_dir(), "{:?}", entry.path());
                if kind.is_dir() {
                    folders.push(entry.path());
                }
            }
        }
        let _ = cask_catalog::tap::fuzz::inventory(&tree);
    }
});
