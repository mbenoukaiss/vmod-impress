use std::collections::HashMap;
use std::fs;
use image::ImageFormat;
use log::LevelFilter;
use regex::Regex;
use ron::extensions::Extensions;
use ron::Options;
use serde::{Deserialize, Serialize};
use crate::error::Error;
use crate::images::OptimizationConfig;

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Config {
    pub extensions: Vec<Extension>,
    pub default_quality: f32,
    pub default_format: Extension,
    pub root: String,
    pub url: String,
    pub cache_directory: String,
    pub pre_optimizer_threads: Option<usize>,
    pub sizes: HashMap<String, Size>,
    pub logger: Option<Logger>,

    #[serde(skip_deserializing, skip_serializing)]
    pub url_regex: Option<Regex>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Size {
    pub width: u32,
    pub height: u32,
    pub quality: Option<f32>,
    pub pattern: Option<String>,
    pub pre_optimize: Option<bool>,

    #[serde(skip_deserializing, skip_serializing)]
    pub pattern_regex: Option<Regex>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Logger {
    pub path: String,
    pub level: Option<LevelFilter>,
}

#[derive(Deserialize, Serialize, Eq, PartialEq, Hash, Copy, Clone, Debug)]
pub enum Extension {
    JPEG,
    WEBP,
    AVIF,
}

impl Extension {
    pub fn image_format(&self) -> ImageFormat {
        match self {
            Extension::JPEG => ImageFormat::Jpeg,
            Extension::WEBP => ImageFormat::WebP,
            Extension::AVIF => ImageFormat::Avif,
        }
    }

    pub fn mime(&self) -> &'static str {
        self.image_format().to_mime_type()
    }

    pub fn extensions(&self) -> &'static [&'static str] {
        self.image_format().extensions_str()
    }
}

impl Config {
    pub fn parse(path: Option<&str>) -> Result<Config, Error> {
        let path = path.unwrap_or("impress.ron").to_owned();

        if let Ok(config) = fs::read_to_string(&path) {
            let mut config = Options::default()
                .with_default_extension(Extensions::IMPLICIT_SOME)
                .from_str::<Config>(&config)?;

            config.url_regex = Some(Regex::new(&format!(r"^{}$", regex::escape(&config.url))
                .replace(r"\{size\}", r"(?<size>\w+)")
                .replace(r"\{path\}", r"(?<path>.+)")
                .replace(r"\{ext\}", r"(?<ext>\w+)"))?);

            for size in config.sizes.values_mut() {
                if let Some(pattern) = &size.pattern {
                    size.pattern_regex = Some(Regex::new(pattern)?)
                }
            }

            Ok(config)
        } else {
            Error::err(format!("Unable to read config file {}", path))
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            extensions: vec![Extension::AVIF],
            default_quality: 415.0,
            default_format: Extension::JPEG,
            root: String::from("/dev/null"),
            url: String::from("/media"),
            cache_directory: String::from("/tmp/impress"),
            pre_optimizer_threads: None,
            sizes: HashMap::from([
                (String::from("default"), Size {
                    width: 500,
                    height: 500,
                    quality: None,
                    pattern: None,
                    pre_optimize: None,
                    pattern_regex: None,
                }),
            ]),
            logger: None,
            url_regex: None,
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
    pub fn new(config: &Config, size: &str, format: Extension, prefer_quality: bool) -> OptimizationConfig {
        let quality = config.sizes.get(size).unwrap().quality.unwrap_or(config.default_quality);

        match format {
            Extension::WEBP => OptimizationConfig::Webp {
                quality,
                prefer_quality,
            },
            Extension::AVIF => OptimizationConfig::Avif {
                quality,
                prefer_quality,
            },
            _ => panic!("Unsupported extension"),
        }
    }
}