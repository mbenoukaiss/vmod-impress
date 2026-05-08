use std::collections::HashMap;
use std::sync::Arc;

use varnish::vcl::Backend;

use crate::backend::FileBackend;
use crate::config::{Config, Extension, Logger, Optimization, Size, StaticRoute};
use crate::error::Error;
use crate::static_files::Transfer;

/// Mutable accumulator that mirrors the file-loaded `Config` shape but
/// builds up across many VCL calls (`add_root`, `add_extension`, `add_size`,
/// …). `into_config` produces a fully-finalized `Config` ready for `Cache`
/// + `FileBackend` construction.
pub struct BuilderState {
    pub url: String,
    pub cache_directory: String,
    pub default_format: Extension,
    pub cache_control: Option<String>,
    pub pre_optimizer_threads: Option<usize>,
    pub global_qualities: HashMap<Extension, f32>,
    pub roots: Vec<String>,
    pub extensions: Vec<Extension>,
    pub sizes: HashMap<String, Size>,
    pub statics: Vec<StaticRoute>,
    pub logger: Option<Logger>,
}

impl BuilderState {
    pub fn new(url: String, cache_directory: String) -> Self {
        BuilderState {
            url,
            cache_directory,
            default_format: Extension::JPEG,
            cache_control: None,
            pre_optimizer_threads: None,
            global_qualities: HashMap::new(),
            roots: Vec::new(),
            extensions: Vec::new(),
            sizes: HashMap::new(),
            statics: Vec::new(),
            logger: None,
        }
    }

    pub fn into_config(self) -> Result<Config, Error> {
        if self.roots.is_empty() {
            return Error::err("at least one root is required (call add_root)");
        }
        if self.extensions.is_empty() {
            return Error::err("at least one extension is required (call add_extension)");
        }
        if self.sizes.is_empty() {
            return Error::err("at least one size is required (call add_size)");
        }

        let quality_serialized = if self.global_qualities.is_empty() {
            None
        } else {
            Some(self.global_qualities)
        };

        let mut config = Config {
            extensions: self.extensions,
            default_format: self.default_format,
            roots: self.roots,
            url: self.url,
            cache_directory: self.cache_directory,
            pre_optimizer_threads: self.pre_optimizer_threads,
            sizes: self.sizes,
            logger: self.logger,
            cache_control: self.cache_control,
            url_regex: None,
            cache_control_value: Arc::from(""),
            cache_control_fallback: Arc::from(""),
            quality_serialized,
            statics: self.statics,
        };

        config.finalize()?;
        Ok(config)
    }
}

/// Engine lifecycle. Mutators are valid only in `Building`; `build()`
/// transitions to `Built` (or `Failed` if construction errored). `backend()`
/// is only meaningful in `Built`.
pub enum EngineState {
    Building(Box<BuilderState>),
    Built(Backend<FileBackend, Transfer>),
    Failed,
}

impl EngineState {
    /// Borrow the inner `BuilderState` for mutator methods. Returns a
    /// human-friendly error if the engine has already been built or its
    /// previous build attempt failed.
    pub fn as_building_mut(&mut self) -> Result<&mut BuilderState, &'static str> {
        match self {
            EngineState::Building(b) => Ok(b.as_mut()),
            EngineState::Built(_) => Err("engine already built; call mutators before build()"),
            EngineState::Failed => Err("engine.build() failed; reload VCL to retry"),
        }
    }
}

/// Build a `Size` from VCL-side scalars. `quality_*` values of `None` mean
/// "inherit from the engine-level qualities at finalize time"; the `[f32; 3]`
/// array is filled by `Config::finalize`.
pub fn make_size(
    width: u32,
    height: u32,
    quality_jpeg: Option<f32>,
    quality_webp: Option<f32>,
    quality_avif: Option<f32>,
    pattern: Option<String>,
    pre_optimize: Option<bool>,
) -> Size {
    let mut quality_serialized: HashMap<Extension, f32> = HashMap::new();
    if let Some(q) = quality_jpeg {
        quality_serialized.insert(Extension::JPEG, q);
    }
    if let Some(q) = quality_webp {
        quality_serialized.insert(Extension::WEBP, q);
    }
    if let Some(q) = quality_avif {
        quality_serialized.insert(Extension::AVIF, q);
    }
    let quality_serialized = if quality_serialized.is_empty() {
        None
    } else {
        Some(quality_serialized)
    };

    Size {
        width,
        height,
        quality: [0.0; 3],
        pattern,
        pre_optimize,
        pattern_regex: None,
        quality_serialized,
    }
}

/// Build a `StaticRoute` with builder-style optional toggles. Values left
/// `None` use the same defaults the file loaders apply.
#[allow(clippy::too_many_arguments)]
pub fn make_static_route(
    url: String,
    root: String,
    cache_control: Option<String>,
    optimize_html: Option<bool>,
    optimize_css: Option<bool>,
    optimize_js: Option<bool>,
    optimize_json: Option<bool>,
    optimize_max_bytes: Option<usize>,
) -> StaticRoute {
    let defaults = Optimization::default();
    let optimization = Optimization {
        html: optimize_html.unwrap_or(defaults.html),
        css: optimize_css.unwrap_or(defaults.css),
        js: optimize_js.unwrap_or(defaults.js),
        json: optimize_json.unwrap_or(defaults.json),
    };

    StaticRoute {
        url,
        root,
        cache_control,
        optimization,
        optimize_max_bytes,
        url_regex: None,
        root_canon: None,
        cache_control_value: Arc::from(""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh_builder() -> BuilderState {
        BuilderState::new(
            "/media/{size}/{path}[.{ext}]".to_string(),
            "/tmp/impress-test".to_string(),
        )
    }

    #[test]
    fn happy_path_minimal_config() {
        let mut b = fresh_builder();
        b.roots.push("/tmp".to_string());
        b.extensions.push(Extension::JPEG);
        b.sizes.insert(
            "default".to_string(),
            make_size(100, 100, None, None, None, None, None),
        );

        let cfg = b.into_config().expect("happy path should build");
        assert_eq!(cfg.extensions, vec![Extension::JPEG]);
        assert_eq!(cfg.roots, vec!["/tmp".to_string()]);
        assert!(cfg.url_regex.is_some());
        assert!(cfg.sizes.contains_key("default"));
        //extension default applied since neither builder global nor per-size override is set
        assert_eq!(
            cfg.sizes["default"].quality[Extension::JPEG as usize],
            Extension::JPEG.default_quality()
        );
    }

    #[test]
    fn missing_roots_errors() {
        let mut b = fresh_builder();
        b.extensions.push(Extension::JPEG);
        b.sizes.insert(
            "default".to_string(),
            make_size(100, 100, None, None, None, None, None),
        );
        assert!(b.into_config().is_err());
    }

    #[test]
    fn missing_extensions_errors() {
        let mut b = fresh_builder();
        b.roots.push("/tmp".to_string());
        b.sizes.insert(
            "default".to_string(),
            make_size(100, 100, None, None, None, None, None),
        );
        assert!(b.into_config().is_err());
    }

    #[test]
    fn missing_sizes_errors() {
        let mut b = fresh_builder();
        b.roots.push("/tmp".to_string());
        b.extensions.push(Extension::JPEG);
        assert!(b.into_config().is_err());
    }

    #[test]
    fn duplicate_size_replaces() {
        let mut b = fresh_builder();
        b.roots.push("/tmp".to_string());
        b.extensions.push(Extension::JPEG);
        b.sizes.insert(
            "low".to_string(),
            make_size(100, 100, None, None, None, None, None),
        );
        b.sizes.insert(
            "low".to_string(),
            make_size(200, 200, None, None, None, None, None),
        );
        let cfg = b.into_config().expect("build");
        assert_eq!(cfg.sizes["low"].width, 200);
        assert_eq!(cfg.sizes["low"].height, 200);
    }

    #[test]
    fn quality_inheritance_from_global() {
        let mut b = fresh_builder();
        b.roots.push("/tmp".to_string());
        b.extensions.push(Extension::WEBP);
        b.global_qualities.insert(Extension::WEBP, 88.0);
        b.global_qualities.insert(Extension::JPEG, 77.0);
        b.sizes.insert(
            "no_override".to_string(),
            make_size(100, 100, None, None, None, None, None),
        );
        b.sizes.insert(
            "with_override".to_string(),
            make_size(100, 100, None, Some(95.0), None, None, None),
        );

        let cfg = b.into_config().expect("build");
        //inherited from builder global
        assert_eq!(
            cfg.sizes["no_override"].quality[Extension::WEBP as usize],
            88.0
        );
        assert_eq!(
            cfg.sizes["no_override"].quality[Extension::JPEG as usize],
            77.0
        );
        //per-size override wins
        assert_eq!(
            cfg.sizes["with_override"].quality[Extension::WEBP as usize],
            95.0
        );
    }

    #[test]
    fn static_route_optimize_max_bytes_zero_disables_cap() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();

        let mut b = fresh_builder();
        b.roots.push("/tmp".to_string());
        b.extensions.push(Extension::JPEG);
        b.sizes.insert(
            "default".to_string(),
            make_size(100, 100, None, None, None, None, None),
        );
        b.statics.push(make_static_route(
            "/assets/{path}".to_string(),
            root,
            None,
            None,
            None,
            None,
            None,
            Some(0),
        ));

        let cfg = b.into_config().expect("build");
        assert!(cfg.statics[0].allows_optimization_at_size(usize::MAX));
        assert!(cfg.statics[0].allows_optimization_at_size(0));
    }

    #[test]
    fn static_route_inherits_global_cache_control() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();

        let mut b = fresh_builder();
        b.cache_control = Some("public, max-age=42".to_string());
        b.roots.push("/tmp".to_string());
        b.extensions.push(Extension::JPEG);
        b.sizes.insert(
            "default".to_string(),
            make_size(100, 100, None, None, None, None, None),
        );
        b.statics.push(make_static_route(
            "/assets/{path}".to_string(),
            root,
            None,
            None,
            None,
            None,
            None,
            None,
        ));

        let cfg = b.into_config().expect("build");
        assert_eq!(&*cfg.cache_control_value, "public, max-age=42");
        assert_eq!(&*cfg.statics[0].cache_control_value, "public, max-age=42");
    }
}
