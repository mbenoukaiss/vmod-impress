# Impress
Optimizes images by resizing, compressing and converting them to the
best format supported by the client

## Running the project
First start the container which contains varnish and all the necessary
tools to build the plugin :
```shell
docker compose up -d
```

Each time a change is made either to the rust code or to the varnish
configuration file and you want to recompile the plugin, run this command :
```shell
docker exec vmod-impress /build.sh
```

Running this will also copy the compiled plugin and documentations into the
`out` directory.

## Setting up the plugin
Minimal varnish configuration to use the plugin :
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

Create the `/etc/varnish/impress.ron` configuration file, the following is an example
configuration file, see below for more details :
```ron
Config(
    extensions: [AVIF, WEBP],
    default_format: JPEG,
    qualities: {WEBP: 70, AVIF: 40},
    root: "/var/www/media",
    url: "/media/{size}/{path}.{ext}",
    cache_directory: "/var/cache/varnish",
    sizes: {
        "low": Size(width: 300, height: 300, qualities: {WEBP: 90, JPEG: 100}),
        "medium": Size(width: 600, height: 600),
        "high": Size(width: 1200, height: 1200),
        "product": Size(width: 546, height: 302, pattern: "^products/", pre_optimize: true),
    },
    logger: Logger(
        path: "/var/log/impress.log",
        level: WARN
    ),
)
```

## Configuration

### Config
The `Config` struct has the following fields :
- `extensions` : List of supported image formats, the order in the array defines the 
priority, currently only `webp` and `avif` are supported
- `default_format` : Default image format to use when the client does not support 
any of the supported formats. Currently ignored and images do not get optimized when 
falling back to this format, the original image format will be served
- `qualities` : Quality when compressing images the default value is `{AVIF: 40, WEBP: 70, JPEG: 90}`. 
Can be overriden in the size configuration
- `root` : Root directory where images are stored
- `url` : URL pattern to match and extract the image size, path and extension from
- `cache_directory` : Directory to store the optimized and resized images
- `sizes` : Map of image sizes and their configurations, see below
- `logger` : Logger configuration, leave empty to disable

### Sizes
You can add multiple sizes to the `sizes` map, each size has the following fields :
- `width` : Maximum width to resize the image to
- `height` : Maximum height to resize the image to
- `qualities` : Quality when compressing images the default value is `{AVIF: 40, WEBP: 70, JPEG: 90}`. 
Overrides the qualities specified in the `Config` object
- `pattern` : Regex pattern to match the `{path}` variable in the URL pattern, if 
the path does not match a 404 will be returned
- `pre_optimize` : If set to true, a thread will be spawned to optimize all the 
matching images to this format. It is recommanded to also set a pattern if not 
all images will be served in this format to avoid generating a lot of useless files

### Logger
Configures the logger, leave empty to deactivate the logger
- `path` : Log file path
- `level` : Minimum level of log, levels below will be filtered out

## Todo
- Add support for AVIF and JPEG
- Support fetching images from another backend ?
- Tests
- Don't start pre optimizer thread if no image is set to pre optimize
