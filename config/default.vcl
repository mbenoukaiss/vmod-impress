vcl 4.1;

import shrink;

backend default none;

sub vcl_init {
    new images = shrink.root("/etc/varnish/shrink.ron");
}

sub vcl_recv {
    set req.backend_hint = images.backend();

    #disable cache while testing
    return (pass);
}
