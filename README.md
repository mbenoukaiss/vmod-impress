# Impress
A Varnish module that resizes, compresses and converts images on the fly to
the best format the requesting client supports (AVIF, WebP, JPEG). Optimized
variants are written to a local cache directory and served with proper HTTP
cache validators (`ETag`, `Last-Modified`, `stale-while-revalidate`) so
Varnish itself can revalidate cheaply.

## Setting up the plugin

Download [`libvmod_impress.so`](https://github.com/mbenoukaiss/vmod-impress/releases/latest/download/libvmod_impress.so)
from the latest release and place it in your Varnish vmods directory
(typically `/usr/lib/varnish/vmods`).

Minimal VCL:

```vcl
vcl 4.1;

import impress;

backend default none;

sub vcl_init {
    new images = impress.new("/etc/varnish/impress.ron");
}

sub vcl_recv {
    set req.backend_hint = images.backend();
}
```

Configure Varnish to vary on / sanitize the `Accept` header for image URLs;
otherwise different clients may get the wrong format from a single cache key.

Create `/etc/varnish/impress.ron` (see below for the full schema):

```ron
Config(
    extensions: [AVIF, WEBP],
    default_format: JPEG,
    qualities: {WEBP: 70, AVIF: 40},
    roots: ["/var/www/media"],
    url: "/media/{size}/{path}[.{ext}]",
    cache_directory: "/var/cache/varnish",
    sizes: {
        "low":     Size(width: 300,  height: 300, qualities: {WEBP: 90, JPEG: 100}),
        "medium":  Size(width: 600,  height: 600),
        "high":    Size(width: 1200, height: 1200),
        "product": Size(width: 546,  height: 302, pattern: "^products/", pre_optimize: true),
    },
    cache_control: CacheControl(
        optimized_max_age_seconds: 86400,
        optimized_stale_while_revalidate_seconds: 604800,
    ),
    cleanup: Cleanup(
        interval_seconds: 86400,
    ),
    logger: Logger(
        path: "/var/log/impress.log",
        level: WARN,
    ),
)
```

## Configuration

### `Config`

| Field | Description |
|---|---|
| `extensions` | List of optimized formats to produce, in priority order. Currently `AVIF`, `WEBP`, `JPEG`. |
| `default_format` | Format served when the client's `Accept` header doesn't match any of `extensions`. |
| `qualities` | Per-format encoder quality used by every `Size` unless overridden. Defaults: `{AVIF: 40, WEBP: 70, JPEG: 90}`. |
| `roots` | List of source-image directories. Multiple roots supported; paths in URLs are matched against the first root that contains the requested file. |
| `url` | URL pattern with `{size}`, `{path}`, optional `{ext}`, and `[...]` for optional segments — see the URL pattern section. |
| `cache_directory` | Directory where optimized variants are written. Layout is `<cache_directory>/<size>/<image_id>.<ext>`. |
| `pre_optimizer_threads` | Threads in the optimization pool. Default `1`. |
| `sizes` | Map of named sizes, see below. |
| `cleanup` | Optional. Disk-cleanup policy, see below. Defaults are sensible; omitting the section keeps the previous behavior of "no periodic sweep" but still does a startup sweep. |
| `cache_control` | Optional. Tune the `Cache-Control` header sent on responses, see below. |
| `logger` | Optional file logger. Omit to disable file logging. |

### URL pattern (`url`)

The `url` field describes how to extract `size`, `path`, and optionally `ext` from the request URL.

- `{size}` (required) — matched against the keys of `sizes`.
- `{path}` (required) — image identifier, used to locate the source under one of `roots`.
- `{ext}` (optional) — explicit extension; usually wrapped in `[...]` so it can be omitted.
- `[...]` — wraps a portion of the URL that is optional.

Examples:

```
url: "/media/{size}/{path}[.{ext}]"
   matches /media/medium/products/logo
       and /media/medium/products/logo.jpg
```

### `Size`

| Field | Description |
|---|---|
| `width` | Maximum width of the resized variant. |
| `height` | Maximum height of the resized variant. |
| `qualities` | Optional. Per-format quality, overrides `Config.qualities`. |
| `pattern` | Optional regex that must match `{path}` for this size to apply. If absent, all paths match. |
| `pre_optimize` | Optional. If `true`, every matching image is optimized into all configured `extensions` at startup and on watcher modification, instead of lazily on first request. Recommended only with a `pattern` so you don't pre-optimize the whole tree. |

### `Cleanup` (optional)

| Field | Default | Description |
|---|---|---|
| `interval_seconds` | `86400` | How often the periodic orphan-sweep thread runs. |
| `orphan_sweep_on_startup` | `true` | Whether to walk `cache_directory` once at startup and remove any cache file whose source no longer exists or whose size/extension is no longer configured. |

The cleaner removes only **orphans** — cache files for which the source has
been deleted, or that don't fit the current `sizes` / `extensions`
configuration. There is no LRU or size-cap eviction; the source filesystem
is the source of truth for what should exist in the cache.

If the `cleanup` block is omitted entirely, only the startup sweep runs (no
periodic thread). If you set `cleanup: Cleanup()` (empty), both sweeps run
with the defaults above.

### `CacheControl` (optional)

Controls the `Cache-Control` header sent on responses. Defaults are tuned
for stale-while-revalidate behavior — Varnish reads `stale-while-revalidate=N`
from the response and uses it as `beresp.grace`, so it serves stale content
to clients while firing a cheap background revalidation through this VMOD's
304-aware backend.

| Field | Default | Description |
|---|---|---|
| `optimized_max_age_seconds` | `86400` | `max-age` on optimized variants (cache hits) |
| `optimized_stale_while_revalidate_seconds` | `604800` | `stale-while-revalidate` window on optimized variants |
| `fallback_max_age_seconds` | `60` | `max-age` on the un-optimized fallback (returned while a job is in-flight) |
| `fallback_stale_while_revalidate_seconds` | `3600` | `stale-while-revalidate` on the fallback |

### `Logger` (optional)

| Field | Description |
|---|---|
| `path` | Log file path. |
| `level` | Minimum level of log; entries below are dropped. Default `INFO`. |

## Cache freshness model

The on-disk optimized cache is kept in sync with the source images via three
layers, no periodic disk-walking required:

1. **Live invalidation** — a `notify`-based file watcher tracks every
   configured root and re-optimizes / removes cached variants on source
   modify and delete events.
2. **Startup reconcile** — at VMOD load time, `load_images` walks the source
   roots, compares each cached variant's `mtime` to the source's `mtime`,
   and removes any cache file older than its source. Catches changes that
   happened during downtime or that the watcher missed.
3. **Lazy HTTP-level revalidation** — responses carry
   `Cache-Control: public, max-age=N, stale-while-revalidate=M`. After
   `max-age` expires, Varnish keeps serving stale to clients while firing
   an asynchronous background fetch through this VMOD; the backend computes
   an `ETag` from `(inode, size, mtime, is_optimized)` and returns 304 if
   the disk is unchanged or 200 with the new bytes if the watcher updated
   the file.

For explicit invalidation (e.g. after a bulk source upload that you don't
want to wait `max-age` for), wire a VCL `BAN` handler:

```vcl
import std;

acl purge {
    "localhost";
    "127.0.0.1";
}

sub vcl_recv {
    if (req.method == "BAN") {
        if (!client.ip ~ purge) {
            return (synth(403, "Forbidden"));
        }
        if (!std.ban("req.url ~ " + req.url)) {
            return (synth(400, std.ban_error()));
        }
        return (synth(200, "Ban added"));
    }
}
```

This is purely Varnish configuration; the VMOD doesn't need code for it.

## Running the project

Start the dev container:

```shell
docker compose up -d
```

After every Rust or Varnish-config change, rebuild and reload:

```shell
docker exec vmod-impress /build.sh
```

The script compiles the cdylib, runs the test suite, copies the resulting
`.so` into Varnish's vmods directory, restarts varnishd, and tails the log.
