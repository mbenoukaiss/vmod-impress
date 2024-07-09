use std::collections::HashMap;
use std::fs;
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

    #[serde(skip_deserializing)]
    pub url_regex: Option<Regex>,

    #[serde(rename = "qualities")]
    pub quality_serialized: Option<HashMap<Extension, f32>>,
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

#[derive(Deserialize, Eq, PartialEq, Hash, Copy, Clone, Debug)]
#[repr(u8)]
pub enum Extension {
    JPEG,
    WEBP,
    AVIF,
}

impl Extension {
    pub fn values() -> [Extension; 3] {
        return [
            Extension::JPEG,
            Extension::WEBP,
            Extension::AVIF,
        ];
    }

    pub fn to_media_type(&self) -> MediaType {
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
            url_regex: None,
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
}