//! Reading the client from a release archive must not panic, and it returns
//! only a non-empty client of at most 256 MiB.
#![no_main]

use std::io::Write as _;

use libfuzzer_sys::fuzz_target;

const MEMBER: &str = "pkg-1.0.0-x86_64-linux/bin/pkg";

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
    let path = std::env::temp_dir().join(format!("pkg-fuzz-archive-{}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    if let Ok(client) = pkg_cli::fuzz::release_archive(&path, MEMBER) {
        assert!(!client.is_empty() && client.len() <= 256 << 20);
    }
});
