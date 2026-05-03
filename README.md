# Impress
A Varnish module that resizes, compresses and converts images on the fly to
the best format the requesting client supports (AVIF, WebP, JPEG), and that
also serves general static files (HTML, CSS, JS, JSON, fonts, raw images,
…) with per-extension in-process minification on cache miss. Optimized
image variants are written to a local cache directory; static-file output
is held in memory only as long as Varnish needs it, then re-derived on the
next miss. Both paths emit proper HTTP cache validators (`ETag`,
`Last-Modified`, `stale-while-revalidate`) so Varnish itself can
revalidate cheaply.

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
    cache_control: "public, max-age=86400, stale-while-revalidate=604800",
    static: [
        StaticRoute(
            url: "/assets/{path}",
            root: "/var/www/static",
            optimization: Optimization(js: true),
        ),
    ],
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
| `cache_control` | Optional. Raw `Cache-Control` header value used on optimized image responses and static-file responses. Defaults to `"public, max-age=86400, stale-while-revalidate=604800"`. The image in-flight fallback (raw bytes served while optimization is running) is hardcoded to `"no-cache"` and is not configurable — caching it would pin the un-optimized variant at the HTTP layer until the next mtime. |
| `static` | Optional. List of `StaticRoute`s for serving general static assets alongside images, see below. |
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

### Disk cleanup

Orphan cleanup runs unconditionally — there is no `cleanup` config block:

- A startup sweep walks `cache_directory` once after `load_images` and
  removes any cache file whose source no longer exists or whose
  size/extension is no longer in the current configuration.
- A periodic sweep thread runs the same logic every 24 hours so
  long-running varnishd instances reclaim disk space without a restart.

The cleaner removes only **orphans** — cache files for which the source has
been deleted, or that don't fit the current `sizes` / `extensions`
configuration. There is no LRU or size-cap eviction; the source filesystem
is the source of truth for what should exist in the cache.

### `StaticRoute` (optional, repeatable)

Each entry serves files from `root` under the URL pattern `url`. Routes
evaluate top-down; the first route whose URL pattern matches owns the
response — even if the file is missing or the path-traversal guard
trips, the route does not fall through to a later route or to the image
regex. Place narrow routes before broad ones.

| Field | Description |
|---|---|
| `url` | URL pattern. Must contain `{path}`. `[...]` brackets denote optional segments. Example: `/assets/{path}`. |
| `root` | Filesystem root the route serves from. Canonicalized once at parse time. Requests are joined under this root and rejected if the resolved path escapes (parent-dir, absolute, or symlink to outside-root). |
| `cache_control` | Optional raw `Cache-Control` header value. Falls back to `Config.cache_control` when unset. |
| `optimization` | Optional. Per-extension toggles, see below. |
| `optimize_max_bytes` | Optional. Files larger than this skip optimization and stream from disk regardless of toggles — keeps a single big file from OOMing a worker. Default `2_097_152` (2 MiB). `Some(0)` removes the cap. |

#### `Optimization`

Per-extension toggles. Defaults run minify-html, lightningcss, serde_json
on HTML / CSS / JSON. JS minification (`oxc_minifier`) defaults **off**
because that crate is alpha and has shipped semantically-broken outputs
on edge cases — opt in only after batch-testing against your asset corpus.

| Field | Default | Optimizer used |
|---|---|---|
| `html` | `true` | minify-html (also runs lightningcss / oxc on inline `<style>` / `<script>`) |
| `css` | `true` | lightningcss |
| `js` | `false` | oxc_minifier (alpha — opt in deliberately) |
| `json` | `true` | serde_json (parse + compact-serialize) |

SVG is served unoptimized — `oxvg_optimiser` pins a `lightningcss` feature
that conflicts with the version `minify-html` requires, so the two crates
can't coexist in this dep graph.

When optimization runs, output bytes ship via an in-memory `MemoryTransfer`.
On no-improvement (output not strictly smaller than the input) we still
ship the in-heap bytes rather than re-opening the file — that avoids the
read-after-rename TOCTOU window. Files past `optimize_max_bytes` and files
with optimization toggled off stream directly from disk via `FileTransfer`.

ETag for static responses is hashed from
`(inode, body_len, mtime_secs, mime, is_optimized, STATIC_OPTIMIZER_VERSION)`.
The version constant is bumped manually after a minifier crate upgrade
that changes output bytes for unchanged sources, so clients holding an
old `If-None-Match` get a real 200 with the new bytes rather than a stale
304.

### `Logger` (optional)

| Field | Description |
|---|---|
| `path` | Log file path. |
| `level` | Minimum level of log; entries below are dropped. Default `INFO`. |

## On-the-fly minifier (`impress_minify`)

The VMOD registers a Varnish Fetch Processor named `impress_minify` that
buffers a backend response, minifies it, and stores the minified bytes in
the cache. Subsequent cache hits don't run the filter.

Wire it up in `vcl_backend_response`:

```vcl
import impress;

sub vcl_backend_response {
    if (beresp.http.content-type ~ "^(text/(html|css|javascript)|application/(json|javascript))") {
        # Gunzip the backend body so the minifier sees plaintext; Varnish
        # re-gzips on the way to storage/clients.
        set beresp.do_gunzip = true;
        # Insert impress_minify *before* the trailing gzip filter — appending
        # would feed gzipped bytes into the minifier and silently no-op.
        set beresp.filters = regsuball(beresp.filters, "(^| )gzip( |$)", "\1impress_minify gzip\2");
        if (beresp.filters !~ "impress_minify") {
            set beresp.filters = beresp.filters + " impress_minify";
        }
    }
}
```

Supported types: `text/html`, `text/css`, `text/javascript`,
`application/javascript`, `application/json`.

The filter only engages on cacheable responses. First request to a fresh
URL pays a TTFB hit equal to the backend response time plus a few ms of
minify; cache hits are unaffected. Pair with
`Cache-Control: stale-while-revalidate=N` to amortize refreshes.

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
