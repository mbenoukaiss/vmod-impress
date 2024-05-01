use std::collections::HashMap;
use std::fs;
use regex::Regex;
use serde::{Deserialize, Serialize};
use crate::error::Error;

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Config {
    pub formats: Vec<String>,
    pub default_quality: f32,
    pub root: String,
    pub url: String,
    pub cache_directory: String,
    pub sizes: HashMap<String, Format>,

    #[serde(skip_deserializing, skip_serializing)]
    pub url_regex: Option<Regex>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Format {
    pub width: u32,
    pub height: u32,
    pub quality: Option<f32>,
}

impl Config {
    pub fn parse(path: Option<&str>) -> Result<Config, Error> {
        let path = path.unwrap_or("shrink.ron").to_owned();


        if let Ok(config) = fs::read_to_string(&path) {
            let mut config = ron::from_str::<Config>(&config)?;

            config.url_regex = Some(Regex::new(&format!(r"^{}$", regex::escape(&config.url))
                .replace(r"\{size\}", r"(?<size>\w+)")
                .replace(r"\{path\}", r"(?<path>\w+)")
                .replace(r"\{ext\}", r"(?<ext>\w+)"))?);

            Ok(config)
        } else {
            Error::err(format!("Unable to read config file {}", path))
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            formats: vec![String::from("webp"), String::from("jpeg")],
            default_quality: 415.0,
            root: String::from("/dev/null"),
            url: String::from("/media"),
            cache_directory: String::from("/tmp/shrink"),
            sizes: HashMap::from([
                (String::from("default"), Format {
                    width: 500,
                    height: 500,
                    quality: None,
                }),
            ]),
            url_regex: None,
        }
    }
}