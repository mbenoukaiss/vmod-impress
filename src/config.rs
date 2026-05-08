use crate::error::Error;
use crate::images::OptimizationConfig;
use image::ImageFormat;
use log::LevelFilter;
use mediatype::names::{AVIF, IMAGE, JPEG, WEBP};
use mediatype::MediaType;
use regex::Regex;
use ron::extensions::Extensions;
use ron::Options;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

pub type SharedConfig = Arc<Config>;

#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    pub extensions: Vec<Extension>,
    pub default_format: Extension,
    pub roots: Vec<String>,
    pub url: String,
    pub cache_directory: String,
    pub pre_optimizer_threads: Option<usize>,
    pub sizes: HashMap<String, Size>,
    pub logger: Option<Logger>,
    /// Cache-Control header value sent on optimized responses (image cache
    /// hits and static-file responses). Defaults to
    /// `"public, max-age=86400, stale-while-revalidate=604800"`. The image
    /// in-flight fallback (raw source served while the optimizer is running)
    /// always uses `"no-cache"` and is not user-tunable — caching the raw
    /// bytes for any non-trivial duration would pin the un-optimized variant
    /// at the HTTP layer until the next mtime change.
    pub cache_control: Option<String>,

    #[serde(skip_deserializing)]
    pub url_regex: Option<Regex>,
    //resolved once at parse time as Arc<str> so the hot path can clone
    //refcounts into FetchResult without allocating
    #[serde(skip_deserializing)]
    pub cache_control_value: Arc<str>,
    #[serde(skip_deserializing)]
    pub cache_control_fallback: Arc<str>,

    #[serde(rename = "qualities")]
    pub quality_serialized: Option<HashMap<Extension, f32>>,

    #[serde(default, rename = "static")]
    pub statics: Vec<StaticRoute>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct StaticRoute {
    pub url: String,
    pub root: String,
    /// Per-route Cache-Control override. When unset, the route uses the
    /// global `Config.cache_control` (which itself falls back to the
    /// built-in default). Static routes have no in-flight fallback notion,
    /// so this single value is the only Cache-Control they ever emit.
    pub cache_control: Option<String>,
    #[serde(default)]
    pub optimization: Optimization,
    /// Skip optimization for files larger than this many bytes; the source
    /// is streamed from disk instead. Default 2 MiB. `Some(0)` removes the
    /// cap and runs the optimizer regardless of size — use with care, the
    /// optimizers all need the whole document resident in heap.
    pub optimize_max_bytes: Option<usize>,

    #[serde(skip_deserializing)]
    pub url_regex: Option<Regex>,
    #[serde(skip_deserializing)]
    pub root_canon: Option<PathBuf>,
    #[serde(skip_deserializing)]
    pub cache_control_value: Arc<str>,
}

impl StaticRoute {
    /// Returns true when a file of `len` bytes should be run through the
    /// optimizer (i.e. is at or below the configured cap). Encapsulates
    /// the `Some(0) == disabled cap` sentinel.
    pub fn allows_optimization_at_size(&self, len: usize) -> bool {
        match self.optimize_max_bytes {
            Some(0) => true,
            Some(cap) => len <= cap,
            None => len <= 2 * 1024 * 1024,
        }
    }
}

/// Default Cache-Control for optimized responses. Long max-age plus a long
/// stale-while-revalidate window so Varnish serves stale to clients while
/// firing a cheap background revalidation through the 304-aware backend.
pub(crate) const DEFAULT_CACHE_CONTROL: &str =
    "public, max-age=86400, stale-while-revalidate=604800";

/// Image in-flight fallback header. Hardcoded — anything cacheable would
/// pin the un-optimized variant at the HTTP layer until the next mtime.
pub(crate) const FALLBACK_CACHE_CONTROL: &str = "no-cache";

/// Compile a URL template into a regex.
///
/// `{name}` placeholders are mapped to named regex captures via `subs`
/// (escaped form: `\{name\}` → replacement). `[...]` segments become
/// optional groups via the bracket-rewrite. `required` is the list of
/// raw placeholders (`{name}`) that must appear in `url`; missing any
/// produces a parse error. Bracket pairs must balance after the rewrite.
fn compile_url_template(
    url: &str,
    subs: &[(&str, &str)],
    required: &[&str],
) -> Result<Regex, Error> {
    for placeholder in required {
        if !url.contains(placeholder) {
            return Error::err(format!(
                "Argument {} is required in URL pattern",
                placeholder,
            ));
        }
    }
    let mut clean = format!(r"^{}$", regex::escape(url));
    for (from, to) in subs {
        clean = clean.replace(from, to);
    }
    clean = clean.replace(r"\[", "(").replace(r"\]", ")?");
    if clean.chars().filter(|c| *c == '(').count() != clean.chars().filter(|c| *c == ')').count() {
        return Error::err("Invalid URL pattern in config file");
    }
    Ok(Regex::new(&clean)?)
}

/// Per-extension toggles. JS minification defaults to FALSE because oxc_minifier
/// is still alpha and has historically shipped semantically-broken outputs on
/// edge cases; opt in once you've batch-tested against your asset corpus.
/// Other optimizers (HTML, CSS, JSON) default ON.
#[derive(Deserialize, Clone, Debug)]
pub struct Optimization {
    #[serde(default = "default_true")]
    pub html: bool,
    #[serde(default = "default_true")]
    pub css: bool,
    #[serde(default = "default_false")]
    pub js: bool,
    #[serde(default = "default_true")]
    pub json: bool,
}

impl Default for Optimization {
    fn default() -> Self {
        Optimization {
            html: true,
            css: true,
            js: false,
            json: true,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}

#[derive(Deserialize, Clone, Debug)]
pub struct Size {
    pub width: u32,
    pub height: u32,
    #[serde(skip_deserializing)]
    pub quality: [f32; 3],
    pub pattern: Option<String>,
    pub pre_optimize: Option<bool>,

    #[serde(skip_deserializing)]
    pub pattern_regex: Option<Regex>,

    #[serde(rename = "qualities")]
    pub quality_serialized: Option<HashMap<Extension, f32>>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Logger {
    pub path: String,
    pub level: Option<LevelFilter>,
}

//Variant names are matched against ron config files; renaming to CamelCase
//would silently break every user's impress.ron, so the upper-case names stay.
#[allow(clippy::upper_case_acronyms)]
#[derive(Deserialize, Eq, PartialEq, Hash, Copy, Clone, Debug)]
#[repr(u8)]
pub enum Extension {
    JPEG,
    WEBP,
    AVIF,
}

impl Extension {
    pub fn values() -> [Extension; 3] {
        [Extension::JPEG, Extension::WEBP, Extension::AVIF]
    }

    pub fn to_media_type(self) -> MediaType<'static> {
        match self {
            Extension::AVIF => MediaType::new(IMAGE, AVIF),
            Extension::WEBP => MediaType::new(IMAGE, WEBP),
            Extension::JPEG => MediaType::new(IMAGE, JPEG),
        }
    }

    pub fn from_ext(value: &str) -> Option<Extension> {
        match value.to_lowercase().as_str() {
            "jpeg" | "jpg" => Some(Extension::JPEG),
            "webp" => Some(Extension::WEBP),
            "avif" => Some(Extension::AVIF),
            _ => None,
        }
    }

    pub fn default_quality(&self) -> f32 {
        match self {
            Extension::JPEG => 90.0, //TODO find value
            Extension::WEBP => 70.0,
            Extension::AVIF => 40.0,
        }
    }

    pub fn image_format(&self) -> ImageFormat {
        match self {
            Extension::JPEG => ImageFormat::Jpeg,
            Extension::WEBP => ImageFormat::WebP,
            Extension::AVIF => ImageFormat::Avif,
        }
    }

    pub fn extensions(&self) -> &'static [&'static str] {
        self.image_format().extensions_str()
    }

    pub fn mime_str(&self) -> &'static str {
        match self {
            Extension::JPEG => "image/jpeg",
            Extension::WEBP => "image/webp",
            Extension::AVIF => "image/avif",
        }
    }
}

impl Config {
    pub fn open(path: &str) -> Result<Config, Error> {
        let raw = fs::read_to_string(path)
            .map_err(|_| Error::new(format!("Unable to read config file {}", path)))?;

        match std::path::Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref()
        {
            Some("json") => Self::parse_json(raw),
            Some("ron") => Self::parse_ron(raw),
            Some(other) => Error::err(format!(
                "Unsupported config extension `.{other}` (use `.json` or `.ron`)",
            )),
            None => Error::err(format!(
                "Config path `{path}` has no extension; use `.json` or `.ron`",
            )),
        }
    }

    pub fn parse_ron(config: String) -> Result<Config, Error> {
        let mut config = Options::default()
            .with_default_extension(Extensions::IMPLICIT_SOME)
            .from_str::<Config>(&config)?;
        config.finalize()?;
        Ok(config)
    }

    pub fn parse_json(config: String) -> Result<Config, Error> {
        let mut config: Config = serde_json::from_str(&config)?;
        config.finalize()?;
        Ok(config)
    }

    /// Format-agnostic post-deserialization fixups: compile the URL regex,
    /// resolve per-size quality matrices from size→global→default, wrap
    /// cache-control strings in `Arc<str>`, and canonicalize static-route
    /// roots. Called by both `parse_ron` / `parse_json` and the VCL builder
    /// after assembling a `Config` directly.
    pub fn finalize(&mut self) -> Result<(), Error> {
        self.url_regex = Some(Self::build_url_regex(&self.url)?);

        for size in &mut self.sizes.values_mut() {
            for extension in Extension::values() {
                let size_quality = size
                    .quality_serialized
                    .as_ref()
                    .and_then(|q| q.get(&extension));
                let config_quality = self
                    .quality_serialized
                    .as_ref()
                    .and_then(|q| q.get(&extension));

                size.quality[extension as usize] = if let Some(quality) = size_quality {
                    *quality
                } else if let Some(quality) = config_quality {
                    *quality
                } else {
                    extension.default_quality()
                }
            }

            size.quality_serialized = None;

            if let Some(pattern) = &size.pattern {
                size.pattern_regex = Some(Regex::new(pattern)?)
            }
        }

        self.quality_serialized = None;

        let global_cc: Arc<str> = match self.cache_control.as_deref() {
            Some(s) => Arc::from(s),
            None => Arc::from(DEFAULT_CACHE_CONTROL),
        };
        self.cache_control_value = global_cc.clone();
        self.cache_control_fallback = Arc::from(FALLBACK_CACHE_CONTROL);

        for route in &mut self.statics {
            route.url_regex = Some(Self::build_static_url_regex(&route.url)?);
            let canon = std::fs::canonicalize(&route.root)
                .map_err(|e| Error::new(format!("static root {} unreadable: {}", route.root, e)))?;
            route.root_canon = Some(canon);
            //Per-route Cache-Control falls back to the global value when
            //unset. Stored as Arc<str> so the response path can clone
            //refcounts instead of allocating.
            route.cache_control_value = match route.cache_control.as_deref() {
                Some(s) => Arc::from(s),
                None => global_cc.clone(),
            };
        }

        Ok(())
    }

    fn build_static_url_regex(url: &str) -> Result<Regex, Error> {
        compile_url_template(url, &[(r"\{path\}", r"(?<path>.+?)")], &["{path}"])
    }

    fn build_url_regex(url: &str) -> Result<Regex, Error> {
        compile_url_template(
            url,
            &[
                (r"\{size\}", r"(?<size>\w+)"),
                (r"\{path\}", r"(?<path>.+?)"),
                (r"\{ext\}", r"(?<ext>[a-zA-Z0-9]+)"),
            ],
            &["{size}", "{path}"],
        )
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            extensions: vec![Extension::AVIF],
            default_format: Extension::JPEG,
            roots: vec![String::from("/dev/null")],
            url: String::from("/media"),
            cache_directory: String::from("/tmp/impress"),
            pre_optimizer_threads: None,
            sizes: HashMap::from([(
                String::from("default"),
                Size {
                    width: 500,
                    height: 500,
                    quality: [0.0; 3],
                    pattern: None,
                    pre_optimize: None,
                    pattern_regex: None,
                    quality_serialized: None,
                },
            )]),
            logger: None,
            cache_control: None,
            url_regex: None,
            cache_control_value: Arc::from(""),
            cache_control_fallback: Arc::from(""),
            quality_serialized: None,
            statics: Vec::new(),
        }
    }
}

impl Size {
    pub fn matches(&self, image: &str) -> bool {
        if let Some(pattern) = &self.pattern_regex {
            pattern.is_match(image)
        } else {
            true
        }
    }
}

impl OptimizationConfig {
    pub fn new(size: &Size, format: Extension, prefer_quality: bool) -> OptimizationConfig {
        let quality = size.quality[format as usize];

        match format {
            Extension::WEBP => OptimizationConfig::Webp {
                quality,
                prefer_quality,
            },
            Extension::AVIF => OptimizationConfig::Avif {
                quality,
                prefer_quality,
            },
            Extension::JPEG => OptimizationConfig::Jpeg {
                quality,
                prefer_quality,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_parse_valid_config() {
        let config_content = String::from(
            r#"
        (
            extensions: [AVIF, WEBP, JPEG],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{size}/{path}[.{ext}]",
            cache_directory: "/build/cache",
            sizes: {
                "low": Size(width: 300, height: 300),
                "medium": Size(width: 600, height: 600),
                "high": Size(width: 1200, height: 1200),
                "product": Size(width: 546, height: 302, pattern: "^products/", pre_optimize: true),
            },
            logger: Logger(
                path: "/build/debug/impress.log",
                level: WARN
            ),
        )
        "#,
        );

        let config = Config::parse_ron(config_content).expect("Failed to parse valid config");

        assert_eq!(
            config.extensions,
            vec![Extension::AVIF, Extension::WEBP, Extension::JPEG]
        );
        assert_eq!(config.default_format, Extension::JPEG);
        assert_eq!(config.roots, vec!["/build/media".to_string()]);
        assert_eq!(config.url, "/media/{size}/{path}[.{ext}]");
        assert_eq!(config.cache_directory, "/build/cache".to_string());
        assert!(config.sizes.contains_key("low"));
        assert!(config.sizes.contains_key("medium"));
        assert!(config.sizes.contains_key("high"));
        assert!(config.sizes.contains_key("product"));
        assert!(config.logger.is_some());
        assert!(config.url_regex.is_some());
    }

    #[test]
    fn test_parse_invalid_url_pattern() {
        let config_content = String::from(
            r#"
        (
            extensions: [AVIF, WEBP, JPEG],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{size}/{path}[.{ext}[",
            cache_directory: "/build/cache",
            sizes: {
                "low": Size(width: 300, height: 300),
                "medium": Size(width: 600, height: 600),
                "high": Size(width: 1200, height: 1200),
                "product": Size(width: 546, height: 302, pattern: "^products/", pre_optimize: true),
            },
            logger: Logger(
                path: "/build/debug/impress.log",
                level: WARN
            ),
        )
        "#,
        );

        let result = Config::parse_ron(config_content);
        assert!(result.is_err());
        if let Err(err) = result {
            assert_eq!(
                err.to_string(),
                "Invalid URL pattern in config file".to_string()
            );
        }
    }

    #[test]
    fn test_parse_default_quality_values() {
        let config_content = String::from(
            r#"
        (
            extensions: [AVIF, WEBP, JPEG],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{size}/{path}[.{ext}]",
            cache_directory: "/build/cache",
            sizes: {
                "low": Size(width: 300, height: 300),
                "medium": Size(width: 600, height: 600),
                "high": Size(width: 1200, height: 1200),
                "product": Size(width: 546, height: 302, pattern: "^products/", pre_optimize: true),
            },
            logger: Logger(
                path: "/build/debug/impress.log",
                level: WARN
            ),
        )
        "#,
        );

        let config = Config::parse_ron(config_content).expect("Failed to parse valid config");

        assert_eq!(
            config.sizes["low"].quality[Extension::JPEG as usize],
            Extension::JPEG.default_quality()
        );
        assert_eq!(
            config.sizes["medium"].quality[Extension::WEBP as usize],
            Extension::WEBP.default_quality()
        );
        assert_eq!(
            config.sizes["high"].quality[Extension::AVIF as usize],
            Extension::AVIF.default_quality()
        );
    }
    #[test]
    fn test_build_url_regex_valid_pattern() {
        let url = "/media/{size}/{path}[.{ext}]";
        let regex = Config::build_url_regex(url).expect("Failed to build regex");

        let url_to_test = "/media/medium/some/path/image.jpeg";
        let captures = regex.captures(url_to_test).expect("Failed to match URL");

        assert_eq!(captures.name("size").unwrap().as_str(), "medium");
        assert_eq!(captures.name("path").unwrap().as_str(), "some/path/image");
        assert_eq!(captures.name("ext").unwrap().as_str(), "jpeg");
    }

    #[test]
    fn test_build_url_regex_optional_extension() {
        let url = "/media/{size}/{path}[.{ext}]";
        let regex = Config::build_url_regex(url).expect("Failed to build regex");

        let url_to_test = "/media/high/another/path/image";
        let captures = regex.captures(url_to_test).expect("Failed to match URL");

        assert_eq!(captures.name("size").unwrap().as_str(), "high");
        assert_eq!(
            captures.name("path").unwrap().as_str(),
            "another/path/image"
        );
        assert!(captures.name("ext").is_none());
    }

    #[test]
    fn test_build_url_regex_invalid_pattern_unbalanced_brackets() {
        let url = "/media/{size}/{path}[.{ext}[";
        let result = Config::build_url_regex(url);

        assert!(result.is_err());
        if let Err(err) = result {
            assert_eq!(err.to_string(), "Invalid URL pattern in config file");
        }
    }

    #[test]
    fn test_build_url_regex_valid_pattern_no_optional_extension() {
        let url = "/media/{size}/{path}.{ext}";
        let regex = Config::build_url_regex(url).expect("Failed to build regex");

        let url_to_test = "/media/low/some/other/path/image.webp";
        let captures = regex.captures(url_to_test).expect("Failed to match URL");

        assert_eq!(captures.name("size").unwrap().as_str(), "low");
        assert_eq!(
            captures.name("path").unwrap().as_str(),
            "some/other/path/image"
        );
        assert_eq!(captures.name("ext").unwrap().as_str(), "webp");
    }

    #[test]
    fn test_build_url_regex_valid_pattern_optional_part() {
        let url = "/media/[optional/]{size}/{path}.{ext}";
        let regex = Config::build_url_regex(url).expect("Failed to build regex");

        let url_to_test = "/media/optional/low/some/other/path/image.webp";
        let captures = regex.captures(url_to_test).expect("Failed to match URL");

        assert_eq!(captures.name("size").unwrap().as_str(), "low");
        assert_eq!(
            captures.name("path").unwrap().as_str(),
            "some/other/path/image"
        );
        assert_eq!(captures.name("ext").unwrap().as_str(), "webp");

        let url_to_test = "/media/low/some/other/path/image.webp";
        let captures = regex.captures(url_to_test).expect("Failed to match URL");

        assert_eq!(captures.name("size").unwrap().as_str(), "low");
        assert_eq!(
            captures.name("path").unwrap().as_str(),
            "some/other/path/image"
        );
        assert_eq!(captures.name("ext").unwrap().as_str(), "webp");
    }

    #[test]
    fn test_build_url_regex_invalid_pattern_missing_path() {
        let url = "/media/{size}//[.{ext}]";
        let result = Config::build_url_regex(url);

        assert!(result.is_err());
    }

    #[test]
    fn test_parse_cache_control_defaults() {
        let config_content = String::from(
            r#"
        (
            extensions: [AVIF],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{size}/{path}.{ext}",
            cache_directory: "/build/cache",
            sizes: { "default": Size(width: 100, height: 100) },
        )
        "#,
        );
        let config = Config::parse_ron(config_content).expect("config should parse");
        assert_eq!(&*config.cache_control_value, DEFAULT_CACHE_CONTROL);
        assert_eq!(&*config.cache_control_fallback, FALLBACK_CACHE_CONTROL);
    }

    #[test]
    fn test_parse_static_routes_with_defaults() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let config_content = format!(
            r#"
        (
            extensions: [AVIF],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{{size}}/{{path}}.{{ext}}",
            cache_directory: "/build/cache",
            sizes: {{ "default": Size(width: 100, height: 100) }},
            static: [
                StaticRoute(
                    url: "/assets/{{path}}",
                    root: "{root}",
                ),
            ],
        )
        "#
        );
        let config = Config::parse_ron(config_content).expect("parse");
        assert_eq!(config.statics.len(), 1);
        assert_eq!(config.statics[0].url, "/assets/{path}");
        assert!(config.statics[0].url_regex.is_some());
        assert!(config.statics[0].root_canon.is_some());
        assert!(config.statics[0].optimization.html);
        assert!(config.statics[0].optimization.css);
        assert!(
            !config.statics[0].optimization.js,
            "js should default OFF (alpha)"
        );
        assert!(config.statics[0].optimization.json);
        //2 MiB default cap: small file → optimization allowed
        assert!(config.statics[0].allows_optimization_at_size(1_000_000));
        assert!(!config.statics[0].allows_optimization_at_size(3 * 1024 * 1024));
        //per-route Cache-Control falls back to global default
        assert_eq!(
            &*config.statics[0].cache_control_value,
            DEFAULT_CACHE_CONTROL
        );
    }

    #[test]
    fn test_parse_static_route_optimization_overrides() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let config_content = format!(
            r#"
        (
            extensions: [AVIF],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{{size}}/{{path}}.{{ext}}",
            cache_directory: "/build/cache",
            sizes: {{ "default": Size(width: 100, height: 100) }},
            static: [
                StaticRoute(
                    url: "/assets/{{path}}",
                    root: "{root}",
                    optimization: Optimization(js: true, css: false),
                    optimize_max_bytes: 1024,
                ),
            ],
        )
        "#
        );
        let config = Config::parse_ron(config_content).expect("parse");
        assert!(config.statics[0].optimization.js);
        assert!(!config.statics[0].optimization.css);
        assert!(
            config.statics[0].optimization.html,
            "html unchanged from default"
        );
        assert!(config.statics[0].allows_optimization_at_size(1024));
        assert!(!config.statics[0].allows_optimization_at_size(1025));
    }

    #[test]
    fn test_optimize_max_bytes_zero_disables_cap() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let config_content = format!(
            r#"
        (
            extensions: [AVIF],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{{size}}/{{path}}.{{ext}}",
            cache_directory: "/build/cache",
            sizes: {{ "default": Size(width: 100, height: 100) }},
            static: [
                StaticRoute(
                    url: "/assets/{{path}}",
                    root: "{root}",
                    optimize_max_bytes: 0,
                ),
            ],
        )
        "#
        );
        let config = Config::parse_ron(config_content).expect("parse");
        //Some(0) sentinel means "no cap" — files of any size are optimized
        assert!(config.statics[0].allows_optimization_at_size(0));
        assert!(config.statics[0].allows_optimization_at_size(usize::MAX));
    }

    #[test]
    fn test_parse_static_route_per_route_cache_control() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let config_content = format!(
            r#"
        (
            extensions: [AVIF],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{{size}}/{{path}}.{{ext}}",
            cache_directory: "/build/cache",
            sizes: {{ "default": Size(width: 100, height: 100) }},
            static: [
                StaticRoute(
                    url: "/assets/{{path}}",
                    root: "{root}",
                    cache_control: "public, max-age=60, immutable",
                ),
            ],
        )
        "#
        );
        let config = Config::parse_ron(config_content).expect("parse");
        assert_eq!(
            &*config.statics[0].cache_control_value,
            "public, max-age=60, immutable",
        );
    }

    #[test]
    fn test_parse_static_route_inherits_global_cache_control() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let config_content = format!(
            r#"
        (
            extensions: [AVIF],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{{size}}/{{path}}.{{ext}}",
            cache_directory: "/build/cache",
            sizes: {{ "default": Size(width: 100, height: 100) }},
            cache_control: "public, max-age=3600",
            static: [
                StaticRoute(
                    url: "/assets/{{path}}",
                    root: "{root}",
                ),
            ],
        )
        "#
        );
        let config = Config::parse_ron(config_content).expect("parse");
        //route without its own cache_control inherits the global one
        assert_eq!(&*config.cache_control_value, "public, max-age=3600");
        assert_eq!(
            &*config.statics[0].cache_control_value,
            "public, max-age=3600"
        );
    }

    #[test]
    fn test_parse_static_route_unreadable_root_fails() {
        let config_content = String::from(
            r#"
        (
            extensions: [AVIF],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{size}/{path}.{ext}",
            cache_directory: "/build/cache",
            sizes: { "default": Size(width: 100, height: 100) },
            static: [
                StaticRoute(
                    url: "/assets/{path}",
                    root: "/this/path/does/not/exist/anywhere",
                ),
            ],
        )
        "#,
        );
        let result = Config::parse_ron(config_content);
        assert!(
            result.is_err(),
            "parse should fail when static root is unreadable"
        );
    }

    #[test]
    fn test_parse_static_url_requires_path_capture() {
        let result = Config::build_static_url_regex("/assets/foo");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_cache_control_override() {
        //user-supplied raw header replaces the default; image fallback stays
        //hardcoded "no-cache" regardless
        let config_content = String::from(
            r#"
        (
            extensions: [AVIF],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{size}/{path}.{ext}",
            cache_directory: "/build/cache",
            sizes: { "default": Size(width: 100, height: 100) },
            cache_control: "private, max-age=300",
        )
        "#,
        );
        let config = Config::parse_ron(config_content).expect("config should parse");
        assert_eq!(&*config.cache_control_value, "private, max-age=300");
        assert_eq!(&*config.cache_control_fallback, FALLBACK_CACHE_CONTROL);
    }

    #[test]
    fn test_parse_valid_config_json() {
        let config_content = String::from(
            r#"
        {
            "extensions": ["AVIF", "WEBP", "JPEG"],
            "default_format": "JPEG",
            "roots": ["/build/media"],
            "url": "/media/{size}/{path}[.{ext}]",
            "cache_directory": "/build/cache",
            "sizes": {
                "low": {"width": 300, "height": 300},
                "medium": {"width": 600, "height": 600},
                "high": {"width": 1200, "height": 1200},
                "product": {"width": 546, "height": 302, "pattern": "^products/", "pre_optimize": true}
            },
            "logger": {
                "path": "/build/debug/impress.log",
                "level": "WARN"
            }
        }
        "#,
        );

        let config = Config::parse_json(config_content).expect("Failed to parse valid JSON config");

        assert_eq!(
            config.extensions,
            vec![Extension::AVIF, Extension::WEBP, Extension::JPEG]
        );
        assert_eq!(config.default_format, Extension::JPEG);
        assert_eq!(config.roots, vec!["/build/media".to_string()]);
        assert_eq!(config.url, "/media/{size}/{path}[.{ext}]");
        assert_eq!(config.cache_directory, "/build/cache".to_string());
        assert!(config.sizes.contains_key("low"));
        assert!(config.sizes.contains_key("product"));
        assert!(config.sizes["product"].pattern_regex.is_some());
        assert!(config.logger.is_some());
        assert!(config.url_regex.is_some());
    }

    #[test]
    fn test_parse_default_quality_values_json() {
        let config_content = String::from(
            r#"
        {
            "extensions": ["AVIF", "WEBP", "JPEG"],
            "default_format": "JPEG",
            "qualities": {"WEBP": 80, "AVIF": 50},
            "roots": ["/build/media"],
            "url": "/media/{size}/{path}.{ext}",
            "cache_directory": "/build/cache",
            "sizes": {
                "low": {"width": 300, "height": 300, "qualities": {"JPEG": 100}},
                "medium": {"width": 600, "height": 600}
            }
        }
        "#,
        );

        let config = Config::parse_json(config_content).expect("Failed to parse valid JSON config");

        //per-size override wins
        assert_eq!(config.sizes["low"].quality[Extension::JPEG as usize], 100.0);
        //global override wins over default
        assert_eq!(config.sizes["low"].quality[Extension::WEBP as usize], 80.0);
        assert_eq!(
            config.sizes["medium"].quality[Extension::AVIF as usize],
            50.0
        );
        //fallback to extension default when neither overrides
        assert_eq!(
            config.sizes["medium"].quality[Extension::JPEG as usize],
            Extension::JPEG.default_quality()
        );
    }

    #[test]
    fn test_parse_static_routes_with_defaults_json() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_string_lossy().to_string();
        let config_content = format!(
            r#"
        {{
            "extensions": ["AVIF"],
            "default_format": "JPEG",
            "roots": ["/build/media"],
            "url": "/media/{{size}}/{{path}}.{{ext}}",
            "cache_directory": "/build/cache",
            "sizes": {{ "default": {{"width": 100, "height": 100}} }},
            "static": [
                {{
                    "url": "/assets/{{path}}",
                    "root": "{root}"
                }}
            ]
        }}
        "#
        );
        let config = Config::parse_json(config_content).expect("parse");
        assert_eq!(config.statics.len(), 1);
        assert_eq!(config.statics[0].url, "/assets/{path}");
        assert!(config.statics[0].url_regex.is_some());
        assert!(config.statics[0].root_canon.is_some());
        assert!(config.statics[0].optimization.html);
        assert!(
            !config.statics[0].optimization.js,
            "js should default OFF (alpha)"
        );
        assert_eq!(
            &*config.statics[0].cache_control_value,
            DEFAULT_CACHE_CONTROL
        );
    }

    #[test]
    fn test_parse_invalid_json_fails() {
        let config_content = String::from("{ this is not valid json");
        let result = Config::parse_json(config_content);
        assert!(
            result.is_err(),
            "malformed JSON should produce a parse error"
        );
    }
}
