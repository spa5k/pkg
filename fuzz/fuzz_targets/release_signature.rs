//! The release signature check must not panic on any input.
#![no_main]

use libfuzzer_sys::fuzz_target;

/// The test key of `client.rs`, so a seed with a valid signature reaches the
/// signature check.
const TEST_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIFwE3tikzJoifU8bnO83SgImfBJcBidTNvWYVcJAGJIc";

fuzz_target!(|data: &[u8]| {
    // The message is before the first zero byte, the signature after it.
    let (message, armored) = match data.iter().position(|&byte| byte == 0) {
        Some(split) => (&data[..split], &data[split + 1..]),
        None => (data, &[][..]),
    };
    let armored = String::from_utf8_lossy(armored);
    let _ = pkg_cli::fuzz::release_signature(message, &armored, &[TEST_KEY]);
});
