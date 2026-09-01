set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

smoke_image := "pkg-linux-smoke:local"
smoke_container := "pkg-linux-smoke"
smoke_platform := "linux/amd64"

# Show the available commands.
default:
    @just --list

# Fast local gate: format plus the workspace deny lints.
lint:
    cargo fmt --all -- --check
    cargo clippy --locked --workspace --all-targets --all-features

# Strict quality gate: strict complexity budgets plus the debt ratchet.
quality:
    tools/quality/quality-gate.sh check

# Strictest gate: every file changed against BASE_REF must be debt-free.
# BASE_REF defaults to origin/main; export it for stacked branches, e.g.
# `BASE_REF=origin/dn/16-determinate-cutover just lint-strict`.
lint-strict:
    FULL_TOUCHED=1 tools/quality/quality-gate.sh check

# Record the current debt as the new baseline after deliberate paydown.
ratchet-rebase:
    tools/quality/quality-gate.sh rebase

# Build the clean x86-64 Linux image.
vm-build:
    docker build --platform {{ smoke_platform }} --file tests/linux-public-smoke/Dockerfile --tag {{ smoke_image }} .

# Create and start the Linux test container.
vm-create: vm-build
    @if ! docker container inspect {{ smoke_container }} >/dev/null 2>&1; then docker run --detach --privileged --platform {{ smoke_platform }} --cgroupns=private --name {{ smoke_container }} --tmpfs /run --tmpfs /run/lock {{ smoke_image }} >/dev/null; fi
    @just vm-start

# Start or resume the Linux test container.
vm-start:
    @docker container inspect {{ smoke_container }} >/dev/null 2>&1 || { echo "The container does not exist. Run 'just vm-create'." >&2; exit 1; }
    @docker start {{ smoke_container }} >/dev/null
    @for attempt in {1..30}; do state=$(docker exec {{ smoke_container }} systemctl is-system-running 2>/dev/null || true); case "$state" in running|degraded) echo "{{ smoke_container }} is $state."; exit 0;; esac; sleep 1; done; docker logs {{ smoke_container }}; echo "The container did not start." >&2; exit 1

# Open a shell as the normal tester user.
vm-shell: vm-start
    docker exec -it --user tester --workdir /home/tester {{ smoke_container }} bash -l

# Stop the Linux test container.
vm-stop:
    @if docker container inspect {{ smoke_container }} >/dev/null 2>&1; then docker stop {{ smoke_container }} >/dev/null; fi
    @echo "{{ smoke_container }} is stopped."

# Show the Linux test container status.
vm-status:
    @docker ps --all --filter "name=^/{{ smoke_container }}$"

# Remove the Linux test container and its anonymous volumes.
vm-remove:
    @if docker container inspect {{ smoke_container }} >/dev/null 2>&1; then docker rm --force --volumes {{ smoke_container }} >/dev/null; fi
    @echo "{{ smoke_container }} is removed."
