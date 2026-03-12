#!/bin/sh

set -ex

OUT="out"

cd /build
cargo build --lib --release
cargo test --release

mkdir -p "$OUT"
cp target/release/libvmod_impress.so "$OUT/libvmod_impress.so"
cp out/libvmod_impress.so /usr/lib/varnish/vmods

if [ -f /tmp/varnish ]; then
    kill -15 $(cat /tmp/varnish) || true
    sleep 1
fi

varnishd \
	  -a :80 \
	  -p feature=+http2 \
	  -f /etc/varnish/default.vcl \
	  -s malloc,512m \
	  -P /tmp/varnish

varnishlog
