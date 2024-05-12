varnish::boilerplate!();

#[macro_use]
extern crate log;

mod backend;
mod cache;
mod config;
mod images;
mod error;
mod macros;
mod utils;

use std::fs::File;
use std::io::Write;
use chrono::Local;
use env_logger::Target;
use varnish::vcl::ctx::Ctx;
use varnish::vcl::backend::{Backend, VCLBackendPtr};
use crate::error::Error;
use crate::backend::{FileBackend, FileTransfer};
use crate::cache::Cache;
use crate::config::Config;

#[allow(non_camel_case_types)]
struct root {
    backend: Backend<FileBackend, FileTransfer>,
}

impl root {
    pub fn new(ctx: &mut Ctx, vcl_name: &str, path: Option<&str>) -> Result<Self, Error> {
        let config = Config::parse(path)?;
        if let Some(log_path) = &config.log_path {
            setup_logging(log_path);
        }

        let cache = Cache::new(&config);
        let backend = FileBackend::new(config, cache);

        let backend = Backend::new(ctx, vcl_name, backend, false)?;

        Ok(root { backend })
    }

    pub fn backend(&self, _ctx: &Ctx) -> VCLBackendPtr {
        self.backend.vcl_ptr()
    }
}

fn setup_logging(log_path: &str) {
    let target = Box::new(File::create(log_path).unwrap());

    env_logger::Builder::new()
        .target(Target::Pipe(target))
        .format(|buf, record| {
            writeln!(
                buf,
                "[{} {} {}:{}] {}",
                Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                record.level(),
                record.file().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                record.args()
            )
        })
        .init();
}