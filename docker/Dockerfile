FROM rust:1.77-slim-bookworm

RUN apt update && \
    apt-get install debian-archive-keyring curl gnupg apt-transport-https -y && \
    curl -s -L https://packagecloud.io/varnishcache/varnish75/gpgkey | apt-key add - && \
    . /etc/os-release && \
     echo 'deb https://packagecloud.io/varnishcache/varnish75/debian bookworm main' > /etc/apt/sources.list.d/varnish.list && \
     echo 'Package: varnish varnish-* \
    Pin: release o=packagecloud.io/varnishcache/* \
    Pin-Priority: 1000' > /etc/apt/preferences.d/varnishcache && \
    apt update && \
    apt install varnish varnish-dev llvm jq python3-docutils clang -y && \
    apt-get clean && rm -rf /var/lib/apt/lists/*

ADD build.sh /build.sh
WORKDIR /build
ENTRYPOINT ["tail", "-f", "/dev/null"]
