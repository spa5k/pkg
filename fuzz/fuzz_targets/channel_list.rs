//! The NixOS channel list parser must not panic, and it stores only text
//! without terminal controls and only plain command names.
#![no_main]

use libfuzzer_sys::fuzz_target;

fn shown(text: &str) -> bool {
    !text.chars().any(|c| {
        c.is_control()
            || matches!(
                c,
                '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
    })
}

/// A Brotli stream that holds `data` in uncompressed meta-blocks (RFC 7932).
fn stored(data: &[u8]) -> Vec<u8> {
    fn flush(out: &mut Vec<u8>, mut bits: u32, mut count: u32) {
        while count > 0 {
            out.push(bits as u8);
            bits >>= 8;
            count = count.saturating_sub(8);
        }
    }
    let mut out = Vec::new();
    // The stream header: one 0 bit for a 64 KiB window.
    let mut count = 1;
    for chunk in data.chunks(1 << 16) {
        // Not last, 4 length nibbles, the length - 1, uncompressed.
        flush(
            &mut out,
            ((chunk.len() as u32 - 1) << 3 | 1 << 19) << count,
            count + 20,
        );
        out.extend_from_slice(chunk);
        count = 0;
    }
    // The last meta-block, empty.
    flush(&mut out, 0b11 << count, count + 2);
    out
}

fuzz_target!(|data: &[u8]| {
    // A first byte of 0 marks JSON text, which this target wraps in Brotli.
    let bytes = match data.split_first() {
        Some((0, json)) => stored(json),
        _ => data.to_vec(),
    };
    let path = std::env::temp_dir().join(format!("pkg-fuzz-list-{}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    for system in ["x86_64-linux", "aarch64-darwin"] {
        let Ok(rows) = pkg_cli::fuzz::read_list(&path, system) else {
            continue;
        };
        for (name, pname, version, description, command) in rows {
            for text in [&name, &pname, &version, &description] {
                assert!(shown(text), "{text:?}");
            }
            if let Some(command) = command {
                assert!(
                    !command.is_empty() && !command.contains('/') && shown(&command),
                    "{command:?}"
                );
            }
        }
    }
});
