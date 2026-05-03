#[macro_use]
extern crate log;

#[macro_use]
mod macros;

mod backend;
mod cache;
mod config;
mod images;
mod error;
mod static_files;
mod utils;
mod vfp;

use log4rs::append::file::FileAppender;
use log4rs::config::{Appender, Config as LogConfig, Root};
use log4rs::encode::pattern::PatternEncoder;
use log::LevelFilter;
use varnish::vcl::Backend;
use crate::error::Error;
use crate::backend::FileBackend;
use crate::config::Logger as LoggerConfig;
use crate::static_files::Transfer;

#[allow(non_camel_case_types)]
struct new {
    backend: Backend<FileBackend, Transfer>,
}

#[varnish::vmod]
mod impress {
    use std::sync::Arc;
    use varnish::ffi::VCL_BACKEND;
    use varnish::vcl::{Backend, Ctx, Event, FetchFilters, VclError};

    use super::{new, setup_logging, FileBackend};
    use crate::cache::Cache;
    use crate::config::Config;
    use crate::vfp::MinifyVfp;

    /// Register/deregister our VFP for the lifetime of each VCL load. Without
    /// the matching `Discard` arm, repeated VCL reloads would leak the filter
    /// registration and could double-register on the next load.
    #[event]
    pub fn on_event(event: Event, vfp: &mut FetchFilters) -> Result<(), VclError> {
        match event {
            Event::Load => {
                vfp.register::<MinifyVfp>();
            }
            Event::Discard => {
                vfp.unregister::<MinifyVfp>();
            }
            _ => {}
        }
        Ok(())
    }

    //varnish::vmod's struct-as-class pattern names the constructor after the
    //type, which trips clippy::self_named_constructors. Documented invariant
    //of the macro — the VCL surface is `new(...)` and renaming the impl
    //method would break that.
    #[allow(clippy::self_named_constructors)]
    impl new {
        pub fn new(
            ctx: &mut Ctx,
            #[vcl_name] vcl_name: &str,
            path: Option<&str>,
        ) -> Result<Self, VclError> {
            let config = Arc::new(Config::open(path).map_err(|e| VclError::new(e.to_string()))?);
            if let Some(logger) = &config.logger {
                if let Err(e) = setup_logging(logger) {
                    eprintln!("vmod-impress: failed to initialize logger ({}): continuing without file logging", e);
                }
            }

            let cache = Cache::new(config.clone());
            let backend = FileBackend::new(config, cache);
            let backend = Backend::new(ctx, "impress", vcl_name, backend, false)?;

            Ok(super::new { backend })
        }

        pub unsafe fn backend(&self) -> VCL_BACKEND {
            self.backend.vcl_ptr()
        }
    }
}

fn setup_logging(logger_config: &LoggerConfig) -> Result<(), Error> {
    let file = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new("{d(%Y-%m-%d %H:%M:%S)} | {({l}):5.5} | {f}:{L} — {m}{n}")))
        .append(true)
        .build(&logger_config.path)?;

    let config = LogConfig::builder()
        .appender(Appender::builder().build("file_ap", Box::new(file)))
        .build(Root::builder().appender("file_ap").build(logger_config.level.unwrap_or(LevelFilter::Info)))?;

    log4rs::init_config(config)?;
    Ok(())
}
