# Shrink
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
```sheel
docker exec shrink /build.sh
```

## Todo
- Add support for AVIF and JPEG
- Support fetching images from another backend ?
- Tests
