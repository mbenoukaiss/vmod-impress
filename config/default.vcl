vcl 4.1;

import impress;

backend default none;

sub vcl_init {
    new images = impress.new("/etc/varnish/impress.ron");
}

sub vcl_recv {
    set req.backend_hint = images.backend();

    #disable cache while testing
    return (pass);
}
