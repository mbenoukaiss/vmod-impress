varnish::boilerplate!();

#[macro_use]
extern crate log;

#[macro_use]
mod macros;

mod backend;
mod cache;
mod config;
mod images;
mod error;
mod utils;

use log4rs::append::file::FileAppender;
use log4rs::config::{Appender,Config as LogConfig, Root};
use log4rs::encode::pattern::PatternEncoder;
use log::LevelFilter;
use varnish::vcl::ctx::Ctx;
use varnish::vcl::backend::{Backend, VCLBackendPtr};
use crate::error::Error;
use crate::backend::{FileBackend, FileTransfer};
use crate::cache::Cache;
use crate::config::{Config, Logger as LoggerConfig};

#[allow(non_camel_case_types)]
type new = Impress;

struct Impress {
    backend: Backend<FileBackend, FileTransfer>,
}

impl Impress {
    pub fn new(ctx: &mut Ctx, vcl_name: &str, path: Option<&str>) -> Result<Self, Error> {
        let config = Config::parse(path)?;
        if let Some(logger) = &config.logger {
            setup_logging(logger);
        }

        let cache = Cache::new(&config);
        let backend = FileBackend::new(config, cache);

        let backend = Backend::new(ctx, vcl_name, backend, false)?;

        Ok(Impress { backend })
    }

    pub fn backend(&self, _ctx: &Ctx) -> VCLBackendPtr {
        self.backend.vcl_ptr()
    }
}

fn setup_logging(logger_config: &LoggerConfig) {
    let file = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new("{d(%Y-%m-%d %H:%M:%S)} | {({l}):5.5} | {f}:{L} — {m}{n}")))
        .append(true)
        .build(&logger_config.path)
        .unwrap();

    let config = LogConfig::builder()
        .appender(Appender::builder().build("file_ap", Box::new(file)))
        .build(Root::builder().appender("file_ap").build(logger_config.level.unwrap_or(LevelFilter::Info)))
        .unwrap();

    log4rs::init_config(config).unwrap();
}
