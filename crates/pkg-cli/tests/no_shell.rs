//! Shell strings are denied in product code.
//!
//! Every external process is spawned through an argument vector, so the
//! quoting, globbing, and equals-expansion bug classes cannot exist. This
//! check keeps it that way: product Rust sources must not name a shell
//! program in a Command::new call or use sh -c, and the Python cask
//! helpers must not enable shell mode. Test code is exempt; tests may
//! legitimately drive real shells.

// This integration-test crate is built without `cfg(test)`, so the
// crate-root test opt-out does not apply; the same reasoned exception
// is declared here once.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests abort on broken fixtures; production never panics"
)]

use std::fs;
use std::path::{Path, PathBuf};

/// Shell programs product code must never spawn directly.
const SHELLS: [&str; 8] = ["sh", "zsh", "bash", "dash", "ksh", "fish", "csh", "tcsh"];

/// The scanned workspace roots: the client, the catalog tool, and the
/// cask build helpers.
fn roots() -> Vec<PathBuf> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    ["crates", "tools/cask-catalog", "nix/casks/lib"]
        .into_iter()
        .map(|dir| workspace.join(dir))
        .collect()
}

/// Collect files with one extension, skipping build output and tests.
fn collect(path: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(name.as_ref(), "target" | ".git" | "tests") {
            continue;
        }
        if child.is_dir() {
            collect(&child, extension, out);
        } else if name.ends_with(extension) {
            out.push(child);
        }
    }
}

/// The product part of one source file: inline test modules are cut away.
fn product_text(file: &Path) -> String {
    let text = fs::read_to_string(file).expect("read source");
    match text.find("#[cfg(test)]") {
        Some(index) => text[..index].to_string(),
        None => text,
    }
}

/// Every way product code could name a shell spawn in one Rust source.
fn rust_offenders(file: &Path, text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for shell in SHELLS {
        for spelled in [shell, format!("/bin/{shell}").as_str()] {
            let needle = format!("Command::new({spelled:?})");
            if text.contains(&needle) {
                found.push(format!("{} names {needle}", file.display()));
            }
        }
    }
    let shell_string = ["sh", "-c"].join(" ");
    if text.contains(&shell_string) {
        found.push(format!("{} uses {shell_string}", file.display()));
    }
    found
}

#[test]
fn product_sources_never_spawn_a_shell() {
    let mut rust_files = Vec::new();
    let mut python_files = Vec::new();
    for root in roots() {
        collect(&root, ".rs", &mut rust_files);
        collect(&root, ".py", &mut python_files);
    }
    assert!(
        !rust_files.is_empty(),
        "the lint must find the Rust sources"
    );
    assert!(!python_files.is_empty(), "the lint must find the helpers");
    let mut offenders = Vec::new();
    for file in &rust_files {
        offenders.extend(rust_offenders(file, &product_text(file)));
    }
    for file in &python_files {
        let text = product_text(file);
        for needle in ["shell=True", "os.system("] {
            if text.contains(needle) {
                offenders.push(format!("{} uses {needle}", file.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "spawn processes through argument vectors only: {}",
        offenders.join("; ")
    );
}
