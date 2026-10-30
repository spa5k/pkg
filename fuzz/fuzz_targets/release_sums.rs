//! Only a well-formed checksum line names a release, and the version and the
//! archive name are safe in URLs and paths.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|sums: &str| {
    for system in ["x86_64-linux", "aarch64-darwin"] {
        let Some((version, archive, checksum)) = pkg_cli::fuzz::release_sums(sums, system) else {
            continue;
        };
        assert_eq!(archive, format!("pkg-{version}-{system}.tar.gz"));
        assert!(checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(
            version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
                && !version.contains(".."),
            "{version:?}"
        );
        assert!(
            pkg_cli::fuzz::newer(&version, "0.2.0-alpha.18").is_ok(),
            "{version:?}"
        );
    }
});
