set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# Show the available commands.
default:
    @just --list

# Fast local gate with the same warning policy as CI.
lint:
    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets --all-features -- -D warnings

# Complete Rust checks for all workspace crates.
check: lint
    cargo build --locked --workspace --all-targets --all-features
    cargo test --locked --workspace --all-features
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps --document-private-items

# Package the standalone client archive for this host.
# system: x86_64-linux or aarch64-darwin (must match this host).
package system:
    bash tools/release/package_client.sh {{ system }}
