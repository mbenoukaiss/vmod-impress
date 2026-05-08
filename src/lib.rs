#[macro_use]
extern crate log;

#[macro_use]
mod macros;

mod backend;
mod builder;
mod cache;
mod config;
mod error;
mod images;
mod static_files;
mod utils;
mod vfp;

use crate::backend::FileBackend;
use crate::builder::EngineState;
use crate::config::Logger as LoggerConfig;
use crate::error::Error;
use crate::static_files::Transfer;
use log::LevelFilter;
use log4rs::append::file::FileAppender;
use log4rs::config::{Appender, Config as LogConfig, Root};
use log4rs::encode::pattern::PatternEncoder;
use parking_lot::Mutex;
use varnish::vcl::Backend;

#[allow(non_camel_case_types)]
struct new {
    backend: Backend<FileBackend, Transfer>,
}

#[allow(non_camel_case_types)]
struct engine {
    state: Mutex<EngineState>,
    vcl_name: String,
}

#[varnish::vmod]
mod impress {
    use log::LevelFilter;
    use parking_lot::Mutex;
    use std::str::FromStr;
    use std::sync::Arc;
    use varnish::ffi::VCL_BACKEND;
    use varnish::vcl::{Backend, Ctx, Event, FetchFilters, VclError};

    use super::{engine, new, setup_logging, FileBackend};
    use crate::builder::{make_size, make_static_route, BuilderState, EngineState};
    use crate::cache::Cache;
    use crate::config::{Config, Extension, Logger as LoggerConfig};
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
            path: &str,
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

    #[allow(clippy::self_named_constructors)]
    impl engine {
        #[allow(clippy::too_many_arguments)]
        pub fn new(
            _ctx: &mut Ctx,
            #[vcl_name] vcl_name: &str,
            url: &str,
            cache_directory: &str,
            default_format: Option<&str>,
            quality_jpeg: Option<f64>,
            quality_webp: Option<f64>,
            quality_avif: Option<f64>,
            cache_control: Option<&str>,
            pre_optimizer_threads: Option<i64>,
        ) -> Result<Self, VclError> {
            let mut builder = BuilderState::new(url.to_string(), cache_directory.to_string());

            if let Some(fmt) = default_format {
                builder.default_format = Extension::from_ext(fmt).ok_or_else(|| {
                    VclError::new(format!(
                        "unknown default_format `{fmt}` (use jpeg, webp, or avif)"
                    ))
                })?;
            }

            if let Some(q) = quality_jpeg {
                builder.global_qualities.insert(Extension::JPEG, q as f32);
            }
            if let Some(q) = quality_webp {
                builder.global_qualities.insert(Extension::WEBP, q as f32);
            }
            if let Some(q) = quality_avif {
                builder.global_qualities.insert(Extension::AVIF, q as f32);
            }

            if let Some(cc) = cache_control {
                builder.cache_control = Some(cc.to_string());
            }
            if let Some(n) = pre_optimizer_threads {
                if n < 0 {
                    return Err(VclError::new(
                        "pre_optimizer_threads must be >= 0".to_string(),
                    ));
                }
                builder.pre_optimizer_threads = Some(n as usize);
            }

            Ok(super::engine {
                state: Mutex::new(EngineState::Building(Box::new(builder))),
                vcl_name: vcl_name.to_string(),
            })
        }

        pub fn add_root(&self, path: &str) -> Result<(), VclError> {
            let mut state = self.state.lock();
            let builder = state
                .as_building_mut()
                .map_err(|e| VclError::new(e.to_string()))?;
            builder.roots.push(path.to_string());
            Ok(())
        }

        pub fn add_extension(&self, format: &str) -> Result<(), VclError> {
            let ext = Extension::from_ext(format).ok_or_else(|| {
                VclError::new(format!(
                    "unknown extension `{format}` (use jpeg, webp, or avif)"
                ))
            })?;
            let mut state = self.state.lock();
            let builder = state
                .as_building_mut()
                .map_err(|e| VclError::new(e.to_string()))?;
            builder.extensions.push(ext);
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        pub fn add_size(
            &self,
            name: &str,
            width: i64,
            height: i64,
            quality_jpeg: Option<f64>,
            quality_webp: Option<f64>,
            quality_avif: Option<f64>,
            pattern: Option<&str>,
            pre_optimize: Option<bool>,
        ) -> Result<(), VclError> {
            if width <= 0 || height <= 0 {
                return Err(VclError::new(format!(
                    "size `{name}`: width and height must be positive"
                )));
            }
            let size = make_size(
                width as u32,
                height as u32,
                quality_jpeg.map(|q| q as f32),
                quality_webp.map(|q| q as f32),
                quality_avif.map(|q| q as f32),
                pattern.map(|s| s.to_string()),
                pre_optimize,
            );

            let mut state = self.state.lock();
            let builder = state
                .as_building_mut()
                .map_err(|e| VclError::new(e.to_string()))?;
            builder.sizes.insert(name.to_string(), size);
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        pub fn add_static(
            &self,
            url: &str,
            root: &str,
            cache_control: Option<&str>,
            optimize_html: Option<bool>,
            optimize_css: Option<bool>,
            optimize_js: Option<bool>,
            optimize_json: Option<bool>,
            optimize_max_bytes: Option<i64>,
        ) -> Result<(), VclError> {
            let max_bytes = match optimize_max_bytes {
                Some(n) if n < 0 => {
                    return Err(VclError::new(
                        "optimize_max_bytes must be >= 0 (use 0 to disable the cap)".to_string(),
                    ))
                }
                Some(n) => Some(n as usize),
                None => None,
            };
            let route = make_static_route(
                url.to_string(),
                root.to_string(),
                cache_control.map(|s| s.to_string()),
                optimize_html,
                optimize_css,
                optimize_js,
                optimize_json,
                max_bytes,
            );

            let mut state = self.state.lock();
            let builder = state
                .as_building_mut()
                .map_err(|e| VclError::new(e.to_string()))?;
            builder.statics.push(route);
            Ok(())
        }

        pub fn set_logger(&self, path: &str, level: Option<&str>) -> Result<(), VclError> {
            let level = match level {
                Some(s) => Some(LevelFilter::from_str(s).map_err(|_| {
                    VclError::new(format!(
                        "invalid log level `{s}` (use off, error, warn, info, debug, trace)"
                    ))
                })?),
                None => None,
            };
            let mut state = self.state.lock();
            let builder = state
                .as_building_mut()
                .map_err(|e| VclError::new(e.to_string()))?;
            builder.logger = Some(LoggerConfig {
                path: path.to_string(),
                level,
            });
            Ok(())
        }

        pub fn build(&self, ctx: &mut Ctx) -> Result<(), VclError> {
            let mut state = self.state.lock();
            //Take the builder out by replacing the slot with Failed first.
            //If construction errors, the engine is left in Failed and any
            //subsequent backend()/mutator call returns a clear error.
            let prev = std::mem::replace(&mut *state, EngineState::Failed);
            let builder = match prev {
                EngineState::Building(b) => b,
                EngineState::Built(b) => {
                    *state = EngineState::Built(b);
                    return Err(VclError::new("engine.build() already called".to_string()));
                }
                EngineState::Failed => {
                    return Err(VclError::new(
                        "engine.build() previously failed; reload VCL".to_string(),
                    ));
                }
            };

            let config = builder
                .into_config()
                .map_err(|e| VclError::new(e.to_string()))?;
            let config = Arc::new(config);

            if let Some(logger) = &config.logger {
                if let Err(e) = setup_logging(logger) {
                    eprintln!("vmod-impress: failed to initialize logger ({e}): continuing without file logging");
                }
            }

            let cache = Cache::new(config.clone());
            let backend = FileBackend::new(config, cache);
            let backend = Backend::new(ctx, "impress", &self.vcl_name, backend, false)?;

            *state = EngineState::Built(backend);
            Ok(())
        }

        pub unsafe fn backend(&self) -> VCL_BACKEND {
            let state = self.state.lock();
            match &*state {
                EngineState::Built(b) => b.vcl_ptr(),
                EngineState::Building(_) => {
                    log::warn!("vmod-impress: engine.backend() called before engine.build(); did vcl_init forget to call build()?");
                    VCL_BACKEND::default()
                }
                EngineState::Failed => {
                    log::warn!("vmod-impress: engine.backend() called after engine.build() failed");
                    VCL_BACKEND::default()
                }
            }
        }
    }
}

fn setup_logging(logger_config: &LoggerConfig) -> Result<(), Error> {
    let file = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new(
            "{d(%Y-%m-%d %H:%M:%S)} | {({l}):5.5} | {f}:{L} — {m}{n}",
        )))
        .append(true)
        .build(&logger_config.path)?;

    let config = LogConfig::builder()
        .appender(Appender::builder().build("file_ap", Box::new(file)))
        .build(
            Root::builder()
                .appender("file_ap")
                .build(logger_config.level.unwrap_or(LevelFilter::Info)),
        )?;

    log4rs::init_config(config)?;
    Ok(())
}
