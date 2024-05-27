use std::collections::HashMap;
use std::fs;
use image::ImageFormat;
use log::LevelFilter;
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
    pub root: String,
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
        } else {
            Error::err(format!("Unable to read config file {}", path))
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            extensions: vec![Extension::AVIF],
            default_format: Extension::JPEG,
            root: String::from("/dev/null"),
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