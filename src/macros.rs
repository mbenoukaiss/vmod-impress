#[macro_export]
macro_rules! respond {
    ($ctx:ident, $status:expr) => {
        $ctx.http_beresp.as_mut().unwrap().set_status($status);
        return Ok(None);
    };
}

#[macro_export]
macro_rules! debug_file {
    ($name:expr, $data:expr) => {
        ::std::fs::create_dir_all("/build/debug").unwrap();
        ::std::fs::write(format!("/build/debug/{}.txt", (&$name).to_string().replace("/", "_")), format!("{:#?}", $data)).unwrap();
    };
}

#[macro_export]
macro_rules! debug_header {
    ($beresp:ident, $name:expr, $message:expr) => {
        $beresp.set_header($name, &format!("{:#?}", $message).replace("\n", " "))?;
    };
    (abort: $beresp:ident, $name:expr, $message:expr) => {
        debug_header!($beresp, $name, $message);
        return Ok(None);
    };
}
