set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

# Show the available commands.
default:
    @just --list

# Fast local gate: format plus the workspace deny lints.
lint:
    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets --all-features

# Package the standalone client archive for this host.
# system: x86_64-linux or aarch64-darwin (must match this host).
package system:
    bash tools/release/package_client.sh {{ system }}
