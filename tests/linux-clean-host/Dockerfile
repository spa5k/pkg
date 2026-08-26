FROM debian:bookworm-slim

ENV container=docker
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends \
        binutils ca-certificates curl dbus file openssl passwd procps python3 sudo systemd systemd-sysv \
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

COPY publication-1 /srv/pkg-releases/1
COPY publication-2 /srv/pkg-releases/2
COPY install.sh /usr/local/sbin/pkg-bootstrap
COPY pkg-proof-server.py /usr/local/libexec/pkg-proof-server.py
COPY pkg-proof-release.service /etc/systemd/system/pkg-proof-release.service
COPY test-binaries/pkg-installer-lib-tests /usr/local/libexec/pkg-installer-lib-tests
RUN ln -s /srv/pkg-releases/1 /srv/pkg-release \
    && systemctl enable pkg-proof-release.service

STOPSIGNAL SIGRTMIN+3
CMD ["/sbin/init"]
