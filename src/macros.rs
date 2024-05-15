#[macro_export]
macro_rules! respond {
    ($ctx:ident, $status:expr) => {
        $ctx.http_beresp.as_mut().unwrap().set_status($status);
        return Ok(None);
    };
}

#[macro_export]
macro_rules! debug_header {
    ($beresp:ident, $name:expr, $message:expr) => {
        $beresp.set_header($name, $message)?;
    };
    (abort: $beresp:expr, $name:expr, $message:expr) => {
        $beresp.set_header($name, $message)?;
        return Ok(None);
    };
}
