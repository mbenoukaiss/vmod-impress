#!/bin/sh

set -ex

OUT="out"
VMODTOOL="$(pkg-config  --variable=vmodtool varnishapi)"

cd /build
cargo build --release
cargo test --release

mkdir -p "$OUT"
cp target/release/libvmod_shrink.so "$OUT/libvmod_shrink.so"
#rst2man shrink.man.rst > "$OUT/shrink.3"
"$VMODTOOL" vmod.vcc -w "$OUT" --output /tmp/tmp_file_to_delete
rm /tmp/tmp_file_to_delete.*
cp out/libvmod_shrink.so /usr/lib/varnish/vmods

if [ -f /var/run/varnish ]; then
    kill -15 $(cat /var/run/varnish)
    sleep 1
fi

varnishd \
	  -a :80 \
	  -p feature=+http2 \
	  -f /etc/varnish/default.vcl \
	  -s malloc,512m \
	  -P /var/run/varnish
