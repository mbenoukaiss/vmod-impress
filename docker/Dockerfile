FROM rust:1.95-slim-bookworm AS base

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
    mkdir -p /var/lib/varnish/varnishd; \
    chmod -R 777 /usr/lib/varnish/vmods /var/lib/varnish/varnishd

RUN rustup component add rustfmt clippy


FROM base AS ci-builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && : > src/lib.rs

RUN cargo build --lib --release --locked

RUN rm -rf src Cargo.toml Cargo.lock \
    && tar -czf /target-cache.tar.gz -C /build target


FROM base AS ci

COPY --from=ci-builder /target-cache.tar.gz /target-cache.tar.gz


FROM base AS dev

ARG USER_ID
ARG GROUP_ID

RUN set -eux; \
    groupadd -f -g "$GROUP_ID" defaultgroup; \
    useradd defaultuser -u "$USER_ID" -g "$GROUP_ID" -m -s /bin/sh

COPY docker/build.sh /build.sh
WORKDIR /build
USER defaultuser
ENTRYPOINT ["tail", "-f", "/dev/null"]
