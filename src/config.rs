use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use image::ImageFormat;
use log::LevelFilter;
use mediatype::MediaType;
use mediatype::names::{AVIF, IMAGE, JPEG, WEBP};
use regex::Regex;
use ron::extensions::Extensions;
use ron::Options;
use serde::Deserialize;
use crate::error::Error;
use crate::images::OptimizationConfig;

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
    pub cache_control: Option<CacheControl>,

    #[serde(skip_deserializing)]
    pub url_regex: Option<Regex>,
    //precomputed once at parse time so the hot path is allocation-free
    #[serde(skip_deserializing)]
    pub cache_control_optimized: String,
    #[serde(skip_deserializing)]
    pub cache_control_fallback: String,

    #[serde(rename = "qualities")]
    pub quality_serialized: Option<HashMap<Extension, f32>>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct CacheControl {
    pub optimized_max_age_seconds: Option<u64>,
    pub optimized_stale_while_revalidate_seconds: Option<u64>,
    pub fallback_max_age_seconds: Option<u64>,
    pub fallback_stale_while_revalidate_seconds: Option<u64>,
}

impl CacheControl {
    pub fn optimized_header(&self) -> String {
        let max_age = self.optimized_max_age_seconds.unwrap_or(86_400);
        let swr = self.optimized_stale_while_revalidate_seconds.unwrap_or(604_800);
        format!("public, max-age={}, stale-while-revalidate={}", max_age, swr)
    }

    pub fn fallback_header(&self) -> String {
        let max_age = self.fallback_max_age_seconds.unwrap_or(60);
        let swr = self.fallback_stale_while_revalidate_seconds.unwrap_or(3_600);
        format!("public, max-age={}, stale-while-revalidate={}", max_age, swr)
    }
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
        [
            Extension::JPEG,
            Extension::WEBP,
            Extension::AVIF,
        ]
    }

    #[allow(clippy::wrong_self_convention)] //changing &self → self propagates lifetimes through callers in undesirable ways
    pub fn to_media_type(&self) -> MediaType<'_> {
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
    pub fn open(path: Option<&str>) -> Result<Config, Error> {
        let path = path.unwrap_or("impress.ron").to_owned();

        if let Ok(config) = fs::read_to_string(&path) {
            Config::parse(config)
        } else {
            Error::err(format!("Unable to read config file {}", path))
        }
    }

    fn parse(config: String) -> Result<Config, Error> {
        let mut config = Options::default()
            .with_default_extension(Extensions::IMPLICIT_SOME)
            .from_str::<Config>(&config)?;

        config.url_regex = Some(Self::build_url_regex(&config.url)?);

        for size in &mut config.sizes.values_mut() {
            for extension in Extension::values() {
                let size_quality = size.quality_serialized.as_ref().and_then(|q| q.get(&extension));
                let config_quality = config.quality_serialized.as_ref().and_then(|q| q.get(&extension));

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

        config.quality_serialized = None;

        let cc = config.cache_control.clone().unwrap_or_default();
        config.cache_control_optimized = cc.optimized_header();
        config.cache_control_fallback = cc.fallback_header();

        Ok(config)
    }

    fn build_url_regex(url: &str) -> Result<Regex, Error> {
        if !url.contains("{size}") || !url.contains("{path}") {
            return Error::err("Arguments {size} and {path} are required in URL pattern");
        }

        let clean_url = format!(r"^{}$", regex::escape(url))
            .replace(r"\{size\}", r"(?<size>\w+)")
            .replace(r"\{path\}", r"(?<path>.+?)")
            .replace(r"\{ext\}", r"(?<ext>[a-zA-Z0-9]+)")
            .replace(r"\[", "(")
            .replace(r"\]", ")?");

        if clean_url.chars().filter(|c| *c == '(').count() != clean_url.chars().filter(|c| *c == ')').count() {
            return Error::err("Invalid URL pattern in config file");
        }

        Ok(Regex::new(&clean_url)?)
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            extensions: vec![Extension::AVIF],
            default_format: Extension::JPEG,
            roots: vec![
                String::from("/dev/null"),
            ],
            url: String::from("/media"),
            cache_directory: String::from("/tmp/impress"),
            pre_optimizer_threads: None,
            sizes: HashMap::from([
                (String::from("default"), Size {
                    width: 500,
                    height: 500,
                    quality: [0.0; 3],
                    pattern: None,
                    pre_optimize: None,
                    pattern_regex: None,
                    quality_serialized: None,
                }),
            ]),
            logger: None,
            cache_control: None,
            url_regex: None,
            cache_control_optimized: String::new(),
            cache_control_fallback: String::new(),
            quality_serialized: None,
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

    #[test]
    fn test_parse_valid_config() {
        let config_content = String::from(r#"
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
        "#);

        let config = Config::parse(config_content).expect("Failed to parse valid config");

        assert_eq!(config.extensions, vec![Extension::AVIF, Extension::WEBP, Extension::JPEG]);
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
        let config_content = String::from(r#"
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
        "#);

        let result = Config::parse(config_content);
        assert!(result.is_err());
        if let Err(err) = result {
            assert_eq!(err.to_string(), "Invalid URL pattern in config file".to_string());
        }
    }

    #[test]
    fn test_parse_default_quality_values() {
        let config_content = String::from(r#"
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
        "#);

        let config = Config::parse(config_content).expect("Failed to parse valid config");

        assert_eq!(config.sizes["low"].quality[Extension::JPEG as usize], Extension::JPEG.default_quality());
        assert_eq!(config.sizes["medium"].quality[Extension::WEBP as usize], Extension::WEBP.default_quality());
        assert_eq!(config.sizes["high"].quality[Extension::AVIF as usize], Extension::AVIF.default_quality());
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
        assert_eq!(captures.name("path").unwrap().as_str(), "another/path/image");
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
        assert_eq!(captures.name("path").unwrap().as_str(), "some/other/path/image");
        assert_eq!(captures.name("ext").unwrap().as_str(), "webp");
    }

    #[test]
    fn test_build_url_regex_valid_pattern_optional_part() {
        let url = "/media/[optional/]{size}/{path}.{ext}";
        let regex = Config::build_url_regex(url).expect("Failed to build regex");

        let url_to_test = "/media/optional/low/some/other/path/image.webp";
        let captures = regex.captures(url_to_test).expect("Failed to match URL");

        assert_eq!(captures.name("size").unwrap().as_str(), "low");
        assert_eq!(captures.name("path").unwrap().as_str(), "some/other/path/image");
        assert_eq!(captures.name("ext").unwrap().as_str(), "webp");

        let url_to_test = "/media/low/some/other/path/image.webp";
        let captures = regex.captures(url_to_test).expect("Failed to match URL");

        assert_eq!(captures.name("size").unwrap().as_str(), "low");
        assert_eq!(captures.name("path").unwrap().as_str(), "some/other/path/image");
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
        let config_content = String::from(r#"
        (
            extensions: [AVIF],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{size}/{path}.{ext}",
            cache_directory: "/build/cache",
            sizes: { "default": Size(width: 100, height: 100) },
        )
        "#);
        let config = Config::parse(config_content).expect("config should parse");
        assert_eq!(config.cache_control_optimized, "public, max-age=86400, stale-while-revalidate=604800");
        assert_eq!(config.cache_control_fallback, "public, max-age=60, stale-while-revalidate=3600");
    }

    #[test]
    fn test_parse_cache_control_overrides() {
        let config_content = String::from(r#"
        (
            extensions: [AVIF],
            default_format: JPEG,
            roots: ["/build/media"],
            url: "/media/{size}/{path}.{ext}",
            cache_directory: "/build/cache",
            sizes: { "default": Size(width: 100, height: 100) },
            cache_control: CacheControl(
                optimized_max_age_seconds: 3600,
                optimized_stale_while_revalidate_seconds: 86400,
                fallback_max_age_seconds: 30,
                fallback_stale_while_revalidate_seconds: 600,
            ),
        )
        "#);
        let config = Config::parse(config_content).expect("config should parse");
        assert_eq!(config.cache_control_optimized, "public, max-age=3600, stale-while-revalidate=86400");
        assert_eq!(config.cache_control_fallback, "public, max-age=30, stale-while-revalidate=600");
    }

}