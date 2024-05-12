use std::collections::HashMap;
use std::fs;
use regex::Regex;
use ron::extensions::Extensions;
use ron::Options;
use serde::{Deserialize, Serialize};
use crate::error::Error;
use crate::images;
use crate::images::OptimizationConfig;

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Config {
    pub extensions: Vec<String>,
    pub default_quality: f32,
    pub default_format: String,
    pub root: String,
    pub url: String,
    pub cache_directory: String,
    pub sizes: HashMap<String, Size>,
    pub log_path: Option<String>,

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

impl Config {
    pub fn parse(path: Option<&str>) -> Result<Config, Error> {
        let path = path.unwrap_or("shrink.ron").to_owned();

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

            let unsupported_extensions = config.extensions.iter()
                .map(|extension| extension.as_str())
                .filter(|extension| !images::supports(extension))
                .collect::<Vec<&str>>();

            if !unsupported_extensions.is_empty() {
                return Error::err(format!("Unsupported extensions: {:?}", unsupported_extensions));
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
            extensions: vec![String::from("webp")],
            default_quality: 415.0,
            default_format: String::from("jpeg"),
            root: String::from("/dev/null"),
            url: String::from("/media"),
            cache_directory: String::from("/tmp/shrink"),
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
            log_path: None,
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
    pub fn new(config: &Config, size: &str, format: &str) -> OptimizationConfig {
        match format {
            "webp" => OptimizationConfig::Webp {
                quality: config.sizes.get(size).unwrap().quality.unwrap_or(config.default_quality),
            },
            _ => panic!("Unsupported extension"),
        }
    }
}