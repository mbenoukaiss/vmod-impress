FROM rust:1.77-slim-bookworm

ARG USER_ID
ARG GROUP_ID

ENV DEBIAN_FRONTEND=noninteractive

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        ca-certificates curl gnupg apt-transport-https debian-archive-keyring; \
    install -d -m 0755 /etc/apt/keyrings; \
    curl -fsSL https://packagecloud.io/varnishcache/varnish75/gpgkey \
        | gpg --dearmor -o /etc/apt/keyrings/varnishcache-varnish75.gpg; \
    chmod 0644 /etc/apt/keyrings/varnishcache-varnish75.gpg; \
    echo 'deb [signed-by=/etc/apt/keyrings/varnishcache-varnish75.gpg] https://packagecloud.io/varnishcache/varnish75/debian bookworm main' \
        > /etc/apt/sources.list.d/varnishcache_varnish75.list; \
    printf 'Package: varnish varnish-*\nPin: release o=packagecloud.io/varnishcache/*\nPin-Priority: 1000\n' \
        > /etc/apt/preferences.d/varnishcache; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        varnish varnish-dev \
        build-essential \
        llvm clang \
        jq python3-docutils \
        meson ninja-build cmake nasm \
        pkg-config; \
    apt-get clean; \
    rm -rf /var/lib/apt/lists/*; \
    groupadd -f -g "$GROUP_ID" defaultgroup; \
    useradd defaultuser -u "$USER_ID" -g "$GROUP_ID" -m -s /bin/sh; \
    mkdir -p /var/lib/varnish/varnishd; \
    chmod -R 777 /usr/lib/varnish/vmods /var/lib/varnish/varnishd

ADD build.sh /build.sh
WORKDIR /build
USER defaultuser
ENTRYPOINT ["tail", "-f", "/dev/null"]
