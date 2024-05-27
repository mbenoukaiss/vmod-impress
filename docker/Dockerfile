#rustlang/rust:nightly-bookworm-slim
FROM rust:1.77-slim-bookworm

ARG USER_ID
ARG GROUP_ID

RUN apt update && \
    apt install debian-archive-keyring curl gnupg apt-transport-https -y && \
    curl -s -L https://packagecloud.io/varnishcache/varnish75/gpgkey | apt-key add - && \
    . /etc/os-release && \
     echo 'deb https://packagecloud.io/varnishcache/varnish75/debian bookworm main' > /etc/apt/sources.list.d/varnish.list && \
     echo 'Package: varnish varnish-* \
    Pin: release o=packagecloud.io/varnishcache/* \
    Pin-Priority: 1000' > /etc/apt/preferences.d/varnishcache && \
    apt update && \
    apt install varnish varnish-dev llvm jq python3-docutils clang meson ninja-build cmake nasm -y && \
    apt clean && rm -rf /var/lib/apt/lists/* && \
    groupadd -f -g $GROUP_ID defaultgroup && \
    useradd defaultuser -u $USER_ID -g $GROUP_ID -m -s /bin/sh && \
    mkdir /var/lib/varnish/varnishd && \
    chmod -R 777 /usr/lib/varnish/vmods /var/lib/varnish/varnishd

ADD build.sh /build.sh
WORKDIR /build
USER defaultuser
ENTRYPOINT ["tail", "-f", "/dev/null"]
