vcl 4.1;

import shrink;

backend default none;

sub vcl_init {
    new www = shrink.root("/etc/varnish/shrink.ron");
}

sub vcl_recv {
    set req.backend_hint = www.backend();
}
