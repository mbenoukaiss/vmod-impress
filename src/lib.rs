varnish::boilerplate!();

mod backend;
mod config;
mod error;
mod images;
mod macros;
mod utils;

use varnish::vcl::ctx::Ctx;
use varnish::vcl::backend::{Backend, VCLBackendPtr};
use crate::error::Error;
use crate::backend::{FileBackend, FileTransfer};
use crate::config::Config;
use crate::images::Cache;

#[allow(non_camel_case_types)]
struct root {
    backend: Backend<FileBackend, FileTransfer>,
}

impl root {
    pub fn new(ctx: &mut Ctx, vcl_name: &str, path: Option<&str>) -> Result<Self, Error> {
        let config = Config::parse(path)?;
        let cache = Cache::new(&config);
        let backend = FileBackend::new(config, cache);


        //todo: load images and create a thread that reads them on modification
        let backend = Backend::new(ctx, vcl_name, backend, false)?;

        Ok(root { backend })
    }

    pub fn backend(&self, _ctx: &Ctx) -> VCLBackendPtr {
        self.backend.vcl_ptr()
    }
}
