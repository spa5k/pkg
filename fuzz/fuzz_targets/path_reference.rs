//! Every path survives a `path:` reference round trip, and decoding never
//! panics.
#![no_main]

use std::os::unix::ffi::OsStrExt as _;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(data));
    let encoded = pkg_cli::nix::encode_path_reference(path);
    let decoded = pkg_cli::nix::decode_path_reference(&encoded);
    assert_eq!(
        decoded.as_ref().map(|path| path.as_os_str().as_bytes()),
        Some(data)
    );
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = pkg_cli::nix::decode_path_reference(text);
        let _ = pkg_cli::nix::is_fixed_reference(text);
    }
});
