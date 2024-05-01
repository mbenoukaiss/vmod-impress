use std::collections::{HashMap, HashSet};
use std::fs::{File, Metadata};
use std::io::Read;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use image::{DynamicImage, ImageFormat};
use image::imageops::FilterType;
use walkdir::WalkDir;
use webp::{Encoder, WebPConfig};
use crate::config::Config;
use crate::error::Error;
use crate::utils;

pub struct Cache {
    config: Config,
    data: Arc<RwLock<HashMap<String, CacheImage>>>,
}

impl Cache {
    pub fn new(config: &Config) -> Self {
        let data  = Arc::new(RwLock::new(Cache::load_images(&config.root)));

        Cache {
            config: config.clone(),
            data,
        }
    }

    fn load_images(root: &str) -> HashMap<String, CacheImage> {
        let mut output = HashMap::new();

        let supported_extensions = ImageFormat::all()
            .flat_map(ImageFormat::extensions_str)
            .map(Deref::deref)
            .collect::<HashSet<&str>>();

        let files = WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| !e.file_type().is_dir());

        for file in files {
            let filename = String::from(file.file_name().to_string_lossy());

            if let (Some(stem), Some(extension)) = utils::decompose_filename(&filename) {
                if !supported_extensions.contains(extension) {
                    continue;
                }

                output.insert(stem.to_owned(), CacheImage::new(filename));
            }
        }

        output
    }

    pub fn get(&self, image: &str, size: &str, ext: &str) -> Result<Option<(Vec<u8>, Option<Metadata>)>, Error> {
        let lock = self.data.read()?;
        let Some(cache) = lock.get(image) else {
            return Ok(None);
        };

        let mut path = PathBuf::from(&self.config.root);

        let data = if let Some(file) = cache.get(size, ext) {
            path.push(file);

            let mut file = File::open(path)?;
            let metadata = file.metadata()?;
            let mut buffer = vec![0; metadata.len() as usize];
            file.read(&mut buffer).expect("buffer overflow");

            (buffer, Some(metadata))
        } else {
            path.push(&cache.base_image);

            let image = read_image(path)?;
            if ext != "webp" {
                panic!("Unsupported format");
            }

            let Some(format) = self.config.sizes.get(size) else {
                return Error::err("Size not found in config");
            };

            let image = resize_image(image, format.width, format.height);
            let webp = convert_webp(&image, format.quality.unwrap_or(self.config.default_quality));

            (webp, None)
        };

        Ok(Some(data))
    }

    pub fn add_image(&self, base_image: String, item: CacheImage) {
        self.data.write().unwrap().insert(base_image, item);
    }
}

pub struct CacheImage {
    pub base_image: String,
    pub optimized: HashMap<(String, String), String>,
}

impl CacheImage {
    pub fn new(base_image: String) -> Self {
        CacheImage {
            base_image,
            optimized: HashMap::new(),
        }
    }

    pub fn add(&mut self, size: String, ext: String, path: String) {
        self.optimized.insert((size, ext), path);
    }

    pub fn get(&self, size: &str, ext: &str) -> Option<&String> {
        self.optimized.get(&(size.to_string(), ext.to_string()))
    }
}

fn read_image(path: PathBuf) -> Result<DynamicImage, Error> {
    let image = image::open(path)?;
    if matches!(&image, DynamicImage::ImageRgb8(_)) || matches!(&image, DynamicImage::ImageRgba8(_)) {
        Ok(image)
    } else {
        Ok(DynamicImage::ImageRgba8(image.to_rgba8()))
    }
}

fn resize_image(image: DynamicImage, width: u32, height: u32) -> DynamicImage {
    image.resize(width, height, FilterType::Triangle)
}

fn convert_webp(image: &DynamicImage, quality: f32) -> Vec<u8> {
    //TODO: check libwebp configs to find the best compromise
    let mut config = WebPConfig::new().unwrap();
    config.quality = quality;
    config.lossless = 0;
    config.alpha_quality = 0;
    config.alpha_compression = 0;
    config.alpha_filtering = 0;
    config.autofilter = 0;
    config.filter_sharpness = 0;
    config.filter_strength = 0;
    config.filter_type = 0;
    config.use_sharp_yuv = 0;
    config.method = 3;

    Encoder::from_image(image)
        .expect("Unsupported format")
        .encode_advanced(&config)
        .unwrap()
        .to_vec()
}