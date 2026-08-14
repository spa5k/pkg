FROM rust:1.96-bookworm AS builder

ARG PKG_SYSTEM
WORKDIR /workspace
COPY . .

RUN --mount=type=cache,id=pkg-linux-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=pkg-linux-target,target=/target \
    CARGO_TARGET_DIR=/target cargo run --locked --release \
        -p pkg-release --example linux_proof_publication -- \
        --prepare /proof-signing
RUN --mount=type=cache,id=pkg-linux-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=pkg-linux-target,target=/target \
    PKG_RELEASE_TUF_ROOT_JSON="$(cat /proof-signing/root.json)" \
    PKG_RELEASE_CHANNEL_METADATA_URL=https://127.0.0.1:8443/metadata/ \
    PKG_RELEASE_CHANNEL_TARGETS_URL=https://127.0.0.1:8443/targets/ \
    CARGO_TARGET_DIR=/target cargo build --locked --release \
        -p pkg-cli --bin pkg \
        -p pkg-installer --bin pkg-nix-broker \
        -p pkg-installer --bin pkg-root-helper \
        -p pkg-installer --bin pkg-install \
    && mkdir /binaries \
    && cp /target/release/pkg /target/release/pkg-nix-broker \
        /target/release/pkg-root-helper /target/release/pkg-install /binaries/
RUN curl -fL --retry 3 \
    --output /tmp/nix.tar.xz \
    "https://releases.nixos.org/nix/nix-2.34.8/nix-2.34.8-${PKG_SYSTEM}.tar.xz" \
    && case "${PKG_SYSTEM}" in \
        aarch64-linux) expected=b4e565fe4db5c352547b8146488cab81db06be89301a4c67b081c76f1e457760 ;; \
        x86_64-linux) expected=2c2e146b80834fe0ca201b51deeb939405b4f18e8d2071bf80b10f8123c50464 ;; \
        *) exit 1 ;; \
    esac \
    && printf '%s  %s\n' "${expected}" /tmp/nix.tar.xz | sha256sum --check --strict
RUN --mount=type=cache,id=pkg-linux-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=pkg-linux-target,target=/target \
    CARGO_TARGET_DIR=/target cargo run --locked --release \
        -p pkg-release --example linux_proof_publication -- \
        /publication /tmp/nix.tar.xz /binaries "${PKG_SYSTEM}" /proof-signing

FROM debian:bookworm-slim

ENV container=docker
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends \
        ca-certificates curl dbus openssl passwd procps python3 systemd systemd-sysv \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/sh proof-user \
    && mkdir -p /etc/pkg-proof /usr/local/libexec \
    && openssl req -x509 -newkey rsa:2048 -nodes -days 30 \
        -subj /CN=pkg-linux-proof-ca \
        -addext basicConstraints=critical,CA:TRUE \
        -addext keyUsage=critical,keyCertSign,cRLSign \
        -keyout /etc/pkg-proof/ca.key \
        -out /usr/local/share/ca-certificates/pkg-linux-proof.crt \
    && openssl req -newkey rsa:2048 -nodes \
        -subj /CN=pkg-linux-proof \
        -keyout /etc/pkg-proof/server.key \
        -out /etc/pkg-proof/server.csr \
    && printf '%s\n' \
        'basicConstraints=critical,CA:FALSE' \
        'keyUsage=critical,digitalSignature,keyEncipherment' \
        'extendedKeyUsage=serverAuth' \
        'subjectAltName=IP:127.0.0.1' \
        > /etc/pkg-proof/server.ext \
    && openssl x509 -req -days 30 \
        -in /etc/pkg-proof/server.csr \
        -CA /usr/local/share/ca-certificates/pkg-linux-proof.crt \
        -CAkey /etc/pkg-proof/ca.key \
        -CAcreateserial \
        -extfile /etc/pkg-proof/server.ext \
        -out /etc/pkg-proof/server.crt \
    && update-ca-certificates

COPY --from=builder /publication /srv/pkg-release
COPY --from=builder /binaries/pkg-install /usr/local/sbin/pkg-install
COPY tests/linux-clean-host/pkg-proof-server.py /usr/local/libexec/pkg-proof-server.py
COPY tests/linux-clean-host/pkg-proof-release.service /etc/systemd/system/pkg-proof-release.service
RUN systemctl enable pkg-proof-release.service

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
